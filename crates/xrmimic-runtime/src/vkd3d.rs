//! Vulkan apps on Windows (XR_KHR_vulkan_enable2): the app renders into
//! ordinary Vulkan swapchain images (crate::vk); each image has a proxy, a
//! shared D3D11 texture on the runtime's own D3D11 device, imported into the
//! app's VkDevice. At xrEndFrame the runtime copies the used layers into the
//! proxies on the app's queue (resolving MSAA colour, moving depth through a
//! buffer into a float texture) and signals a shared fence; the D3D11 side
//! waits for it and then takes the proxies through the D3D11 path as if a
//! D3D11 app had drawn them, so the window, its views and the server work
//! unchanged.

use crate::d3d::{self, EyeSource, SessionGfx, SwapchainData};
use crate::shared::Shared;
use crate::xlog;
use ash::vk;
use ash::vk::Handle as _;
use openxr_sys as xr;
use std::sync::Arc;
use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE, HMODULE, LUID};
use windows::Win32::Graphics::Direct3D::{D3D_DRIVER_TYPE_UNKNOWN, D3D_FEATURE_LEVEL_11_0, D3D_FEATURE_LEVEL_11_1};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

/// Command buffers in flight (one per frame).
const IN_FLIGHT: usize = 3;

/// The Vulkan formats offered, and the DXGI format of each one's proxy. Depth
/// proxies are float or unorm colour textures; D24_UNORM_S8_UINT is not offered
/// (its depth copies out packed with the stencil's spare bits).
const FORMATS: [(vk::Format, DXGI_FORMAT, DXGI_FORMAT); 8] = [
    // (Vulkan format, the proxy's format, the format the D3D11 path is told)
    (vk::Format::B8G8R8A8_SRGB, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB, DXGI_FORMAT_B8G8R8A8_UNORM_SRGB),
    (vk::Format::B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_FORMAT_B8G8R8A8_UNORM),
    (vk::Format::R8G8B8A8_SRGB, DXGI_FORMAT_R8G8B8A8_UNORM_SRGB, DXGI_FORMAT_R8G8B8A8_UNORM_SRGB),
    (vk::Format::R8G8B8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_FORMAT_R8G8B8A8_UNORM),
    (vk::Format::R16G16B16A16_SFLOAT, DXGI_FORMAT_R16G16B16A16_FLOAT, DXGI_FORMAT_R16G16B16A16_FLOAT),
    (vk::Format::D32_SFLOAT, DXGI_FORMAT_R32_FLOAT, DXGI_FORMAT_D32_FLOAT),
    (vk::Format::D16_UNORM, DXGI_FORMAT_R16_UNORM, DXGI_FORMAT_D16_UNORM),
    (vk::Format::D32_SFLOAT_S8_UINT, DXGI_FORMAT_R32_FLOAT, DXGI_FORMAT_D32_FLOAT),
];

fn all_formats() -> Vec<vk::Format> {
    FORMATS.iter().map(|f| f.0).collect()
}

fn proxy_format(f: vk::Format) -> Option<(vk::Format, DXGI_FORMAT, DXGI_FORMAT)> {
    let &(_, proxy, told) = FORMATS.iter().find(|x| x.0 == f)?;
    // The proxy as Vulkan sees it: the colour twin, or a float/unorm one for depth.
    let vk_proxy = match f {
        vk::Format::D32_SFLOAT | vk::Format::D32_SFLOAT_S8_UINT => vk::Format::R32_SFLOAT,
        vk::Format::D16_UNORM => vk::Format::R16_UNORM,
        other => other,
    };
    Some((vk_proxy, proxy, told))
}

fn is_depth(f: vk::Format) -> bool {
    matches!(f, vk::Format::D32_SFLOAT | vk::Format::D16_UNORM | vk::Format::D32_SFLOAT_S8_UINT)
}

