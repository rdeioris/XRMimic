//! Vulkan side: XR_KHR_vulkan_enable2. The runtime creates the app's
//! VkInstance and VkDevice and hands out swapchain images made on that
//! device. On Linux this is the whole backend (it can read a submitted frame
//! back for tests, `XRMIMIC_CAPTURE`); on Windows crate::vkd3d takes the
//! frames on to the D3D11 window.
#![cfg_attr(windows, allow(dead_code))]

use crate::shared::Shared;
use crate::xlog;
use ash::vk;
use ash::vk::Handle as _;
use openxr_sys as xr;
use std::ffi::{c_void, CStr};
use std::sync::{Arc, Mutex};

/// The extensions this backend adds (Linux; on Windows d3d.rs lists them).
pub const EXTENSIONS: &[(&str, u32)] = &[("XR_KHR_vulkan_enable2", 2), ("XR_KHR_convert_timespec_time", 1)];

/// Formats offered to apps, preferred first (Chrome takes the first of
/// BGRA sRGB, BGRA, RGBA sRGB, RGBA that it finds).
#[cfg(not(windows))]
const COLOR_FORMATS: &[vk::Format] = &[
    vk::Format::B8G8R8A8_SRGB,
    vk::Format::B8G8R8A8_UNORM,
    vk::Format::R8G8B8A8_SRGB,
    vk::Format::R8G8B8A8_UNORM,
];
#[cfg(not(windows))]
const DEPTH_FORMATS: &[vk::Format] = &[vk::Format::D24_UNORM_S8_UINT, vk::Format::D32_SFLOAT, vk::Format::D16_UNORM];
// Windows: the formats crate::vkd3d has proxies for.
#[cfg(windows)]
const COLOR_FORMATS: &[vk::Format] = &[
    vk::Format::B8G8R8A8_SRGB,
    vk::Format::B8G8R8A8_UNORM,
    vk::Format::R8G8B8A8_SRGB,
    vk::Format::R8G8B8A8_UNORM,
    vk::Format::R16G16B16A16_SFLOAT,
];
#[cfg(windows)]
const DEPTH_FORMATS: &[vk::Format] = &[vk::Format::D32_SFLOAT, vk::Format::D16_UNORM, vk::Format::D32_SFLOAT_S8_UINT];

fn has_stencil(f: vk::Format) -> bool {
    matches!(f, vk::Format::D24_UNORM_S8_UINT | vk::Format::D32_SFLOAT_S8_UINT)
}

pub fn swapchain_formats() -> Vec<i64> {
    COLOR_FORMATS.iter().chain(DEPTH_FORMATS.iter()).map(|f| f.as_raw() as i64).collect()
}

fn is_depth(f: vk::Format) -> bool {
    DEPTH_FORMATS.contains(&f)
}

/// What the runtime and the headless headset thread exchange.
#[cfg(not(windows))]
#[derive(Default)]
pub struct Slot {
    pub frames: u64,
}

/// The app's vkGetInstanceProcAddr and the instance created for it.
struct Globals {
    gipa: Option<vk::PFN_vkGetInstanceProcAddr>,
    instance: vk::Instance,
}

static GLOBALS: Mutex<Globals> = Mutex::new(Globals { gipa: None, instance: vk::Instance::null() });

fn entry(gipa: Option<vk::PFN_vkGetInstanceProcAddr>) -> Option<ash::Entry> {
    match gipa {
        Some(f) => Some(unsafe { ash::Entry::from_static_fn(ash::StaticFn { get_instance_proc_addr: f }) }),
        None => unsafe { ash::Entry::load().ok() },
    }
}

fn xr_gipa(f: Option<xr::platform::VkGetInstanceProcAddr>) -> Option<vk::PFN_vkGetInstanceProcAddr> {
    f.map(|f| unsafe { std::mem::transmute::<xr::platform::VkGetInstanceProcAddr, vk::PFN_vkGetInstanceProcAddr>(f) })
}

/// Entry points of this backend's extensions.
pub fn lookup(name: &str) -> Option<xr::pfn::VoidFunction> {
    use std::mem::transmute as t;
    unsafe {
        Some(match name {
            "xrGetVulkanGraphicsRequirements2KHR" | "xrGetVulkanGraphicsRequirementsKHR" => {
                t(requirements as xr::pfn::GetVulkanGraphicsRequirements2KHR)
            }
            "xrCreateVulkanInstanceKHR" => t(create_instance as xr::pfn::CreateVulkanInstanceKHR),
            "xrGetVulkanGraphicsDevice2KHR" => t(graphics_device as xr::pfn::GetVulkanGraphicsDevice2KHR),
            "xrCreateVulkanDeviceKHR" => t(create_device as xr::pfn::CreateVulkanDeviceKHR),
            #[cfg(unix)]
            "xrConvertTimespecTimeToTimeKHR" => t(timespec_to_time as xr::pfn::ConvertTimespecTimeToTimeKHR),
            #[cfg(unix)]
            "xrConvertTimeToTimespecTimeKHR" => t(time_to_timespec as xr::pfn::ConvertTimeToTimespecTimeKHR),
            _ => return None,
        })
    }
}

