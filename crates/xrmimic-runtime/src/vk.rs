//! Vulkan side (Linux): XR_KHR_vulkan_enable2. The runtime creates the app's
//! VkInstance and VkDevice, hands out swapchain images made on that device,
//! and can read a submitted frame back for tests (`XRMIMIC_CAPTURE`).

use crate::shared::Shared;
use crate::xlog;
use ash::vk;
use ash::vk::Handle as _;
use openxr_sys as xr;
use std::ffi::{c_void, CStr};
use std::sync::{Arc, Mutex};

/// The extensions this backend adds.
pub const EXTENSIONS: &[(&str, u32)] = &[("XR_KHR_vulkan_enable2", 2), ("XR_KHR_convert_timespec_time", 1)];

/// Formats offered to apps, preferred first (Chrome takes the first of
/// BGRA sRGB, BGRA, RGBA sRGB, RGBA that it finds).
const COLOR_FORMATS: [vk::Format; 4] = [
    vk::Format::B8G8R8A8_SRGB,
    vk::Format::B8G8R8A8_UNORM,
    vk::Format::R8G8B8A8_SRGB,
    vk::Format::R8G8B8A8_UNORM,
];
const DEPTH_FORMATS: [vk::Format; 3] =
    [vk::Format::D24_UNORM_S8_UINT, vk::Format::D32_SFLOAT, vk::Format::D16_UNORM];

pub fn swapchain_formats() -> Vec<i64> {
    COLOR_FORMATS.iter().chain(DEPTH_FORMATS.iter()).map(|f| f.as_raw() as i64).collect()
}

fn is_depth(f: vk::Format) -> bool {
    DEPTH_FORMATS.contains(&f)
}

/// What the runtime and the headless headset thread exchange.
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
            "xrConvertTimespecTimeToTimeKHR" => t(timespec_to_time as xr::pfn::ConvertTimespecTimeToTimeKHR),
            "xrConvertTimeToTimespecTimeKHR" => t(time_to_timespec as xr::pfn::ConvertTimeToTimespecTimeKHR),
            _ => return None,
        })
    }
}

// XR_KHR_convert_timespec_time: XrTime is CLOCK_MONOTONIC in nanoseconds.
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

pub struct SessionGfx {
    device: ash::Device,
    queue: vk::Queue,
    pool: vk::CommandPool,
    cmd: vk::CommandBuffer,
    fence: vk::Fence,
    mem_props: vk::PhysicalDeviceMemoryProperties,
    pub shared: Arc<Shared>,
    capture: Option<(std::path::PathBuf, u64)>,
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
        Ok(SessionGfx { device, queue, pool, cmd, fence, mem_props, shared, capture })
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

