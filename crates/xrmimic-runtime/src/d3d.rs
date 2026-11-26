//! D3D11 side: swapchain images on the app's device, and the copy of each
//! submitted frame into a shared side-by-side texture the window displays.

use crate::shared::Shared;
use crate::{time, xlog};
use openxr_sys as xr;
use std::sync::Arc;
use windows::core::Interface;
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE, LUID};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

/// The extensions this backend adds.
pub const EXTENSIONS: &[(&str, u32)] = &[
    // Sandboxed Chrome only keeps runtimes that promise to work inside its
    // AppContainer. xrmimic does: it needs no IPC, only its own folder's files.
    ("XR_EXT_win32_appcontainer_compatible", 1),
    ("XR_KHR_D3D11_enable", 9),
    ("XR_KHR_win32_convert_performance_counter_time", 1),
];

/// Entry points of this backend's extensions.
pub fn lookup(name: &str) -> Option<xr::pfn::VoidFunction> {
    use std::mem::transmute as t;
    unsafe {
        Some(match name {
            "xrGetD3D11GraphicsRequirementsKHR" => {
                t(d3d11_requirements as xr::pfn::GetD3D11GraphicsRequirementsKHR)
            }
            "xrConvertWin32PerformanceCounterToTimeKHR" => {
                t(qpc_to_time as xr::pfn::ConvertWin32PerformanceCounterToTimeKHR)
            }
            "xrConvertTimeToWin32PerformanceCounterKHR" => {
                t(time_to_qpc as xr::pfn::ConvertTimeToWin32PerformanceCounterKHR)
            }
            _ => return None,
        })
    }
}