/// A session's Vulkan side: the app's device and the shared fence.
pub struct Bridge {
    vk: crate::vk::SessionGfx,
    mem_ext: ash::khr::external_memory_win32::Device,
    semaphore: vk::Semaphore,
    fence: ID3D11Fence,
    ctx4: ID3D11DeviceContext4,
    cmds: [vk::CommandBuffer; IN_FLIGHT],
    done: [vk::Fence; IN_FLIGHT],
    next: usize,
    /// The fence value of the last Vulkan copy, and the one at which the
    /// D3D11 side last finished reading the proxies.
    value: u64,
    read: u64,
    /// Depth goes image -> buffer -> proxy; this buffer grows as needed.
    staging: Option<(vk::Buffer, vk::DeviceMemory, u64)>,
}

impl Drop for Bridge {
    fn drop(&mut self) {
        let d = &self.vk.device;
        unsafe {
            let _ = d.queue_wait_idle(self.vk.queue);
            for f in self.done {
                d.destroy_fence(f, None);
            }
            d.free_command_buffers(self.vk.pool, &self.cmds);
            d.destroy_semaphore(self.semaphore, None);
            if let Some((b, m, _)) = self.staging.take() {
                d.destroy_buffer(b, None);
                d.free_memory(m, None);
            }
        }
    }
}

/// A swapchain's Vulkan images and the proxies' imports (the proxies' D3D11
/// textures are the swapchain's `images`).
pub struct Images {
    sc: crate::vk::SwapchainData,
    device: ash::Device,
    proxies: Vec<(vk::Image, vk::DeviceMemory)>,
    format: vk::Format,
    layers: u32,
    faces: u32,
    width: u32,
    height: u32,
}

impl Drop for Images {
    fn drop(&mut self) {
        unsafe {
            let _ = self.device.device_wait_idle();
            for (i, m) in self.proxies.drain(..) {
                self.device.destroy_image(i, None);
                self.device.free_memory(m, None);
            }
        }
    }
}

/// What to copy for one eye or layer: from the app's image into its proxy.
#[derive(Clone, Copy, PartialEq)]
pub struct VkCopy {
    app: vk::Image,
    proxy: vk::Image,
    format: vk::Format,
    samples: u32,
    first_layer: u32,
    layers: u32,
    width: u32,
    height: u32,
}

/// The Vulkan images to hand to the app (xrEnumerateSwapchainImages).
pub unsafe fn write_images(images: &Images, out: *mut xr::SwapchainImageBaseHeader) -> xr::Result {
    let out = out as *mut xr::SwapchainImageVulkanKHR;
    for (i, img) in images.sc.images.iter().enumerate() {
        let o = &mut *out.add(i);
        if o.ty != xr::StructureType::SWAPCHAIN_IMAGE_VULKAN_KHR {
            return xr::Result::ERROR_VALIDATION_FAILURE;
        }
        o.image = img.as_raw();
    }
    xr::Result::SUCCESS
}

/// The copy an eye or layer needs (all six faces of a cube map's image).
pub fn copy_for(images: &Images, idx: u32, array_index: u32) -> VkCopy {
    let (first_layer, layers) =
        if images.faces == 6 { (0, images.layers) } else { (array_index.min(images.layers - 1), 1) };
    VkCopy {
        app: images.sc.images[idx as usize],
        proxy: images.proxies[idx as usize].0,
        format: images.format,
        samples: images.sc.sample_count,
        first_layer,
        layers,
        width: images.width,
        height: images.height,
    }
}

/// The adapter a Vulkan physical device runs on.
unsafe fn device_luid(inst: &ash::Instance, pd: vk::PhysicalDevice) -> Option<LUID> {
    let mut id = vk::PhysicalDeviceIDProperties::default();
    let mut p2 = vk::PhysicalDeviceProperties2::default().push_next(&mut id);
    inst.get_physical_device_properties2(pd, &mut p2);
    (id.device_luid_valid != 0).then(|| LUID {
        LowPart: u32::from_le_bytes(id.device_luid[0..4].try_into().unwrap()),
        HighPart: i32::from_le_bytes(id.device_luid[4..8].try_into().unwrap()),
    })
}