// XR_KHR_convert_timespec_time: XrTime is CLOCK_MONOTONIC in nanoseconds.
#[cfg(unix)]
unsafe extern "system" fn timespec_to_time(
    _instance: xr::Instance,
    ts: *const libc::timespec,
    out: *mut xr::Time,
) -> xr::Result {
    crate::traced!("xrConvertTimespecTimeToTimeKHR", [_instance], || {
        let ts = &*ts;
        *out = xr::Time::from_nanos(ts.tv_sec as i64 * 1_000_000_000 + ts.tv_nsec as i64);
        xr::Result::SUCCESS
    })
}

#[cfg(unix)]
unsafe extern "system" fn time_to_timespec(
    _instance: xr::Instance,
    t: xr::Time,
    out: *mut libc::timespec,
) -> xr::Result {
    crate::traced!("xrConvertTimeToTimespecTimeKHR", [_instance, t], || {
        let ns = t.as_nanos();
        if ns <= 0 {
            return xr::Result::ERROR_TIME_INVALID;
        }
        (*out).tv_sec = (ns / 1_000_000_000) as _;
        (*out).tv_nsec = (ns % 1_000_000_000) as _;
        xr::Result::SUCCESS
    })
}

unsafe extern "system" fn requirements(
    instance: xr::Instance,
    system: xr::SystemId,
    req: *mut xr::GraphicsRequirementsVulkanKHR,
) -> xr::Result {
    crate::traced!("xrGetVulkanGraphicsRequirements2KHR", [instance, system, req], || {
        if let Some(e) = crate::api::check_system(instance, system) {
            return e;
        }
        (*req).min_api_version_supported = xr::Version::new(1, 1, 0);
        (*req).max_api_version_supported = xr::Version::new(1, 4, 0);
        xr::Result::SUCCESS
    })
}

unsafe extern "system" fn create_instance(
    instance: xr::Instance,
    info: *const xr::VulkanInstanceCreateInfoKHR,
    out: *mut xr::platform::VkInstance,
    vk_result: *mut xr::platform::VkResult,
) -> xr::Result {
    crate::traced!("xrCreateVulkanInstanceKHR", [instance, info, out, vk_result], || {
        if let Some(e) = crate::api::check_system(instance, (*info).system_id) {
            return e;
        }
        let info = &*info;
        let Some(gipa) = xr_gipa(info.pfn_get_instance_proc_addr) else {
            return xr::Result::ERROR_VALIDATION_FAILURE;
        };
        let Some(f) = gipa(vk::Instance::null(), c"vkCreateInstance".as_ptr()) else {
            return xr::Result::ERROR_RUNTIME_FAILURE;
        };
        let create: vk::PFN_vkCreateInstance = std::mem::transmute(f);
        let ci = &*(info.vulkan_create_info as *const vk::InstanceCreateInfo);
        let exts: Vec<String> = (0..ci.enabled_extension_count as usize)
            .map(|i| CStr::from_ptr(*ci.pp_enabled_extension_names.add(i)).to_string_lossy().into_owned())
            .collect();
        let api = if ci.p_application_info.is_null() { 0 } else { (*ci.p_application_info).api_version };
        let mut inst = vk::Instance::null();
        let r = create(ci, info.vulkan_allocator as *const vk::AllocationCallbacks, &mut inst);
        xlog!(
            "vkCreateInstance for the app: api {}.{}, extensions {exts:?} -> {r:?}",
            vk::api_version_major(api),
            vk::api_version_minor(api)
        );
        *vk_result = r.as_raw();
        *out = inst.as_raw() as *const c_void;
        let mut g = GLOBALS.lock().unwrap();
        g.gipa = Some(gipa);
        g.instance = inst;
        xr::Result::SUCCESS
    })
}