pub unsafe extern "system" fn d3d11_requirements(
    instance: xr::Instance,
    system: xr::SystemId,
    req: *mut xr::GraphicsRequirementsD3D11KHR,
) -> xr::Result {
    crate::traced!("xrGetD3D11GraphicsRequirementsKHR", [instance, system, req], || {
        if let Some(e) = crate::api::check_system(instance, system) {
            return e;
        }
        let Some(luid) = default_adapter_luid() else {
            return xr::Result::ERROR_RUNTIME_FAILURE;
        };
        (*req).adapter_luid = xr::platform::LUID { LowPart: luid.LowPart, HighPart: luid.HighPart };
        (*req).min_feature_level = 0xb000; // D3D_FEATURE_LEVEL_11_0
        xlog!("D3D11 requirements: adapter luid {:#x}:{:#x}", luid.HighPart, luid.LowPart);
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn qpc_to_time(
    _instance: xr::Instance,
    counter: *const i64,
    out: *mut xr::Time,
) -> xr::Result {
    crate::traced!("xrConvertWin32PerformanceCounterToTimeKHR", [_instance, counter, out], || {
        *out = xr::Time::from_nanos(time::ticks_to_ns(*counter, time::qpc_frequency()));
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn time_to_qpc(_instance: xr::Instance, t: xr::Time, out: *mut i64) -> xr::Result {
    crate::traced!("xrConvertTimeToWin32PerformanceCounterKHR", [_instance, t, out], || {
        if t.as_nanos() <= 0 {
            return xr::Result::ERROR_TIME_INVALID;
        }
        *out = time::ns_to_ticks(t.as_nanos(), time::qpc_frequency());
        xr::Result::SUCCESS
    })
}


pub fn swapchain_formats() -> Vec<i64> {
    COLOR_FORMATS.iter().chain(DEPTH_FORMATS.iter()).map(|f| f.0 as i64).collect()
}

pub unsafe fn write_images(sc: &SwapchainData, out: *mut xr::SwapchainImageBaseHeader) -> xr::Result {
    let out = out as *mut xr::SwapchainImageD3D11KHR;
    for (i, img) in sc.images.iter().enumerate() {
        let o = &mut *out.add(i);
        if o.ty != xr::StructureType::SWAPCHAIN_IMAGE_D3D11_KHR {
            return xr::Result::ERROR_VALIDATION_FAILURE;
        }
        o.texture = img.as_raw();
    }
    xr::Result::SUCCESS
}

pub fn eye_source(sc: &SwapchainData, idx: u32, sub: &xr::SwapchainSubImage) -> EyeSource {
    EyeSource {
        texture: sc.images[idx as usize].clone(),
        subresource: sub.image_array_index * sc.mip_count, // D3D11CalcSubresource(0, slice, mips)
        mips: sc.mip_count,
        rect: sub.image_rect,
        format: sc.format,
        sample_count: sc.sample_count,
    }
}

/// What the runtime and the window thread exchange about frames.
#[derive(Default)]
pub struct Slot {
    /// NT handles of the ring of shared side-by-side textures and of the fence
    /// signalled after each copy (all owned by the runtime).
    pub handles: Vec<usize>,
    pub fence: usize,
    /// Ring slot holding the newest frame, and the fence value that marks it done.
    pub latest: usize,
    pub latest_value: u64,
    pub generation: u64,
    pub eye_w: u32,
    pub eye_h: u32,
    pub format: i32,
    /// Frames copied in so far.
    pub frames: u64,
    /// The newest frame's eye poses (STAGE space) and fields of view.
    pub views: Vec<(xrmimic_core::math::Pose, xrmimic_core::math::Fov)>,
    /// The newest frame's quad layers, in order.
    pub quads: Vec<QuadSlot>,
    /// The newest frame's depth buffers, one per eye that has one.
    pub depth: Vec<DepthSlot>,
}

/// One eye's depth buffer as the window sees it: a ring of shared textures
/// (typeless copies of the app's whole depth image), the format to read them
/// with, the eye's rectangle in them (x, y, w, h) and how to read the values.
#[derive(Clone)]
pub struct DepthSlot {
    pub eye: usize,
    pub handles: Vec<usize>,
    pub generation: u64,
    pub srv_format: i32,
    pub rect: [i32; 4],
    pub info: crate::shared::DepthInfo,
}

/// The typeless format a depth format can be copied into, and the format a
/// shader reads its depth through.
fn depth_formats(f: DXGI_FORMAT) -> Option<(DXGI_FORMAT, DXGI_FORMAT)> {
    match f {
        DXGI_FORMAT_D24_UNORM_S8_UINT => Some((DXGI_FORMAT_R24G8_TYPELESS, DXGI_FORMAT_R24_UNORM_X8_TYPELESS)),
        DXGI_FORMAT_D32_FLOAT => Some((DXGI_FORMAT_R32_TYPELESS, DXGI_FORMAT_R32_FLOAT)),
        DXGI_FORMAT_D16_UNORM => Some((DXGI_FORMAT_R16_TYPELESS, DXGI_FORMAT_R16_UNORM)),
        DXGI_FORMAT_D32_FLOAT_S8X24_UINT => {
            Some((DXGI_FORMAT_R32G8X24_TYPELESS, DXGI_FORMAT_R32_FLOAT_X8X24_TYPELESS))
        }
        _ => None,
    }
}

/// Depth images the window cannot get a plain copy of: MSAA ones,
/// D32_FLOAT_S8X24_UINT (Unity's 24-bit depth on D3D11), whose typeless
/// format cannot be a shared texture, and upside-down ones (`flipped`). Their
/// depth is drawn into an R32_FLOAT ring instead (sample 0 for MSAA).
fn depth_drawn(format: DXGI_FORMAT, sample_count: u32, flipped: bool) -> bool {
    flipped || sample_count > 1 || format == DXGI_FORMAT_D32_FLOAT_S8X24_UINT
}

/// One quad layer as the window sees it: its own ring of shared textures
/// (same ring index and fence as the eyes) and where to draw it.
#[derive(Clone)]
pub struct QuadSlot {
    pub handles: Vec<usize>,
    pub generation: u64,
    pub meta: crate::shared::QuadMeta,
}

/// The runtime's side of a quad ring.
struct QuadRing {
    texs: Vec<ID3D11Texture2D>,
    handles: Vec<HANDLE>,
    w: u32,
    h: u32,
    format: DXGI_FORMAT,
    generation: u64,
}

impl Drop for QuadRing {
    fn drop(&mut self) {
        unsafe {
            for h in &self.handles {
                let _ = CloseHandle(*h);
            }
        }
    }
}

/// Formats offered to apps, preferred first.
pub const COLOR_FORMATS: [DXGI_FORMAT; 5] = [
    DXGI_FORMAT_R8G8B8A8_UNORM_SRGB,
    DXGI_FORMAT_B8G8R8A8_UNORM_SRGB,
    DXGI_FORMAT_R8G8B8A8_UNORM,
    DXGI_FORMAT_B8G8R8A8_UNORM,
    DXGI_FORMAT_R16G16B16A16_FLOAT,
];
pub const DEPTH_FORMATS: [DXGI_FORMAT; 4] = [
    DXGI_FORMAT_D24_UNORM_S8_UINT,
    DXGI_FORMAT_D32_FLOAT,
    DXGI_FORMAT_D16_UNORM,
    // Unity's OpenXR plugin asks for this one in its 24-bit depth mode.
    DXGI_FORMAT_D32_FLOAT_S8X24_UINT,
];

pub fn is_depth(f: DXGI_FORMAT) -> bool {
    DEPTH_FORMATS.contains(&f)
}

/// Adapter the runtime asks apps to use: the first hardware adapter.
pub fn default_adapter_luid() -> Option<LUID> {
    unsafe {
        let factory: IDXGIFactory1 = CreateDXGIFactory1().ok()?;
        let mut i = 0;
        while let Ok(a) = factory.EnumAdapters1(i) {
            let d = a.GetDesc1().ok()?;
            if d.Flags & DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32 == 0 {
                return Some(d.AdapterLuid);
            }
            i += 1;
        }
        None
    }
}

pub fn device_luid(device: &ID3D11Device) -> Option<LUID> {
    unsafe {
        let dxgi: IDXGIDevice = device.cast().ok()?;
        let adapter = dxgi.GetAdapter().ok()?;
        Some(adapter.GetDesc().ok()?.AdapterLuid)
    }
}

/// Frames go round a ring of shared textures. After each copy the app's
/// context signals a shared fence; the window's GPU waits on that fence
/// before reading. The app never waits for the window, so the window's
/// vsync'd presents cannot slow the app down.
pub const RING: usize = 3;

struct Compositor {
    texs: Vec<ID3D11Texture2D>,
    handles: Vec<HANDLE>,
    fence: ID3D11Fence,
    fence_handle: HANDLE,
    ctx4: ID3D11DeviceContext4,
    next: usize,
    value: u64,
    eye_w: u32,
    eye_h: u32,
    format: DXGI_FORMAT,
}

impl Drop for Compositor {
    fn drop(&mut self) {
        unsafe {
            for h in &self.handles {
                let _ = CloseHandle(*h);
            }
            let _ = CloseHandle(self.fence_handle);
        }
    }
}

pub struct SessionGfx {
    pub device: ID3D11Device,
    pub context: ID3D11DeviceContext,
    pub luid: LUID,
    pub shared: Arc<Shared>,
    /// sRGB colour swapchains are made as typed UNORM textures (Chromium).
    pub srgb_as_unorm: bool,
    /// The app's depth images are upside down relative to its colour (Unity
    /// on D3D11): they are read flipped, so the views that use depth line up.
    pub depth_flipped: bool,
    compositor: Option<Compositor>,
    generation: u64,
    quad_rings: Vec<QuadRing>,
    depth_rings: Vec<Option<QuadRing>>,
    capture: Option<(std::path::PathBuf, u64)>,
    /// MSAA colour images resolve here first (one per eye or layer slot).
    resolves: Vec<Option<ID3D11Texture2D>>,
    /// MSAA depth images: a typeless copy each (the depth format itself
    /// cannot be read by a shader), and the pass that takes sample 0 of it.
    msaa_depth: Vec<Option<ID3D11Texture2D>>,
    depth_resolver: Option<DepthResolver>,
}

// The app's device is free-threaded; its immediate context is only used from
// the app's own xrEndFrame calls, under the runtime lock.
unsafe impl Send for SessionGfx {}

#[allow(dead_code)]
pub struct SwapchainData {
    pub session: u64,
    pub images: Vec<ID3D11Texture2D>,
    pub width: u32,
    pub height: u32,
    pub array_size: u32,
    /// 1, or 6 for a cube map (array slices face-major: slice = index * 6 + face).
    pub faces: u32,
    pub mip_count: u32,
    pub sample_count: u32,
    pub format: DXGI_FORMAT,
    pub next_index: u32,
    pub acquired: std::collections::VecDeque<u32>,
    pub last_released: Option<u32>,
}

unsafe impl Send for SwapchainData {}

/// One eye's part of a projection layer.
pub struct EyeSource {
    pub texture: ID3D11Texture2D,
    pub subresource: u32,
    /// Mip levels per array slice (the subresource step from one slice to the next).
    pub mips: u32,
    pub rect: xr::Rect2Di,
    pub format: DXGI_FORMAT,
    pub sample_count: u32,
}

/// Reads sample 0 of an MSAA depth image into an R32_FLOAT texture, on the
/// app's own context with a context state of its own, so the app's pipeline
/// state is left as it was.
struct DepthResolver {
    vs: ID3D11VertexShader,
    ps: ID3D11PixelShader,
    /// The same for a single-sample image (`ps1`).
    ps1: ID3D11PixelShader,
    /// Both again, reading the image upside down (`ps_flip`, `ps1_flip`).
    ps_flip: ID3D11PixelShader,
    ps1_flip: ID3D11PixelShader,
    state: ID3DDeviceContextState,
}

const DEPTH_RESOLVE: &str = r#"
Texture2DMSArray<float> d : register(t0);
Texture2DArray<float> d1 : register(t0);
float4 vs(uint id : SV_VertexID) : SV_Position {
    float2 uv = float2((id << 1) & 2, id & 2);
    return float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
}
float ps(float4 p : SV_Position) : SV_Target { return d.Load(int3(p.xy, 0), 0); }
float ps1(float4 p : SV_Position) : SV_Target { return d1.Load(int4(p.xy, 0, 0)); }
float ps_flip(float4 p : SV_Position) : SV_Target {
    uint w, h, n, s;
    d.GetDimensions(w, h, n, s);
    int2 q = int2(p.xy);
    return d.Load(int3(q.x, h - 1 - q.y, 0), 0);
}
float ps1_flip(float4 p : SV_Position) : SV_Target {
    uint w, h, n, m;
    d1.GetDimensions(0, w, h, n, m);
    int2 q = int2(p.xy);
    return d1.Load(int4(q.x, h - 1 - q.y, 0, 0));
}
"#;

unsafe fn compile(src: &str, entry: windows::core::PCSTR, target: windows::core::PCSTR) -> Option<Vec<u8>> {
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    let mut blob = None;
    let mut err = None;
    if D3DCompile(src.as_ptr() as *const _, src.len(), None, None, None, entry, target, 0, 0, &mut blob, Some(&mut err)).is_err() {
        if let Some(err) = err {
            let msg = std::slice::from_raw_parts(err.GetBufferPointer() as *const u8, err.GetBufferSize());
            xlog!("depth resolve shader: {}", String::from_utf8_lossy(msg));
        }
        return None;
    }
    let blob = blob?;
    Some(std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()).to_vec())
}

impl DepthResolver {
    unsafe fn new(device: &ID3D11Device) -> Option<DepthResolver> {
        use windows::core::s;
        let vs_code = compile(DEPTH_RESOLVE, s!("vs"), s!("vs_5_0"))?;
        let ps_code = compile(DEPTH_RESOLVE, s!("ps"), s!("ps_5_0"))?;
        let ps1_code = compile(DEPTH_RESOLVE, s!("ps1"), s!("ps_5_0"))?;
        let ps_flip_code = compile(DEPTH_RESOLVE, s!("ps_flip"), s!("ps_5_0"))?;
        let ps1_flip_code = compile(DEPTH_RESOLVE, s!("ps1_flip"), s!("ps_5_0"))?;
        let (mut vs, mut ps, mut ps1, mut ps_flip, mut ps1_flip) = (None, None, None, None, None);
        device.CreatePixelShader(&ps_flip_code, None, Some(&mut ps_flip)).ok()?;
        device.CreatePixelShader(&ps1_flip_code, None, Some(&mut ps1_flip)).ok()?;
        device.CreateVertexShader(&vs_code, None, Some(&mut vs)).ok()?;
        device.CreatePixelShader(&ps_code, None, Some(&mut ps)).ok()?;
        device.CreatePixelShader(&ps1_code, None, Some(&mut ps1)).ok()?;
        let device1: ID3D11Device1 = device.cast().ok()?;
        let mut state = None;
        let level = device.GetFeatureLevel();
        if let Err(e) =
            device1.CreateDeviceContextState(0, &[level], D3D11_SDK_VERSION, &ID3D11Device::IID, None, Some(&mut state))
        {
            xlog!("CreateDeviceContextState failed: {e:?}");
            return None;
        }
        Some(DepthResolver { vs: vs?, ps: ps?, ps1: ps1?, ps_flip: ps_flip?, ps1_flip: ps1_flip?, state: state? })
    }
}

impl SessionGfx {
    /// The session's graphics binding from xrCreateSession's `next` chain.
    pub unsafe fn from_session_create(next: *const std::ffi::c_void, shared: Arc<Shared>) -> Result<SessionGfx, xr::Result> {
        let b = crate::api::find_in_chain(next, xr::StructureType::GRAPHICS_BINDING_D3D11_KHR);
        if b.is_null() {
            xlog!("xrCreateSession: no D3D11 graphics binding");
            return Err(xr::Result::ERROR_GRAPHICS_DEVICE_INVALID);
        }
        let raw = (*(b as *const xr::GraphicsBindingD3D11KHR)).device;
        let Some(device) = ID3D11Device::from_raw_borrowed(&raw).cloned() else {
            return Err(xr::Result::ERROR_GRAPHICS_DEVICE_INVALID);
        };
        Ok(SessionGfx::new(device, shared))
    }

    pub fn new(device: ID3D11Device, shared: Arc<Shared>) -> SessionGfx {
        let context = unsafe { device.GetImmediateContext() }.expect("immediate context");
        let luid = device_luid(&device).unwrap_or_default();
        let capture = std::env::var_os("XRMIMIC_CAPTURE").map(|p| {
            let frame = std::env::var("XRMIMIC_CAPTURE_FRAME").ok().and_then(|s| s.parse().ok()).unwrap_or(120);
            (std::path::PathBuf::from(p), frame)
        });
        SessionGfx {
            device,
            context,
            luid,
            shared,
            srgb_as_unorm: false,
            depth_flipped: false,
            compositor: None,
            generation: 0,
            quad_rings: Vec::new(),
            depth_rings: Vec::new(),
            capture,
            resolves: Vec::new(),
            msaa_depth: Vec::new(),
            depth_resolver: None,
        }
    }

    pub fn create_swapchain(&self, session: u64, ci: &xr::SwapchainCreateInfo) -> Result<SwapchainData, xr::Result> {
        let format = DXGI_FORMAT(ci.format as i32);
        let depth = is_depth(format);
        if !depth && !COLOR_FORMATS.contains(&format) {
            return Err(xr::Result::ERROR_SWAPCHAIN_FORMAT_UNSUPPORTED);
        }
        // 6 faces: a cube map for XR_KHR_composition_layer_cube (square faces).
        let cube = ci.face_count == 6;
        if (ci.face_count != 1 && !cube) || ci.width == 0 || ci.height == 0 || (cube && ci.width != ci.height) {
            return Err(xr::Result::ERROR_VALIDATION_FAILURE);
        }
        // MSAA: the sample counts the device has for the format; no MSAA cube
        // maps or mip chains (D3D11 has neither).
        let samples = ci.sample_count.max(1);
        if samples > 1 {
            let levels = unsafe { self.device.CheckMultisampleQualityLevels(format, samples) }.unwrap_or(0);
            if cube || ci.mip_count > 1 || levels == 0 {
                xlog!("swapchain: {samples} samples unsupported (format {}, faces {}, mips {})", format.0, ci.face_count, ci.mip_count);
                return Err(xr::Result::ERROR_FEATURE_UNSUPPORTED);
            }
        }
        let usage = ci.usage_flags;
        let mut bind = 0u32;
        if usage.contains(xr::SwapchainUsageFlags::COLOR_ATTACHMENT) || !depth {
            bind |= D3D11_BIND_RENDER_TARGET.0 as u32;
        }
        if usage.contains(xr::SwapchainUsageFlags::DEPTH_STENCIL_ATTACHMENT) || depth {
            bind = D3D11_BIND_DEPTH_STENCIL.0 as u32;
        }
        if !depth {
            bind |= D3D11_BIND_SHADER_RESOURCE.0 as u32;
        }
        if usage.contains(xr::SwapchainUsageFlags::UNORDERED_ACCESS) && !depth {
            bind |= D3D11_BIND_UNORDERED_ACCESS.0 as u32;
        }
        let cube_flag = if cube { D3D11_RESOURCE_MISC_TEXTURECUBE.0 as u32 } else { 0 };
        // Chromium asks for sRGB but writes sRGB-encoded bytes through linear
        // views, and its WebGPU sessions wrap the image as plain RGBA8: its
        // shared-image code refuses a typed sRGB texture (the XR process dies)
        // and Dawn a typeless one (the page's device is lost). A typed UNORM
        // texture of the same family suits both, and the window still reads
        // the frame as sRGB. Other apps get the format they asked for.
        let texture_format = match format {
            DXGI_FORMAT_R8G8B8A8_UNORM_SRGB if self.srgb_as_unorm => DXGI_FORMAT_R8G8B8A8_UNORM,
            DXGI_FORMAT_B8G8R8A8_UNORM_SRGB if self.srgb_as_unorm => DXGI_FORMAT_B8G8R8A8_UNORM,
            f => f,
        };
        let mut desc = D3D11_TEXTURE2D_DESC {
            Width: ci.width,
            Height: ci.height,
            MipLevels: ci.mip_count.max(1),
            ArraySize: ci.array_size.max(1) * ci.face_count,
            Format: texture_format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: ci.sample_count.max(1), Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: bind,
            CPUAccessFlags: 0,
            // Real runtimes hand out shareable textures (their compositor lives in
            // another process); apps such as Chrome may rely on that.
            // (MSAA textures cannot be shared.)
            MiscFlags: cube_flag
                | if depth || samples > 1 {
                    0
                } else {
                    (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32
                },
        };
        let mut images = Vec::new();
        for _ in 0..3 {
            let mut tex = None;
            let mut r = unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut tex)) };
            if r.is_err() && desc.MiscFlags & D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0 as u32 != 0 {
                // Cube maps cannot have NT handles; a legacy shared handle still works.
                xlog!("NT-handle shared texture creation failed ({r:?}); retrying with a legacy shared handle");
                desc.MiscFlags = cube_flag | D3D11_RESOURCE_MISC_SHARED.0 as u32;
                r = unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut tex)) };
            }
            if r.is_err() && desc.MiscFlags != cube_flag {
                xlog!("shared texture creation failed ({r:?}); retrying unshared");
                desc.MiscFlags = cube_flag;
                r = unsafe { self.device.CreateTexture2D(&desc, None, Some(&mut tex)) };
            }
            if let Err(e) = r {
                xlog!("CreateTexture2D failed: {e:?} for {desc:?}");
                return Err(xr::Result::ERROR_RUNTIME_FAILURE);
            }
            images.push(tex.unwrap());
        }
        xlog!(
            "swapchain {}x{} array {} mips {} samples {} format {} usage {:?} misc {:#x}",
            ci.width,
            ci.height,
            desc.ArraySize,
            desc.MipLevels,
            desc.SampleDesc.Count,
            format.0,
            usage,
            desc.MiscFlags
        );
        Ok(SwapchainData {
            session,
            images,
            width: ci.width,
            height: ci.height,
            array_size: ci.array_size.max(1),
            faces: ci.face_count,
            mip_count: desc.MipLevels,
            sample_count: desc.SampleDesc.Count,
            format,
            next_index: 0,
            acquired: Default::default(),
            last_released: None,
        })
    }

    fn ensure_compositor(&mut self, eye_w: u32, eye_h: u32, format: DXGI_FORMAT) -> Option<()> {
        if let Some(c) = &self.compositor {
            if c.eye_w == eye_w && c.eye_h == eye_h && c.format == format {
                return Some(());
            }
        }
        self.compositor = None;
        let device5: ID3D11Device5 = match self.device.cast() {
            Ok(d) => d,
            Err(e) => {
                xlog!("the app's device has no D3D11.4 fences: {e:?}");
                return None;
            }
        };
        let ctx4: ID3D11DeviceContext4 = self.context.cast().ok()?;
        let desc = D3D11_TEXTURE2D_DESC {
            Width: eye_w * 2,
            Height: eye_h,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
        };
        unsafe {
            let mut texs = Vec::new();
            let mut handles = Vec::new();
            for _ in 0..RING {
                let mut tex = None;
                if let Err(e) = self.device.CreateTexture2D(&desc, None, Some(&mut tex)) {
                    xlog!("compositor texture failed: {e:?}");
                    return None;
                }
                let tex = tex?;
                let res: IDXGIResource1 = tex.cast().ok()?;
                match res.CreateSharedHandle(None, (DXGI_SHARED_RESOURCE_READ | DXGI_SHARED_RESOURCE_WRITE).0, None) {
                    Ok(h) => handles.push(h),
                    Err(e) => {
                        xlog!("CreateSharedHandle failed: {e:?}");
                        return None;
                    }
                }
                texs.push(tex);
            }
            let mut fence: Option<ID3D11Fence> = None;
            if let Err(e) = device5.CreateFence(0, D3D11_FENCE_FLAG_SHARED, &mut fence) {
                xlog!("CreateFence failed: {e:?}");
                return None;
            }
            let fence = fence?;
            let fence_handle = match fence.CreateSharedHandle(None, GENERIC_ALL.0, None) {
                Ok(h) => h,
                Err(e) => {
                    xlog!("fence CreateSharedHandle failed: {e:?}");
                    return None;
                }
            };
            self.generation += 1;
            xlog!("compositor ring {RING} x {}x{} format {} (generation {})", eye_w * 2, eye_h, format.0, self.generation);
            {
                let mut slot = self.shared.slot.lock().unwrap();
                slot.handles = handles.iter().map(|h| h.0 as usize).collect();
                slot.fence = fence_handle.0 as usize;
                slot.latest = 0;
                slot.latest_value = 0;
                slot.generation = self.generation;
                slot.eye_w = eye_w;
                slot.eye_h = eye_h;
                slot.format = format.0;
            }
            self.compositor =
                Some(Compositor { texs, handles, fence, fence_handle, ctx4, next: 0, value: 0, eye_w, eye_h, format });
        }
        Some(())
    }

    /// A ring of RING shared textures (NT handles) for one quad layer.
    fn make_quad_ring(&mut self, w: u32, h: u32, format: DXGI_FORMAT) -> Option<QuadRing> {
        self.make_ring(w, h, format, 0)
    }

    /// A ring of RING shared textures, bindable as shader resources and `bind`.
    fn make_ring(&mut self, w: u32, h: u32, format: DXGI_FORMAT, bind: u32) -> Option<QuadRing> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: w,
            Height: h,
            MipLevels: 1,
            ArraySize: 1,
            Format: format,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32 | bind,
            CPUAccessFlags: 0,
            MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
        };
        let mut texs = Vec::new();
        let mut handles = Vec::new();
        unsafe {
            for _ in 0..RING {
                let mut tex = None;
                if let Err(e) = self.device.CreateTexture2D(&desc, None, Some(&mut tex)) {
                    xlog!("quad texture failed ({w}x{h}, format {}, bind {bind:#x}): {e:?}", format.0);
                    return None;
                }
                let tex = tex?;
                let res: IDXGIResource1 = tex.cast().ok()?;
                handles.push(res.CreateSharedHandle(None, DXGI_SHARED_RESOURCE_READ.0, None).ok()?);
                texs.push(tex);
            }
        }
        self.generation += 1;
        xlog!("quad layer textures {w}x{h} format {} (generation {})", format.0, self.generation);
        Some(QuadRing { texs, handles, w, h, format, generation: self.generation })
    }

    /// An image to copy from: the source itself, or for MSAA its resolve into
    /// a single-sample texture kept for `slot` (subresource 0).
    fn single_sample(&mut self, slot: usize, src: &EyeSource) -> Option<(ID3D11Texture2D, u32)> {
        if src.sample_count <= 1 {
            return Some((src.texture.clone(), src.subresource));
        }
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { src.texture.GetDesc(&mut desc) };
        if self.resolves.len() <= slot {
            self.resolves.resize_with(slot + 1, || None);
        }
        let fits = self.resolves[slot].as_ref().is_some_and(|t| {
            let mut d = D3D11_TEXTURE2D_DESC::default();
            unsafe { t.GetDesc(&mut d) };
            d.Width == desc.Width && d.Height == desc.Height && d.Format == desc.Format
        });
        if !fits {
            let rd = D3D11_TEXTURE2D_DESC {
                MipLevels: 1,
                ArraySize: 1,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
                ..desc
            };
            let mut t = None;
            if let Err(e) = unsafe { self.device.CreateTexture2D(&rd, None, Some(&mut t)) } {
                xlog!("MSAA resolve texture failed: {e:?}");
                return None;
            }
            self.resolves[slot] = t;
        }
        let dst = self.resolves[slot].clone()?;
        unsafe { self.context.ResolveSubresource(&dst, 0, &src.texture, src.subresource, src.format) };
        Some((dst, 0))
    }

    /// The depth of an image's slice (sample 0 if MSAA) into `dst`
    /// (R32_FLOAT, the whole image's size), through a typeless copy kept for `slot`.
    fn resolve_depth(&mut self, slot: usize, src: &EyeSource, dst: &ID3D11Texture2D) -> Option<()> {
        let (typeless, read) = depth_formats(src.format)?;
        let mut desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { src.texture.GetDesc(&mut desc) };
        if self.msaa_depth.len() <= slot {
            self.msaa_depth.resize_with(slot + 1, || None);
        }
        let fits = self.msaa_depth[slot].as_ref().is_some_and(|t| {
            let mut d = D3D11_TEXTURE2D_DESC::default();
            unsafe { t.GetDesc(&mut d) };
            d.Width == desc.Width && d.Height == desc.Height && d.Format == typeless && d.ArraySize == desc.ArraySize
                && d.SampleDesc.Count == desc.SampleDesc.Count
        });
        if !fits {
            let cd = D3D11_TEXTURE2D_DESC {
                Format: typeless,
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_SHADER_RESOURCE.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
                ..desc
            };
            let mut t = None;
            if let Err(e) = unsafe { self.device.CreateTexture2D(&cd, None, Some(&mut t)) } {
                xlog!("MSAA depth copy texture failed: {e:?}");
                return None;
            }
            self.msaa_depth[slot] = t;
        }
        if self.depth_resolver.is_none() {
            self.depth_resolver = unsafe { DepthResolver::new(&self.device) };
            if self.depth_resolver.is_none() {
                return None;
            }
            xlog!("MSAA depth resolve pass ready");
        }
        let copy = self.msaa_depth[slot].clone()?;
        let r = self.depth_resolver.as_ref()?;
        unsafe {
            self.context.CopyResource(&copy, &src.texture);
            let ms = desc.SampleDesc.Count > 1;
            let srv_desc = if ms {
                D3D11_SHADER_RESOURCE_VIEW_DESC {
                    Format: read,
                    ViewDimension: windows::Win32::Graphics::Direct3D::D3D11_SRV_DIMENSION_TEXTURE2DMSARRAY,
                    Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                        Texture2DMSArray: D3D11_TEX2DMS_ARRAY_SRV { FirstArraySlice: src.subresource, ArraySize: 1 },
                    },
                }
            } else {
                // A single-sample subresource index is mip + slice * mips; depth has one mip.
                D3D11_SHADER_RESOURCE_VIEW_DESC {
                    Format: read,
                    ViewDimension: windows::Win32::Graphics::Direct3D::D3D11_SRV_DIMENSION_TEXTURE2DARRAY,
                    Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                        Texture2DArray: D3D11_TEX2D_ARRAY_SRV {
                            MostDetailedMip: 0,
                            MipLevels: 1,
                            FirstArraySlice: src.subresource / desc.MipLevels.max(1),
                            ArraySize: 1,
                        },
                    },
                }
            };
            let mut srv = None;
            self.device.CreateShaderResourceView(&copy, Some(&srv_desc), Some(&mut srv)).ok()?;
            let mut rtv = None;
            self.device.CreateRenderTargetView(dst, None, Some(&mut rtv)).ok()?;
            let ctx1: ID3D11DeviceContext1 = self.context.cast().ok()?;
            let mut previous = None;
            ctx1.SwapDeviceContextState(&r.state, Some(&mut previous));
            let c = &self.context;
            c.OMSetRenderTargets(Some(&[rtv]), None);
            c.RSSetViewports(Some(&[D3D11_VIEWPORT {
                TopLeftX: 0.0,
                TopLeftY: 0.0,
                Width: desc.Width as f32,
                Height: desc.Height as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            }]));
            c.IASetPrimitiveTopology(windows::Win32::Graphics::Direct3D::D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
            c.VSSetShader(&r.vs, None);
            let shader = match (ms, self.depth_flipped) {
                (true, false) => &r.ps,
                (false, false) => &r.ps1,
                (true, true) => &r.ps_flip,
                (false, true) => &r.ps1_flip,
            };
            c.PSSetShader(shader, None);
            c.PSSetShaderResources(0, Some(&[srv]));
            c.Draw(3, 0);
            c.PSSetShaderResources(0, Some(&[None]));
            c.OMSetRenderTargets(None, None);
            if let Some(p) = previous {
                ctx1.SwapDeviceContextState(&p, None);
            }
        }
        Some(())
    }

    /// Copy the eyes of the projection layer and each quad layer's image into
    /// the shared textures, signal the fence, and tell the window where things are.
    pub fn submit(
        &mut self,
        eyes: &[EyeSource],
        views: &[(xrmimic_core::math::Pose, xrmimic_core::math::Fov)],
        quads: &[(EyeSource, crate::shared::QuadMeta)],
        depths: &[(usize, EyeSource, crate::shared::DepthInfo)],
        frame: u64,
    ) {
        if eyes.is_empty() {
            return;
        }
        let flipped = self.depth_flipped;
        // Depth rings: typeless textures the size of the app's whole depth
        // image (a depth copy must take the whole subresource).
        // MSAA depth images: sample 0 drawn into R32_FLOAT rings instead.
        for (i, (_, src, _)) in depths.iter().enumerate() {
            let Some((typeless, _)) = depth_formats(src.format) else { continue };
            let msaa = depth_drawn(src.format, src.sample_count, flipped);
            let ring_format = if msaa { DXGI_FORMAT_R32_FLOAT } else { typeless };
            let mut desc = D3D11_TEXTURE2D_DESC::default();
            unsafe { src.texture.GetDesc(&mut desc) };
            let fits = self
                .depth_rings
                .get(i)
                .and_then(|r| r.as_ref())
                .is_some_and(|r| r.w == desc.Width && r.h == desc.Height && r.format == ring_format);
            if !fits {
                let bind = if msaa { D3D11_BIND_RENDER_TARGET.0 as u32 } else { 0 };
                let ring = self.make_ring(desc.Width, desc.Height, ring_format, bind);
                if self.depth_rings.len() <= i {
                    self.depth_rings.resize_with(i + 1, || None);
                }
                self.depth_rings[i] = ring;
            }
        }
        // Quad rings follow their layers' sizes and formats. A cube layer's
        // six faces go side by side in one texture, +X -X +Y -Y +Z -Z.
        let faces = |m: &crate::shared::QuadMeta| if matches!(m.shape, crate::shared::Shape::Cube) { 6 } else { 1 };
        for (i, (src, meta)) in quads.iter().enumerate() {
            let w = src.rect.extent.width.max(1) as u32 * faces(meta);
            let h = src.rect.extent.height.max(1) as u32;
            let fits = self.quad_rings.get(i).is_some_and(|r| r.w == w && r.h == h && r.format == src.format);
            if !fits {
                let Some(ring) = self.make_quad_ring(w, h, src.format) else { return };
                if i < self.quad_rings.len() {
                    self.quad_rings[i] = ring;
                } else {
                    self.quad_rings.push(ring);
                }
            }
        }
        // MSAA colour images resolve first; everything below copies from these.
        let eye_src: Vec<Option<(ID3D11Texture2D, u32)>> =
            eyes.iter().enumerate().take(2).map(|(i, e)| self.single_sample(i, e)).collect();
        let quad_src: Vec<Option<(ID3D11Texture2D, u32)>> =
            quads.iter().enumerate().map(|(i, (e, _))| self.single_sample(2 + i, e)).collect();
        let eye_w = eyes.iter().map(|e| e.rect.extent.width.max(1) as u32).max().unwrap();
        let eye_h = eyes.iter().map(|e| e.rect.extent.height.max(1) as u32).max().unwrap();
        let format = eyes[0].format;
        if self.ensure_compositor(eye_w, eye_h, format).is_none() {
            return;
        }
        let k = self.compositor.as_ref().unwrap().next;
        // MSAA depth: sample 0 into this frame's ring texture.
        let mut resolved_depth = vec![false; depths.len()];
        for (i, (_, src, _)) in depths.iter().enumerate() {
            if depth_drawn(src.format, src.sample_count, flipped) {
                if let Some(Some(r)) = self.depth_rings.get(i) {
                    let dst = r.texs[k].clone();
                    resolved_depth[i] = self.resolve_depth(i, src, &dst).is_some();
                }
            }
        }
        let c = self.compositor.as_mut().unwrap();
        unsafe {
            for (i, e) in eyes.iter().enumerate().take(2) {
                if e.format != c.format {
                    continue;
                }
                let r = e.rect;
                let b = D3D11_BOX {
                    left: r.offset.x.max(0) as u32,
                    top: r.offset.y.max(0) as u32,
                    front: 0,
                    right: (r.offset.x + r.extent.width) as u32,
                    bottom: (r.offset.y + r.extent.height) as u32,
                    back: 1,
                };
                let Some(Some((tex, sub))) = eye_src.get(i) else { continue };
                self.context.CopySubresourceRegion(&c.texs[k], 0, i as u32 * c.eye_w, 0, 0, tex, *sub, Some(&b));
            }
            for (i, (src, meta)) in quads.iter().enumerate() {
                let Some(Some((tex, sub))) = quad_src.get(i) else { continue };
                for f in 0..faces(meta) {
                let r = src.rect;
                let b = D3D11_BOX {
                    left: r.offset.x.max(0) as u32,
                    top: r.offset.y.max(0) as u32,
                    front: 0,
                    right: (r.offset.x + r.extent.width) as u32,
                    bottom: (r.offset.y + r.extent.height) as u32,
                    back: 1,
                };
                self.context.CopySubresourceRegion(
                    &self.quad_rings[i].texs[k],
                    0,
                    f * r.extent.width as u32,
                    0,
                    0,
                    tex,
                    sub + f * src.mips,
                    Some(&b),
                );
                }
            }
            for (i, (_, src, _)) in depths.iter().enumerate() {
                if let Some(Some(r)) = self.depth_rings.get(i) {
                    if !depth_drawn(src.format, src.sample_count, flipped) && depth_formats(src.format).is_some() {
                        // (MSAA and D32S8 ones were drawn in above.)
                        self.context.CopySubresourceRegion(&r.texs[k], 0, 0, 0, 0, &src.texture, src.subresource, None);
                    }
                }
            }
            if let Some((path, at)) = &self.capture {
                static TAKEN: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
                // Once per process: the first session's frame is the one tests check.
                if frame == *at && !TAKEN.swap(true, std::sync::atomic::Ordering::SeqCst) {
                    match capture_bmp(&self.device, &self.context, &c.texs[k], path) {
                        Ok(()) => xlog!("captured frame {frame} to {}", path.display()),
                        Err(e) => xlog!("capture failed: {e}"),
                    }
                }
            }
            c.value += 1;
            let _ = c.ctx4.Signal(&c.fence, c.value);
            // Submit now so the window's GPU wait on this value is not left
            // waiting for the app's next flush.
            self.context.Flush();
        }
        c.next = (k + 1) % RING;
        let mut slot = self.shared.slot.lock().unwrap();
        slot.latest = k;
        slot.latest_value = c.value;
        slot.frames += 1;
        slot.views = views.to_vec();
        slot.quads = quads
            .iter()
            .enumerate()
            .map(|(i, (_, meta))| QuadSlot {
                handles: self.quad_rings[i].handles.iter().map(|h| h.0 as usize).collect(),
                generation: self.quad_rings[i].generation,
                meta: *meta,
            })
            .collect();
        slot.depth = depths
            .iter()
            .enumerate()
            .filter_map(|(i, (eye, src, info))| {
                let r = self.depth_rings.get(i)?.as_ref()?;
                let (_, read) = depth_formats(src.format)?;
                let msaa = depth_drawn(src.format, src.sample_count, flipped);
                (!msaa || resolved_depth[i]).then(|| DepthSlot {
                    eye: *eye,
                    handles: r.handles.iter().map(|h| h.0 as usize).collect(),
                    generation: r.generation,
                    srv_format: if msaa { DXGI_FORMAT_R32_FLOAT.0 } else { read.0 },
                    rect: [src.rect.offset.x, src.rect.offset.y, src.rect.extent.width, src.rect.extent.height],
                    info: *info,
                })
            })
            .collect();
    }
}