/// A Vulkan session: the app's device (crate::vk), a D3D11 device of the
/// runtime's own on the same adapter, and the fence shared between them.
pub unsafe fn session(next: *const std::ffi::c_void, shared: Arc<Shared>) -> Result<SessionGfx, xr::Result> {
    let fail = |what: &str, e: &dyn std::fmt::Debug| {
        xlog!("Vulkan session: {what}: {e:?}");
        xr::Result::ERROR_RUNTIME_FAILURE
    };
    let vkg = crate::vk::SessionGfx::from_session_create(next, shared.clone())?;
    let luid = device_luid(&vkg.instance, vkg.physical)
        .or_else(d3d::default_adapter_luid)
        .ok_or_else(|| fail("no adapter", &()))?;
    let factory: IDXGIFactory4 = CreateDXGIFactory1().map_err(|e| fail("DXGI factory", &e))?;
    let adapter: IDXGIAdapter1 = factory.EnumAdapterByLuid(luid).map_err(|e| fail("adapter by LUID", &e))?;
    let mut device = None;
    D3D11CreateDevice(
        &adapter,
        D3D_DRIVER_TYPE_UNKNOWN,
        HMODULE::default(),
        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
        Some(&[D3D_FEATURE_LEVEL_11_1, D3D_FEATURE_LEVEL_11_0]),
        D3D11_SDK_VERSION,
        Some(&mut device),
        None,
        None,
    )
    .map_err(|e| fail("D3D11 device", &e))?;
    let device: ID3D11Device = device.ok_or_else(|| fail("D3D11 device", &()))?;
    let device5: ID3D11Device5 = device.cast().map_err(|e| fail("ID3D11Device5", &e))?;
    let mut fence: Option<ID3D11Fence> = None;
    device5.CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut fence).map_err(|e| fail("D3D11 fence", &e))?;
    let fence = fence.ok_or_else(|| fail("D3D11 fence", &()))?;
    let handle = fence.CreateSharedHandle(None, GENERIC_ALL.0, None).map_err(|e| fail("fence handle", &e))?;

    let d = &vkg.device;
    let mut ty = vk::SemaphoreTypeCreateInfo::default().semaphore_type(vk::SemaphoreType::TIMELINE).initial_value(0);
    let semaphore = d
        .create_semaphore(&vk::SemaphoreCreateInfo::default().push_next(&mut ty), None)
        .map_err(|e| fail("timeline semaphore", &e))?;
    let sem_ext = ash::khr::external_semaphore_win32::Device::new(&vkg.instance, d);
    let imported = sem_ext.import_semaphore_win32_handle(
        &vk::ImportSemaphoreWin32HandleInfoKHR::default()
            .semaphore(semaphore)
            .handle_type(vk::ExternalSemaphoreHandleTypeFlags::D3D12_FENCE)
            .handle(handle.0 as isize),
    );
    let _ = CloseHandle(handle);
    if let Err(e) = imported {
        d.destroy_semaphore(semaphore, None);
        return Err(fail("importing the D3D11 fence", &e));
    }
    let cmds: [vk::CommandBuffer; IN_FLIGHT] = d
        .allocate_command_buffers(
            &vk::CommandBufferAllocateInfo::default()
                .command_pool(vkg.pool)
                .level(vk::CommandBufferLevel::PRIMARY)
                .command_buffer_count(IN_FLIGHT as u32),
        )
        .map_err(|e| fail("command buffers", &e))?
        .try_into()
        .unwrap();
    let mut done = [vk::Fence::null(); IN_FLIGHT];
    for f in done.iter_mut() {
        *f = d
            .create_fence(&vk::FenceCreateInfo::default().flags(vk::FenceCreateFlags::SIGNALED), None)
            .map_err(|e| fail("fence", &e))?;
    }
    let mem_ext = ash::khr::external_memory_win32::Device::new(&vkg.instance, d);
    let mut gfx = SessionGfx::new(device, shared);
    let ctx4: ID3D11DeviceContext4 = gfx.context.cast().map_err(|e| fail("ID3D11DeviceContext4", &e))?;
    xlog!(
        "Vulkan session: frames go through D3D11 on adapter {:#x}:{:#x} (shared textures and fence)",
        luid.HighPart,
        luid.LowPart
    );
    gfx.vk = Some(Box::new(Bridge {
        vk: vkg,
        mem_ext,
        semaphore,
        fence,
        ctx4,
        cmds,
        done,
        next: 0,
        value: 0,
        read: 0,
        staging: None,
    }));
    Ok(gfx)
}