/// The GPU to run on: `XRMIMIC_VK_DEVICE` (part of its name) or the first
/// discrete one, never a CPU device if a GPU exists.
unsafe fn pick_physical_device(inst: &ash::Instance) -> Option<vk::PhysicalDevice> {
    let devices = inst.enumerate_physical_devices().ok()?;
    let want = std::env::var("XRMIMIC_VK_DEVICE").ok();
    let rank = |t: vk::PhysicalDeviceType| match t {
        vk::PhysicalDeviceType::DISCRETE_GPU => 0,
        vk::PhysicalDeviceType::INTEGRATED_GPU => 1,
        vk::PhysicalDeviceType::VIRTUAL_GPU => 2,
        vk::PhysicalDeviceType::CPU => 4,
        _ => 3,
    };
    // Windows: the adapter D3D11 apps are told to use, so the window and the
    // app share a GPU.
    #[cfg(windows)]
    if want.is_none() {
        if let Some(luid) = crate::d3d::default_adapter_luid() {
            for &d in &devices {
                let mut id = vk::PhysicalDeviceIDProperties::default();
                let mut p2 = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
                inst.get_physical_device_properties2(d, &mut p2);
                let l = id.device_luid;
                if id.device_luid_valid != 0
                    && u32::from_le_bytes(l[0..4].try_into().unwrap()) == luid.LowPart
                    && i32::from_le_bytes(l[4..8].try_into().unwrap()) == luid.HighPart
                {
                    let p = inst.get_physical_device_properties(d);
                    xlog!("Vulkan device for the app: {} (the D3D11 adapter)", CStr::from_ptr(p.device_name.as_ptr()).to_string_lossy());
                    return Some(d);
                }
            }
        }
    }
    let mut best: Option<(i32, vk::PhysicalDevice, String)> = None;
    for d in devices {
        let p = inst.get_physical_device_properties(d);
        let name = CStr::from_ptr(p.device_name.as_ptr()).to_string_lossy().into_owned();
        let r = if want.as_ref().is_some_and(|w| name.contains(w.as_str())) { -1 } else { rank(p.device_type) };
        if best.as_ref().is_none_or(|(br, _, _)| r < *br) {
            best = Some((r, d, name));
        }
    }
    let (_, d, name) = best?;
    xlog!("Vulkan device for the app: {name}");
    Some(d)
}

unsafe extern "system" fn graphics_device(
    instance: xr::Instance,
    info: *const xr::VulkanGraphicsDeviceGetInfoKHR,
    out: *mut xr::platform::VkPhysicalDevice,
) -> xr::Result {
    crate::traced!("xrGetVulkanGraphicsDevice2KHR", [instance, info, out], || {
        if let Some(e) = crate::api::check_system(instance, (*info).system_id) {
            return e;
        }
        let vk_instance = vk::Instance::from_raw((*info).vulkan_instance as u64);
        let gipa = GLOBALS.lock().unwrap().gipa;
        let Some(entry) = entry(gipa) else {
            return xr::Result::ERROR_RUNTIME_FAILURE;
        };
        let inst = ash::Instance::load(entry.static_fn(), vk_instance);
        match pick_physical_device(&inst) {
            Some(d) => {
                *out = d.as_raw() as *const c_void;
                xr::Result::SUCCESS
            }
            None => xr::Result::ERROR_RUNTIME_FAILURE,
        }
    })
}

unsafe extern "system" fn create_device(
    instance: xr::Instance,
    info: *const xr::VulkanDeviceCreateInfoKHR,
    out: *mut xr::platform::VkDevice,
    vk_result: *mut xr::platform::VkResult,
) -> xr::Result {
    crate::traced!("xrCreateVulkanDeviceKHR", [instance, info, out, vk_result], || {
        if let Some(e) = crate::api::check_system(instance, (*info).system_id) {
            return e;
        }
        let info = &*info;
        let (gipa, vk_instance) = {
            let g = GLOBALS.lock().unwrap();
            (xr_gipa(info.pfn_get_instance_proc_addr).or(g.gipa), g.instance)
        };
        let Some(gipa) = gipa else {
            return xr::Result::ERROR_VALIDATION_FAILURE;
        };
        let Some(f) = gipa(vk_instance, c"vkCreateDevice".as_ptr()) else {
            return xr::Result::ERROR_RUNTIME_FAILURE;
        };
        let create: vk::PFN_vkCreateDevice = std::mem::transmute(f);
        let ci = &*(info.vulkan_create_info as *const vk::DeviceCreateInfo);
        let exts: Vec<String> = (0..ci.enabled_extension_count as usize)
            .map(|i| CStr::from_ptr(*ci.pp_enabled_extension_names.add(i)).to_string_lossy().into_owned())
            .collect();
        // Windows: what sharing frames with the D3D11 window takes.
        #[cfg(windows)]
        let mut own = windows_device_needs(ci, &exts);
        #[cfg(windows)]
        let ci = &own.info();
        let mut dev = vk::Device::null();
        let r = create(
            vk::PhysicalDevice::from_raw(info.vulkan_physical_device as u64),
            ci,
            info.vulkan_allocator as *const vk::AllocationCallbacks,
            &mut dev,
        );
        xlog!("vkCreateDevice for the app: extensions {exts:?} -> {r:?}");
        *vk_result = r.as_raw();
        *out = dev.as_raw() as *const c_void;
        xr::Result::SUCCESS
    })
}

/// The app's device extensions plus the ones the D3D11 window needs, and
/// timeline semaphores turned on (in the app's own feature struct if it
/// chains one, else in one of ours put first).
#[cfg(windows)]
struct DeviceNeeds {
    base: vk::DeviceCreateInfo<'static>,
    names: Vec<std::ffi::CString>,
    ptrs: Vec<*const std::ffi::c_char>,
    timeline: Box<vk::PhysicalDeviceTimelineSemaphoreFeatures<'static>>,
    prepend: bool,
}