unsafe fn capture_bmp(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    tex: &ID3D11Texture2D,
    path: &std::path::Path,
) -> Result<(), String> {
    let mut desc = D3D11_TEXTURE2D_DESC::default();
    tex.GetDesc(&mut desc);
    let bgra = matches!(desc.Format, DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB);
    let rgba = matches!(desc.Format, DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_R8G8B8A8_UNORM_SRGB);
    if !bgra && !rgba {
        return Err(format!("format {} not capturable", desc.Format.0));
    }
    let sdesc = D3D11_TEXTURE2D_DESC {
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
        ..desc
    };
    let mut staging = None;
    device.CreateTexture2D(&sdesc, None, Some(&mut staging)).map_err(|e| e.to_string())?;
    let staging = staging.unwrap();
    context.CopyResource(&staging, tex);
    let mut m = D3D11_MAPPED_SUBRESOURCE::default();
    context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m)).map_err(|e| e.to_string())?;
    let (w, h) = (desc.Width as usize, desc.Height as usize);
    let mut px = vec![0u8; w * h * 4];
    for y in 0..h {
        let src = std::slice::from_raw_parts((m.pData as *const u8).add(y * m.RowPitch as usize), w * 4);
        let dst = &mut px[y * w * 4..(y + 1) * w * 4];
        for x in 0..w {
            let s = &src[x * 4..x * 4 + 4];
            let (b, g, r) = if bgra { (s[0], s[1], s[2]) } else { (s[2], s[1], s[0]) };
            dst[x * 4..x * 4 + 4].copy_from_slice(&[b, g, r, 255]);
        }
    }
    context.Unmap(&staging, 0);
    crate::log::write_bmp(path, w as u32, h as u32, &px).map_err(|e| e.to_string())
}