impl Bridge {
    /// The formats offered: those with proxies that the app's GPU renders to.
    pub fn swapchain_formats(&self) -> Vec<i64> {
        all_formats().into_iter().filter(|&f| self.vk.renders_to(f)).map(|f| f.as_raw() as i64).collect()
    }

    /// The app's swapchain images, and a proxy for each.
    pub fn create_swapchain(
        &self,
        device: &ID3D11Device,
        session: u64,
        ci: &xr::SwapchainCreateInfo,
    ) -> Result<SwapchainData, xr::Result> {
        let format = vk::Format::from_raw(ci.format as i32);
        let Some((vk_proxy, proxy_dxgi, told)) = proxy_format(format) else {
            return Err(xr::Result::ERROR_SWAPCHAIN_FORMAT_UNSUPPORTED);
        };
        if is_depth(format) && ci.sample_count > 1 {
            // Sample 0 of an MSAA depth image needs a shader; not done for Vulkan yet.
            xlog!("MSAA depth swapchains are not supported for Vulkan apps on Windows");
            return Err(xr::Result::ERROR_FEATURE_UNSUPPORTED);
        }
        let sc = self.vk.create_swapchain(session, ci)?;
        let layers = ci.array_size.max(1) * ci.face_count;
        let mut textures = Vec::new();
        let mut proxies = Vec::new();
        for _ in 0..sc.images.len() {
            let (tex, img) = unsafe { self.proxy(device, ci.width, ci.height, layers, proxy_dxgi, vk_proxy) }.map_err(|e| {
                xlog!("swapchain proxy ({}x{} x{layers}, format {}): {e}", ci.width, ci.height, proxy_dxgi.0);
                xr::Result::ERROR_RUNTIME_FAILURE
            })?;
            textures.push(tex);
            proxies.push(img);
        }
        let images = Images {
            device: self.vk.device.clone(),
            proxies,
            format,
            layers,
            faces: ci.face_count,
            width: ci.width,
            height: ci.height,
            sc,
        };
        Ok(SwapchainData {
            session,
            images: textures,
            width: ci.width,
            height: ci.height,
            array_size: ci.array_size.max(1),
            faces: ci.face_count,
            // The D3D11 path sees the proxies: one mip, one sample.
            mip_count: 1,
            sample_count: 1,
            format: told,
            next_index: 0,
            acquired: Default::default(),
            last_released: None,
            vk: Some(images),
        })
    }