#[cfg(windows)]
impl DeviceNeeds {
    fn info(&mut self) -> vk::DeviceCreateInfo<'static> {
        self.ptrs = self.names.iter().map(|n| n.as_ptr()).collect();
        let mut ci = self.base;
        ci.enabled_extension_count = self.ptrs.len() as u32;
        ci.pp_enabled_extension_names = self.ptrs.as_ptr();
        if self.prepend {
            self.timeline.p_next = self.base.p_next as *mut c_void;
            ci.p_next = &*self.timeline as *const _ as *const c_void;
        }
        ci
    }
}

#[cfg(windows)]
unsafe fn windows_device_needs(ci: &vk::DeviceCreateInfo, exts: &[String]) -> DeviceNeeds {
    let mut names: Vec<std::ffi::CString> = exts.iter().map(|e| std::ffi::CString::new(e.as_str()).unwrap()).collect();
    for want in ["VK_KHR_external_memory_win32", "VK_KHR_external_semaphore_win32", "VK_KHR_timeline_semaphore"] {
        if !exts.iter().any(|e| e == want) {
            names.push(std::ffi::CString::new(want).unwrap());
        }
    }
    // Timeline semaphores: set in the app's own struct where there is one.
    let mut prepend = true;
    let mut p = ci.p_next as *mut vk::BaseOutStructure;
    while !p.is_null() {
        match (*p).s_type {
            vk::StructureType::PHYSICAL_DEVICE_VULKAN_1_2_FEATURES => {
                (*(p as *mut vk::PhysicalDeviceVulkan12Features)).timeline_semaphore = vk::TRUE;
                prepend = false;
            }
            vk::StructureType::PHYSICAL_DEVICE_TIMELINE_SEMAPHORE_FEATURES => {
                (*(p as *mut vk::PhysicalDeviceTimelineSemaphoreFeatures)).timeline_semaphore = vk::TRUE;
                prepend = false;
            }
            _ => {}
        }
        p = (*p).p_next;
    }
    DeviceNeeds {
        base: std::mem::transmute::<vk::DeviceCreateInfo<'_>, vk::DeviceCreateInfo<'static>>(*ci),
        names,
        ptrs: Vec::new(),
        timeline: Box::new(vk::PhysicalDeviceTimelineSemaphoreFeatures::default().timeline_semaphore(true)),
        prepend,
    }
}

pub struct SessionGfx {
    pub(crate) device: ash::Device,
    pub(crate) queue: vk::Queue,
    pub(crate) pool: vk::CommandPool,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) instance: ash::Instance,
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) physical: vk::PhysicalDevice,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    mem_props: vk::PhysicalDeviceMemoryProperties,
    pub shared: Arc<Shared>,
    capture: Option<(std::path::PathBuf, u64)>,
    /// The control API's Depth view (`/v1/views`): window pictures show depth.
    #[cfg_attr(windows, allow(dead_code))]
    depth_view: bool,
}

impl Drop for SessionGfx {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.queue_wait_idle(self.queue);
            self.device.destroy_fence(self.fence, None);
            self.device.destroy_command_pool(self.pool, None);
        }
    }
}

#[allow(dead_code)] // mip/sample counts kept for parity with the D3D11 backend
pub struct SwapchainData {
    pub session: u64,
    device: ash::Device,
    pub images: Vec<vk::Image>,
    memories: Vec<vk::DeviceMemory>,
    pub width: u32,
    pub height: u32,
    /// 1, or 6 for a cube map (array layers face-major: layer = index * 6 + face).
    pub faces: u32,
    pub mip_count: u32,
    pub sample_count: u32,
    format: vk::Format,
    pub next_index: u32,
    pub acquired: std::collections::VecDeque<u32>,
    pub last_released: Option<u32>,
}

impl Drop for SwapchainData {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            for (i, m) in self.images.iter().zip(&self.memories) {
                self.device.destroy_image(*i, None);
                self.device.free_memory(*m, None);
            }
        }
    }
}

/// One eye's part of a projection layer.
pub struct EyeSource {
    image: vk::Image,
    layer: u32,
    pub rect: xr::Rect2Di,
    format: vk::Format,
    samples: u32,
}

pub unsafe fn write_images(sc: &SwapchainData, out: *mut xr::SwapchainImageBaseHeader) -> xr::Result {
    let out = out as *mut xr::SwapchainImageVulkanKHR;
    for (i, img) in sc.images.iter().enumerate() {
        let o = &mut *out.add(i);
        if o.ty != xr::StructureType::SWAPCHAIN_IMAGE_VULKAN_KHR {
            return xr::Result::ERROR_VALIDATION_FAILURE;
        }
        o.image = img.as_raw();
    }
    xr::Result::SUCCESS
}