    fn memory_type(&self, bits: u32, want: vk::MemoryPropertyFlags) -> Option<u32> {
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
                if format == vk::Format::D24_UNORM_S8_UINT {
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

    /// A frame was submitted. Without a display there is nothing to show; the
    /// frame is only counted, and read back once when a test asks for it.
    pub fn submit(
        &mut self,
        eyes: &[EyeSource],
        _views: &[(xrmimic_core::math::Pose, xrmimic_core::math::Fov)],
        _quads: &[(EyeSource, crate::shared::QuadMeta)],
        _depths: &[(usize, EyeSource, crate::shared::DepthInfo)],
        frame: u64,
    ) {
        self.shared.slot.lock().unwrap().frames += 1;
        // The control API's captures are not read back on Linux yet.
        for tx in std::mem::take(&mut *self.shared.eye_requests.lock().unwrap()) {
            let _ = tx.send(Err("eye captures are not available on Linux yet".into()));
        }
        let Some((path, at)) = &self.capture else { return };
        static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
        if frame != *at || eyes.is_empty() || TAKEN.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        if eyes.iter().any(|e| e.samples > 1) {
            xlog!("capture skipped: MSAA eyes are not resolved on Linux");
            return;
        }
        match unsafe { self.capture_bmp(eyes, path) } {
            Ok(()) => xlog!("captured frame {frame} to {}", path.display()),
            Err(e) => xlog!("capture failed: {e}"),
        }
    }

    /// Both eyes side by side into a host buffer, then a top-down BMP.
    unsafe fn capture_bmp(&self, eyes: &[EyeSource], path: &std::path::Path) -> Result<(), String> {
        let eye_w = eyes.iter().map(|e| e.rect.extent.width as u32).max().unwrap_or(1);
        let eye_h = eyes.iter().map(|e| e.rect.extent.height as u32).max().unwrap_or(1);
        let (w, h) = (eye_w * 2, eye_h);
        let size = (w * h * 4) as u64;
        let d = &self.device;
        let buf = d
            .create_buffer(
                &vk::BufferCreateInfo::default().size(size).usage(vk::BufferUsageFlags::TRANSFER_DST),
                None,
            )
            .map_err(|e| e.to_string())?;
        let req = d.get_buffer_memory_requirements(buf);
        let ty = self
            .memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT)
            .ok_or("no host-visible memory")?;
        let mem = d
            .allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(ty), None)
            .map_err(|e| e.to_string())?;
        d.bind_buffer_memory(buf, mem, 0).map_err(|e| e.to_string())?;

        // Each (image, layer) moves to TRANSFER_SRC and back once.
        let mut subs: Vec<(vk::Image, u32)> = Vec::new();
        for e in eyes.iter().take(2) {
            if !subs.contains(&(e.image, e.layer)) {
                subs.push((e.image, e.layer));
            }
        }
        let barrier = |image, layer, from, to, src, dst| {
            vk::ImageMemoryBarrier::default()
                .src_access_mask(src)
                .dst_access_mask(dst)
                .old_layout(from)
                .new_layout(to)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(vk::ImageSubresourceRange {
                    aspect_mask: vk::ImageAspectFlags::COLOR,
                    base_mip_level: 0,
                    level_count: 1,
                    base_array_layer: layer,
                    layer_count: 1,
                })
        };
        let to_src: Vec<_> = subs
            .iter()
            .map(|&(i, l)| {
                barrier(
                    i,
                    l,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::AccessFlags::COLOR_ATTACHMENT_WRITE | vk::AccessFlags::TRANSFER_WRITE,
                    vk::AccessFlags::TRANSFER_READ,
                )
            })
            .collect();
        let back: Vec<_> = subs
            .iter()
            .map(|&(i, l)| {
                barrier(
                    i,
                    l,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL,
                    vk::AccessFlags::TRANSFER_READ,
                    vk::AccessFlags::COLOR_ATTACHMENT_WRITE | vk::AccessFlags::TRANSFER_WRITE,
                )
            })
            .collect();
        let r = self.run_once(|d, cmd| {
            d.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &to_src,
            );
            for (i, e) in eyes.iter().enumerate().take(2) {
                let region = vk::BufferImageCopy {
                    buffer_offset: (i as u32 * eye_w * 4) as u64,
                    buffer_row_length: w,
                    buffer_image_height: h,
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: e.layer,
                        layer_count: 1,
                    },
                    image_offset: vk::Offset3D { x: e.rect.offset.x, y: e.rect.offset.y, z: 0 },
                    image_extent: vk::Extent3D {
                        width: e.rect.extent.width as u32,
                        height: e.rect.extent.height as u32,
                        depth: 1,
                    },
                };
                d.cmd_copy_image_to_buffer(cmd, e.image, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, buf, &[region]);
            }
            d.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::ALL_COMMANDS,
                vk::DependencyFlags::empty(),
                &[],
                &[],
                &back,
            );
        });
        let result = r.map_err(|e| e.to_string()).and_then(|()| {
            let p = d.map_memory(mem, 0, size, vk::MemoryMapFlags::empty()).map_err(|e| e.to_string())?;
            let src = std::slice::from_raw_parts(p as *const u8, size as usize);
            let bgra_in = matches!(eyes[0].format, vk::Format::B8G8R8A8_SRGB | vk::Format::B8G8R8A8_UNORM);
            let mut px = Vec::with_capacity(size as usize);
            for c in src.chunks(4) {
                if bgra_in {
                    px.extend_from_slice(&[c[0], c[1], c[2], 255]);
                } else {
                    px.extend_from_slice(&[c[2], c[1], c[0], 255]);
                }
            }
            d.unmap_memory(mem);
            crate::log::write_bmp(path, w, h, &px).map_err(|e| e.to_string())
        });
        d.destroy_buffer(buf, None);
        d.free_memory(mem, None);
        result
    }
}