    /// A shared D3D11 texture and its import into the app's VkDevice.
    unsafe fn proxy(
        &self,
        device: &ID3D11Device,
        w: u32,
        h: u32,
        layers: u32,
        dxgi: DXGI_FORMAT,
        format: vk::Format,
    ) -> Result<(ID3D11Texture2D, (vk::Image, vk::DeviceMemory)), String> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: layers,
            Format: dxgi,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
        };
        let mut tex = None;
        device.CreateTexture2D(&desc, None, Some(&mut tex)).map_err(|e| format!("CreateTexture2D: {e}"))?;
        let tex: ID3D11Texture2D = tex.ok_or("no texture")?;
        let res: IDXGIResource1 = tex.cast().map_err(|e| e.to_string())?;
        let handle = res
            .CreateSharedHandle(None, (DXGI_SHARED_RESOURCE_READ.0 | DXGI_SHARED_RESOURCE_WRITE.0) as u32, None)
            .map_err(|e| format!("CreateSharedHandle: {e}"))?;
        let r = self.import(handle, w, h, layers, format);
        let _ = CloseHandle(handle);
        Ok((tex, r?))
    }

    unsafe fn import(
        &self,
        handle: HANDLE,
        w: u32,
        h: u32,
        layers: u32,
        format: vk::Format,
    ) -> Result<(vk::Image, vk::DeviceMemory), String> {
        let d = &self.vk.device;
        let ty = vk::ExternalMemoryHandleTypeFlags::D3D11_TEXTURE;
        let mut ext = vk::ExternalMemoryImageCreateInfo::default().handle_types(ty);
        let info = vk::ImageCreateInfo::default()
            .push_next(&mut ext)
            .image_type(vk::ImageType::TYPE_2D)
            .format(format)
            .extent(vk::Extent3D { width: w, height: h, depth: 1 })
            .mip_levels(1)
            .array_layers(layers)
            .samples(vk::SampleCountFlags::TYPE_1)
            .tiling(vk::ImageTiling::OPTIMAL)
            .usage(vk::ImageUsageFlags::TRANSFER_DST | vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::COLOR_ATTACHMENT)
            .sharing_mode(vk::SharingMode::EXCLUSIVE)
            .initial_layout(vk::ImageLayout::UNDEFINED);
        let image = d.create_image(&info, None).map_err(|e| format!("vkCreateImage: {e:?}"))?;
        let mut props = vk::MemoryWin32HandlePropertiesKHR::default();
        let req = d.get_image_memory_requirements(image);
        let bits = match self.mem_ext.get_memory_win32_handle_properties(ty, handle.0 as isize, &mut props) {
            Ok(()) if props.memory_type_bits != 0 => props.memory_type_bits & req.memory_type_bits,
            _ => req.memory_type_bits,
        };
        let Some(mem_type) = self.vk.memory_type(bits, vk::MemoryPropertyFlags::empty()) else {
            d.destroy_image(image, None);
            return Err("no memory type for the import".into());
        };
        let mut import = vk::ImportMemoryWin32HandleInfoKHR::default().handle_type(ty).handle(handle.0 as isize);
        let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
        let alloc = vk::MemoryAllocateInfo::default()
            .allocation_size(req.size)
            .memory_type_index(mem_type)
            .push_next(&mut import)
            .push_next(&mut dedicated);
        let mem = match d.allocate_memory(&alloc, None) {
            Ok(m) => m,
            Err(e) => {
                d.destroy_image(image, None);
                return Err(format!("importing the texture: {e:?}"));
            }
        };
        if let Err(e) = d.bind_image_memory(image, mem, 0) {
            d.destroy_image(image, None);
            d.free_memory(mem, None);
            return Err(format!("vkBindImageMemory: {e:?}"));
        }
        Ok((image, mem))
    }

    /// Room for `size` bytes of depth on the way to the proxies.
    unsafe fn staging(&mut self, size: u64) -> Result<vk::Buffer, vk::Result> {
        if let Some((b, _, s)) = self.staging {
            if s >= size {
                return Ok(b);
            }
        }
        let d = &self.vk.device;
        d.queue_wait_idle(self.vk.queue)?;
        if let Some((b, m, _)) = self.staging.take() {
            d.destroy_buffer(b, None);
            d.free_memory(m, None);
        }
        let size = size.next_power_of_two();
        let buf = d.create_buffer(
            &vk::BufferCreateInfo::default()
                .size(size)
                .usage(vk::BufferUsageFlags::TRANSFER_SRC | vk::BufferUsageFlags::TRANSFER_DST),
            None,
        )?;
        let req = d.get_buffer_memory_requirements(buf);
        let ty = self
            .vk
            .memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)
            .ok_or(vk::Result::ERROR_OUT_OF_DEVICE_MEMORY)?;
        let mem =
            d.allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(ty), None)?;
        d.bind_buffer_memory(buf, mem, 0)?;
        self.staging = Some((buf, mem, size));
        Ok(buf)
    }

    /// Copy this frame's images into their proxies on the app's queue and
    /// make the D3D11 context wait for it. Returns the value to signal once
    /// the D3D11 side has read them.
    pub fn copy(&mut self, sources: &[&EyeSource]) -> Option<u64> {
        let mut copies: Vec<VkCopy> = Vec::new();
        for c in sources.iter().filter_map(|s| s.vk) {
            if !copies.contains(&c) {
                copies.push(c);
            }
        }
        if copies.is_empty() {
            return None;
        }
        match unsafe { self.record_and_submit(&copies) } {
            Ok(v) => {
                unsafe {
                    let _ = self.ctx4.Wait(&self.fence, v);
                }
                Some(v + 1)
            }
            Err(e) => {
                xlog!("Vulkan frame copy failed: {e:?}");
                None
            }
        }
    }

    /// The D3D11 side has read the proxies (its copies are queued): signal it.
    pub fn read_done(&mut self, value: u64) {
        unsafe {
            let _ = self.ctx4.Signal(&self.fence, value);
            self.gfx_flush();
        }
        self.read = value;
    }

    unsafe fn gfx_flush(&self) {
        let ctx: ID3D11DeviceContext = self.ctx4.cast().unwrap();
        ctx.Flush();
    }

    unsafe fn record_and_submit(&mut self, copies: &[VkCopy]) -> Result<u64, vk::Result> {
        // Depth goes through the staging buffer, one region after another.
        let mut depth_bytes = 0u64;
        let mut offsets = Vec::new();
        for c in copies {
            if is_depth(c.format) {
                offsets.push(depth_bytes);
                let texel = if c.format == vk::Format::D16_UNORM { 2 } else { 4 };
                depth_bytes += (c.width as u64 * c.height as u64 * texel * c.layers as u64 + 255) & !255;
            } else {
                offsets.push(0);
            }
        }
        let staging = if depth_bytes > 0 { Some(self.staging(depth_bytes)?) } else { None };
        let d = &self.vk.device;
        let i = self.next;
        self.next = (self.next + 1) % IN_FLIGHT;
        d.wait_for_fences(&[self.done[i]], true, u64::MAX)?;
        d.reset_fences(&[self.done[i]])?;
        let cmd = self.cmds[i];
        d.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty())?;
        d.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default().flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT))?;

        let range = |c: &VkCopy, aspect| vk::ImageSubresourceRange {
            aspect_mask: aspect,
            base_mip_level: 0,
            level_count: 1,
            base_array_layer: c.first_layer,
            layer_count: c.layers,
        };
        let layers = |c: &VkCopy, aspect| vk::ImageSubresourceLayers {
            aspect_mask: aspect,
            mip_level: 0,
            base_array_layer: c.first_layer,
            layer_count: c.layers,
        };
        let app_aspect = |c: &VkCopy| match c.format {
            vk::Format::D32_SFLOAT_S8_UINT => vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL,
            f if is_depth(f) => vk::ImageAspectFlags::DEPTH,
            _ => vk::ImageAspectFlags::COLOR,
        };
        let app_layout = |c: &VkCopy| {
            if is_depth(c.format) {
                vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL
            } else {
                vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL
            }
        };
        let barrier = |image, range, from, to, src, dst| {
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
        let mut before = Vec::new();
        let mut after = Vec::new();
        for c in copies {
            before.push(barrier(
                c.app,
                range(c, app_aspect(c)),
                app_layout(c),
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                vk::AccessFlags::MEMORY_WRITE,
                vk::AccessFlags::TRANSFER_READ,
            ));
            before.push(barrier(
                c.proxy,
                range(c, vk::ImageAspectFlags::COLOR),
                vk::ImageLayout::UNDEFINED,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::AccessFlags::empty(),
                vk::AccessFlags::TRANSFER_WRITE,
            ));
            after.push(barrier(
                c.app,
                range(c, app_aspect(c)),
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                app_layout(c),
                vk::AccessFlags::TRANSFER_READ,
                vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE,
            ));
            after.push(barrier(
                c.proxy,
                range(c, vk::ImageAspectFlags::COLOR),
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                vk::ImageLayout::GENERAL,
                vk::AccessFlags::TRANSFER_WRITE,
                vk::AccessFlags::MEMORY_READ,
            ));
        }
        // Everything earlier on the queue (the app's drawing, the last
        // frame's use of the staging buffer) before these copies.
        let all = [vk::MemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
            .dst_access_mask(vk::AccessFlags::TRANSFER_READ | vk::AccessFlags::TRANSFER_WRITE)];
        d.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::PipelineStageFlags::TRANSFER,
            vk::DependencyFlags::empty(),
            &all,
            &[],
            &before,
        );
        let extent = |c: &VkCopy| vk::Extent3D { width: c.width, height: c.height, depth: 1 };
        let mut depth_regions = Vec::new();
        for (c, &offset) in copies.iter().zip(&offsets) {
            let colour = vk::ImageAspectFlags::COLOR;
            if is_depth(c.format) {
                let region = vk::BufferImageCopy {
                    buffer_offset: offset,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: layers(c, vk::ImageAspectFlags::DEPTH),
                    image_offset: vk::Offset3D::default(),
                    image_extent: extent(c),
                };
                d.cmd_copy_image_to_buffer(cmd, c.app, vk::ImageLayout::TRANSFER_SRC_OPTIMAL, staging.unwrap(), &[region]);
                depth_regions.push((c, region));
            } else if c.samples > 1 {
                let region = vk::ImageResolve {
                    src_subresource: layers(c, colour),
                    src_offset: vk::Offset3D::default(),
                    dst_subresource: layers(c, colour),
                    dst_offset: vk::Offset3D::default(),
                    extent: extent(c),
                };
                d.cmd_resolve_image(
                    cmd,
                    c.app,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    c.proxy,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                );
            } else {
                let region = vk::ImageCopy {
                    src_subresource: layers(c, colour),
                    src_offset: vk::Offset3D::default(),
                    dst_subresource: layers(c, colour),
                    dst_offset: vk::Offset3D::default(),
                    extent: extent(c),
                };
                d.cmd_copy_image(
                    cmd,
                    c.app,
                    vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                    c.proxy,
                    vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                    &[region],
                );
            }
        }
        if !depth_regions.is_empty() {
            let b = [vk::BufferMemoryBarrier::default()
                .src_access_mask(vk::AccessFlags::TRANSFER_WRITE)
                .dst_access_mask(vk::AccessFlags::TRANSFER_READ)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .buffer(staging.unwrap())
                .offset(0)
                .size(vk::WHOLE_SIZE)];
            d.cmd_pipeline_barrier(
                cmd,
                vk::PipelineStageFlags::TRANSFER,
                vk::PipelineStageFlags::TRANSFER,
                vk::DependencyFlags::empty(),
                &[],
                &b,
                &[],
            );
            for (c, region) in depth_regions {
                let r = vk::BufferImageCopy { image_subresource: layers(c, vk::ImageAspectFlags::COLOR), ..region };
                d.cmd_copy_buffer_to_image(cmd, staging.unwrap(), c.proxy, vk::ImageLayout::TRANSFER_DST_OPTIMAL, &[r]);
            }
        }
        d.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::TRANSFER,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &after,
        );
        d.end_command_buffer(cmd)?;

        // Wait until the D3D11 side has read the last copies; signal this one.
        self.value = self.value.max(self.read) + 1;
        let waits = [self.read];
        let signals = [self.value];
        let mut timeline =
            vk::TimelineSemaphoreSubmitInfo::default().wait_semaphore_values(&waits).signal_semaphore_values(&signals);
        let sems = [self.semaphore];
        let stages = [vk::PipelineStageFlags::TRANSFER];
        let cmds = [cmd];
        let submit = vk::SubmitInfo::default()
            .push_next(&mut timeline)
            .wait_semaphores(&sems)
            .wait_dst_stage_mask(&stages)
            .command_buffers(&cmds)
            .signal_semaphores(&sems);
        d.queue_submit(self.vk.queue, &[submit], self.done[i])?;
        Ok(self.value)
    }
}