pub fn eye_source(sc: &SwapchainData, idx: u32, sub: &xr::SwapchainSubImage) -> EyeSource {
    EyeSource {
        image: sc.images[idx as usize],
        layer: sub.image_array_index,
        rect: sub.image_rect,
        format: sc.format,
        samples: sc.sample_count,
    }
}

impl SessionGfx {
    /// The session's graphics binding from xrCreateSession's `next` chain.
    pub unsafe fn from_session_create(next: *const c_void, shared: Arc<Shared>) -> Result<SessionGfx, xr::Result> {
        let b = crate::api::find_in_chain(next, xr::StructureType::GRAPHICS_BINDING_VULKAN_KHR);
        if b.is_null() {
            xlog!("xrCreateSession: no Vulkan graphics binding");
            return Err(xr::Result::ERROR_GRAPHICS_DEVICE_INVALID);
        }
        let b = &*(b as *const xr::GraphicsBindingVulkanKHR);
        let gipa = GLOBALS.lock().unwrap().gipa;
        let entry = entry(gipa).ok_or(xr::Result::ERROR_RUNTIME_FAILURE)?;
        let inst = ash::Instance::load(entry.static_fn(), vk::Instance::from_raw(b.instance as u64));
        let device = ash::Device::load(inst.fp_v1_0(), vk::Device::from_raw(b.device as u64));
        let mem_props = inst.get_physical_device_memory_properties(vk::PhysicalDevice::from_raw(b.physical_device as u64));
        let queue = device.get_device_queue(b.queue_family_index, b.queue_index);
        let pool = device
            .create_command_pool(
                &vk::CommandPoolCreateInfo::default()
                    .queue_family_index(b.queue_family_index)
                    .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
                None,
            )
            .map_err(|_| xr::Result::ERROR_RUNTIME_FAILURE)?;
        let cmd = device
            .allocate_command_buffers(
                &vk::CommandBufferAllocateInfo::default()
                    .command_pool(pool)
                    .level(vk::CommandBufferLevel::PRIMARY)
                    .command_buffer_count(1),
            )
            .map_err(|_| xr::Result::ERROR_RUNTIME_FAILURE)?[0];
        let fence = device.create_fence(&vk::FenceCreateInfo::default(), None).map_err(|_| xr::Result::ERROR_RUNTIME_FAILURE)?;
        let capture = std::env::var_os("XRMIMIC_CAPTURE").map(|p| {
            let frame = std::env::var("XRMIMIC_CAPTURE_FRAME").ok().and_then(|s| s.parse().ok()).unwrap_or(120);
            (std::path::PathBuf::from(p), frame)
        });
        xlog!("session on queue family {} index {}", b.queue_family_index, b.queue_index);
        let physical = vk::PhysicalDevice::from_raw(b.physical_device as u64);
        let depth_view = std::env::var_os("XRMIMIC_TEST_DEPTH_VIEW").is_some_and(|v| v != "0");
        Ok(SessionGfx { device, queue, pool, instance: inst, physical, cmd, fence, mem_props, shared, capture, depth_view })
    }

    /// Record commands with `f` and run them on the app's queue, waiting for
    /// them to finish (setup and test captures only, never per frame).
    unsafe fn run_once(&self, f: impl FnOnce(&ash::Device, vk::CommandBuffer)) -> Result<(), vk::Result> {
        let d = &self.device;
        d.reset_command_buffer(self.cmd, vk::CommandBufferResetFlags::empty())?;
        d.begin_command_buffer(
            self.cmd,
            &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
        )?;
        f(d, self.cmd);
        d.end_command_buffer(self.cmd)?;
        let cmds = [self.cmd];
        d.queue_submit(self.queue, &[vk::SubmitInfo::default().command_buffers(&cmds)], self.fence)?;
        d.wait_for_fences(&[self.fence], true, u64::MAX)?;
        d.reset_fences(&[self.fence])
    }

    pub fn swapchain_formats(&self) -> Vec<i64> {
        swapchain_formats()
    }

    pub(crate) fn memory_type(&self, bits: u32, want: vk::MemoryPropertyFlags) -> Option<u32> {
        (0..self.mem_props.memory_type_count).find(|&i| {
            bits & (1 << i) != 0 && self.mem_props.memory_types[i as usize].property_flags.contains(want)
        })
    }

    /// The app's graphics adapter: not known on Vulkan (there is no window to place).
    pub fn adapter(&self) -> (u32, i32) {
        (0, 0)
    }

    pub fn create_swapchain(&self, session: u64, ci: &xr::SwapchainCreateInfo) -> Result<SwapchainData, xr::Result> {
        let format = vk::Format::from_raw(ci.format as i32);
        let depth = is_depth(format);
        if !depth && !COLOR_FORMATS.contains(&format) {
            return Err(xr::Result::ERROR_SWAPCHAIN_FORMAT_UNSUPPORTED);
        }
        // 6 faces: a cube map for XR_KHR_composition_layer_cube (square faces).
        let cube = ci.face_count == 6;
        if (ci.face_count != 1 && !cube) || ci.width == 0 || ci.height == 0 || (cube && ci.width != ci.height) {
            return Err(xr::Result::ERROR_VALIDATION_FAILURE);
        }
        let u = ci.usage_flags;
        let mut usage = vk::ImageUsageFlags::TRANSFER_SRC | vk::ImageUsageFlags::TRANSFER_DST;
        if depth {
            usage |= vk::ImageUsageFlags::DEPTH_STENCIL_ATTACHMENT;
        } else {
            usage |= vk::ImageUsageFlags::COLOR_ATTACHMENT | vk::ImageUsageFlags::SAMPLED;
        }
        if u.contains(xr::SwapchainUsageFlags::SAMPLED) {
            usage |= vk::ImageUsageFlags::SAMPLED;
        }
        if u.contains(xr::SwapchainUsageFlags::UNORDERED_ACCESS) {
            usage |= vk::ImageUsageFlags::STORAGE;
        }
        if u.contains(xr::SwapchainUsageFlags::INPUT_ATTACHMENT) {
            usage |= vk::ImageUsageFlags::INPUT_ATTACHMENT;
        }
        let mut flags = vk::ImageCreateFlags::empty();
        if u.contains(xr::SwapchainUsageFlags::MUTABLE_FORMAT) {
            flags |= vk::ImageCreateFlags::MUTABLE_FORMAT;
        }
        let mips = ci.mip_count.max(1);
        let layers = ci.array_size.max(1) * ci.face_count;
        if cube {
            flags |= vk::ImageCreateFlags::CUBE_COMPATIBLE;
        }
        let samples = vk::SampleCountFlags::from_raw(ci.sample_count.max(1));
        // MSAA: no cube maps or mip chains, and only counts this format offers.
        if ci.sample_count > 1 {
            if cube || mips > 1 {
                return Err(xr::Result::ERROR_FEATURE_UNSUPPORTED);
            }
        }
        let info = vk::ImageCreateInfo::default()
            .flags(flags)
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D { width: ci.width, height: ci.height, depth: 1 })
            .mip_levels(mips)
            .array_layers(layers)
            .samples(samples)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(usage)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let d = &self.device;
        let mut images = Vec::new();
        let mut memories = Vec::new();
        unsafe {
            for _ in 0..3 {
                let img = d.create_image(&info, None).map_err(|e| {
                    xlog!("vkCreateImage failed: {e:?}");
                    xr::Result::ERROR_RUNTIME_FAILURE
                })?;
                let req = d.get_image_memory_requirements(img);
                let ty = self
                    .memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)
                    .ok_or(xr::Result::ERROR_RUNTIME_FAILURE)?;
                let mem = d
                    .allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(ty), None)
                    .map_err(|_| xr::Result::ERROR_RUNTIME_FAILURE)?;
                d.bind_image_memory(img, mem, 0).map_err(|_| xr::Result::ERROR_RUNTIME_FAILURE)?;
                images.push(img);
                memories.push(mem);
            }
            // Swapchain images are handed to the app in the attachment layout.
            let (aspect, layout) = if depth {
                let mut a = vk::ImageAspectFlags::DEPTH;
                if has_stencil(format) {
                    a |= vk::ImageAspectFlags::STENCIL;
                }
                (a, vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL)
            } else {
                (vk::ImageAspectFlags::COLOR, vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL)
            };
            let barriers: Vec<vk::ImageMemoryBarrier> = images
                .iter()
                .map(|&image| {
                    vk::ImageMemoryBarrier::default()
                        .src_access_mask(vk::AccessFlags::empty())
                        .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
                        .old_layout(vk::ImageLayout::UNDEFINED)
                        .new_layout(layout)
                        .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                        .image(image)
                        .subresource_range(vk::ImageSubresourceRange {
                            aspect_mask: aspect,
                            base_mip_level: 0,
                            level_count: mips,
                            base_array_layer: 0,
                            layer_count: layers,
                        })
                })
                .collect();
            self.run_once(|d, cmd| {
                d.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TOP_OF_PIPE,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &barriers,
                );
            })
            .map_err(|e| {
                xlog!("swapchain layout transition failed: {e:?}");
                xr::Result::ERROR_RUNTIME_FAILURE
            })?;
        }
        xlog!(
            "swapchain {}x{} array {layers} mips {mips} samples {} format {} usage {:?}",
            ci.width,
            ci.height,
            ci.sample_count,
            format.as_raw(),
            u
        );
        Ok(SwapchainData {
            session,
            device: self.device.clone(),
            images,
            memories,
            width: ci.width,
            height: ci.height,
            faces: ci.face_count,
            mip_count: mips,
            sample_count: ci.sample_count.max(1),
            format,
            next_index: 0,
            acquired: Default::default(),
            last_released: None,
        })
    }

    /// A frame was submitted. Without a display there is nothing to show: the
    /// frame is counted, read back once for `XRMIMIC_CAPTURE`, and read back
    /// for the control API's pictures (`/v1/capture/eyes`; `/v1/capture/window`
    /// gives the same eyes, or their depth as greys while the Depth view is on).
    #[cfg(not(windows))]
    pub fn submit(
        &mut self,
        eyes: &[EyeSource],
        _views: &[(xrmimic_core::math::Pose, xrmimic_core::math::Fov)],
        _quads: &[(EyeSource, crate::shared::QuadMeta)],
        depths: &[(usize, EyeSource, crate::shared::DepthInfo)],
        frame: u64,
    ) {
        self.shared.slot.lock().unwrap().frames += 1;
        let views = std::mem::take(&mut *self.shared.view_request.lock().unwrap());
        if let Some(on) = views[0] {
            self.depth_view = on;
        }
        let eye_waiting = std::mem::take(&mut *self.shared.eye_requests.lock().unwrap());
        let window_waiting = std::mem::take(&mut *self.shared.window_requests.lock().unwrap());
        if !eye_waiting.is_empty() || !window_waiting.is_empty() {
            let colour = unsafe { self.read_eyes(eyes) };
            for tx in eye_waiting {
                let _ = tx.send(colour.clone());
            }
            if !window_waiting.is_empty() {
                let picture = if self.depth_view { unsafe { self.read_depth_view(depths) } } else { colour };
                for tx in window_waiting {
                    let _ = tx.send(picture.clone());
                }
            }
        }
        let Some((path, at)) = &self.capture else { return };
        static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if frame != *at || eyes.is_empty() || TAKEN.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        let written = unsafe { self.read_eyes(eyes) }
            .and_then(|i| crate::log::write_bmp(path, i.width, i.height, &i.bgra).map_err(|e| e.to_string()));
        match written {
            Ok(()) => xlog!("captured frame {frame} to {}", path.display()),
            Err(e) => xlog!("capture failed: {e}"),
        }
    }

    /// One array layer's rectangle of an image, read back through a host
    /// buffer (`texel` bytes per texel of `aspect`); the image is left in `layout`.
    #[allow(clippy::too_many_arguments)]
    unsafe fn read_region(
        &self,
        image: vk::Image,
        layer: u32,
        rect: xr::Rect2Di,
        layout: vk::ImageLayout,
        barrier_aspect: vk::ImageAspectFlags,
        aspect: vk::ImageAspectFlags,
        texel: u32,
    ) -> Result<Vec<u8>, String> {
        let (w, h) = (rect.extent.width.max(0) as u32, rect.extent.height.max(0) as u32);
        if w == 0 || h == 0 {
            return Err("empty image rectangle".into());
        }
        let size = (w * h * texel) as u64;
        let d = &self.device;
        let buf = d
            .create_buffer(&vk::BufferCreateInfo::default().size(size).usage(vk::BufferUsageFlags::TRANSFER_DST), None)
            .map_err(|e| e.to_string())?;
        let req = d.get_buffer_memory_requirements(buf);
        let Some(ty) = self
            .memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT)
        else {
            d.destroy_buffer(buf, None);
            return Err("no host-visible memory".into());
        };
        let mem = match d.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(ty), None) {
            Ok(m) => m,
            Err(e) => {
                d.destroy_buffer(buf, None);
                return Err(e.to_string());
            }
        };
        let result = (|| {
            d.bind_buffer_memory(buf, mem, 0).map_err(|e| e.to_string())?;
            let range = vk::ImageSubresourceRange {
                aspect_mask: barrier_aspect,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: layer,
                layer_count: 1,
            };
            let barrier = |from, to, src, dst| {
                vk::ImageMemoryBarrier::default()
                    .src_access_mask(src)
                    .dst_access_mask(dst)
                    .old_layout(from)
                    .new_layout(to)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image)
                    .subresource_range(range)
            };
            let to_src = [barrier(
                layout,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::AccessFlags::MEMORY_WRITE,
                vk::AccessFlags::TRANSFER_READ,
            )];
            let back = [barrier(
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                layout,
                vk::AccessFlags::TRANSFER_READ,
                vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
            )];
            let region = vk::BufferImageCopy {
                buffer_offset: 0,
                buffer_row_length: 0,
                buffer_image_height: 0,
                image_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: aspect,
                    mip_level: 0,
                    base_array_layer: layer,
                    layer_count: 1,
                },
                image_offset: vk::Offset3D { x: rect.offset.x, y: rect.offset.y, z: 0 },
                image_extent: vk::Extent3D { width: w, height: h, depth: 1 },
            };
            self.run_once(|d, cmd| {
                d.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &to_src,
                );
                d.cmd_copy_image_to_buffer(cmd, image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, buf, &[region]);
                d.cmd_pipeline_barrier(
                    cmd,
                    vk::PipelineStageFlags::TRANSFER,
                    vk::PipelineStageFlags::ALL_COMMANDS,
                    vk::DependencyFlags::empty(),
                    &[],
                    &[],
                    &back,
                );
            })
            .map_err(|e| e.to_string())?;
            let p = d.map_memory(mem, 0, size, vk::MemoryMapFlags::empty()).map_err(|e| e.to_string())?;
            let data = std::slice::from_raw_parts(p as *const u8, size as usize).to_vec();
            d.unmap_memory(mem);
            Ok(data)
        })();
        d.destroy_buffer(buf, None);
        d.free_memory(mem, None);
        result
    }

    /// The projection layer's eyes side by side (BGRA, top row first).
    unsafe fn read_eyes(&self, eyes: &[EyeSource]) -> Result<crate::shared::Image, String> {
        if eyes.is_empty() {
            return Err("the frame has no projection layer".into());
        }
        if eyes.iter().any(|e| e.samples > 1) {
            return Err("MSAA eyes are not resolved on Linux".into());
        }
        let eye_w = eyes.iter().map(|e| e.rect.extent.width as usize).max().unwrap_or(1);
        let eye_h = eyes.iter().map(|e| e.rect.extent.height as usize).max().unwrap_or(1);
        let w = eye_w * 2;
        let mut px = vec![0u8; w * eye_h * 4];
        for (i, e) in eyes.iter().take(2).enumerate() {
            let data = self.read_region(
                e.image,
                e.layer,
                e.rect,
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                vk::ImageAspectFlags::COLOR,
                vk::ImageAspectFlags::COLOR,
                4,
            )?;
            let bgra_in = matches!(e.format, vk::Format::B8G8R8A8_SRGB | vk::Format::B8G8R8A8_UNORM);
            let (ew, eh) = (e.rect.extent.width as usize, e.rect.extent.height as usize);
            for y in 0..eh {
                for x in 0..ew {
                    let s = &data[(y * ew + x) * 4..(y * ew + x) * 4 + 4];
                    let o = (y * w + i * eye_w + x) * 4;
                    let c = if bgra_in { [s[0], s[1], s[2], 255] } else { [s[2], s[1], s[0], 255] };
                    px[o..o + 4].copy_from_slice(&c);
                }
            }
        }
        Ok(crate::shared::Image { width: w as u32, height: eye_h as u32, bgra: px })
    }

    /// The Depth view as the Windows window draws it: each eye's depth as
    /// greys, nearer brighter (math::depth_distance, math::depth_shade).
    unsafe fn read_depth_view(
        &self,
        depths: &[(usize, EyeSource, crate::shared::DepthInfo)],
    ) -> Result<crate::shared::Image, String> {
        use xrmimic_core::math::{depth_distance, depth_shade};
        let eyes: Vec<_> = depths.iter().filter(|(v, _, _)| *v < 2).collect();
        if eyes.is_empty() {
            return Err("the frame has no depth (XR_KHR_composition_layer_depth)".into());
        }
        let eye_w = eyes.iter().map(|(_, e, _)| e.rect.extent.width as usize).max().unwrap_or(1);
        let eye_h = eyes.iter().map(|(_, e, _)| e.rect.extent.height as usize).max().unwrap_or(1);
        let w = eye_w * 2;
        let mut px: Vec<u8> = [0u8, 0, 0, 255].repeat(w * eye_h);
        for (view, e, info) in eyes {
            let texel = if e.format == vk::Format::D16_UNORM { 2 } else { 4 };
            let barrier_aspect = if has_stencil(e.format) {
                vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL
            } else {
                vk::ImageAspectFlags::DEPTH
            };
            let data = self.read_region(
                e.image,
                e.layer,
                e.rect,
                vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL,
                barrier_aspect,
                vk::ImageAspectFlags::DEPTH,
                texel,
            )?;
            let (ew, eh) = (e.rect.extent.width as usize, e.rect.extent.height as usize);
            for y in 0..eh {
                for x in 0..ew {
                    let i = (y * ew + x) * texel as usize;
                    let d = match e.format {
                        vk::Format::D16_UNORM => u16::from_le_bytes([data[i], data[i + 1]]) as f32 / 65535.0,
                        // Copied out as 32 bits, the depth in the low 24.
                        vk::Format::D24_UNORM_S8_UINT => {
                            (u32::from_le_bytes(data[i..i + 4].try_into().unwrap()) & 0xff_ffff) as f32 / 16_777_215.0
                        }
                        _ => f32::from_le_bytes(data[i..i + 4].try_into().unwrap()),
                    };
                    let g = (depth_shade(depth_distance(d, info.min, info.max, info.near, info.far)) * 255.0).round() as u8;
                    let o = (y * w + view * eye_w + x) * 4;
                    px[o..o + 4].copy_from_slice(&[g, g, g, 255]);
                }
            }
        }
        Ok(crate::shared::Image { width: w as u32, height: eye_h as u32, bgra: px })
    }
}
