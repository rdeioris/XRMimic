//! A native OpenXR app with a small 3D room, to look at xrmimic's views with a
//! real scene: a checkered floor, a ring of pillars, a table with cubes (one
//! spinning), boxes far away, and two things very near the eyes (a small cube
//! 35 cm ahead, a thin rod at 50 cm). Each eye is rendered with its own pose
//! and field of view, and its depth buffer goes to the runtime
//! (XR_KHR_composition_layer_depth) unless `nodepth` is given.
//!
//!   cargo run --release -p xrmimic-runtime --example scene             (runs until the window is closed)
//!   cargo run --release -p xrmimic-runtime --example scene -- centre   (starts in the centre view)
//!   XRMIMIC_TEST_SNAPSHOT=<out.bmp> cargo run ... --example scene -- centre [nodepth]
//!
//! It also has controller actions (trigger, squeeze, stick, A/X, grip pose;
//! bound for Touch and the simple controller): each controller is drawn at its
//! grip, red as the trigger is pulled, bigger as it is squeezed, green while
//! A/X is pressed, with a white marker the stick moves.
//!
//!   XRMIMIC_TEST_SNAPSHOT=<out.bmp> cargo run ... --example scene -- input
//!
//! sets inputs through the window's Input tab (XRMIMIC_TEST_INPUT, as clicks and
//! drags would) and checks what the app's actions read.
//!
//! Hands (XR_EXT_hand_tracking, XR_EXT_hand_interaction): when the window's
//! Input tab puts the controllers down (Hands, or the H key) each tracked hand
//! is drawn as its 26 joints, and the pinch and grasp drive the same actions.
//!
//!   XRMIMIC_TEST_SNAPSHOT=<out.bmp> cargo run ... --example scene -- hands
//!
//! starts with hands, pinches the right hand and closes the left through the
//! Input tab, and checks the profile, the actions and the joints.
//!
//! Haptics: pulling a trigger sends that controller a 100 ms pulse
//! (xrApplyHapticFeedback), shown in the window and on a gamepad's motors.
//!
//!   XRMIMIC_TEST_SNAPSHOT=<out.bmp> cargo run ... --example scene -- features
//!
//! checks the haptic calls' results, STAGE's bounds (the play area), and that
//! a controller put down by itself stays put while the head walks towards the
//! boundary (which the window then shows).
//!
//! With XRMIMIC_TEST_SNAPSHOT (still scene, centre view on) it afterwards renders
//! the same scene from a real camera between the eyes and compares it with the
//! window's centre view, which only had the two eye images (and their depth):
//! it prints the share of pixels that differ and writes the reference picture
//! next to the snapshot (`<out>-reference.bmp`).

#[cfg(not(windows))]
fn main() {
    eprintln!("scene needs Windows (D3D11)");
}

#[cfg(windows)]
fn main() {
    let code = unsafe { run() };
    std::process::exit(code);
}

#[cfg(windows)]
const SHADER: &str = r#"
cbuffer Eye : register(b0) { float4x4 vp; float4 light; };
// a = position (xyz) and yaw (w); s = size (xyz) and kind (w: 0 lit, 1 checkered, 2 unlit); c = colour.
struct Inst { float4 a; float4 s; float4 c; };
cbuffer Scene : register(b1) { Inst inst[128]; };
struct O { float4 pos : SV_Position; float3 n : NORMAL; float3 w : TEXCOORD0; float4 c : COLOR0; float kind : TEXCOORD1; };
O vs(uint id : SV_VertexID, uint ii : SV_InstanceID) {
    const float3 N[6] = { float3(1, 0, 0), float3(-1, 0, 0), float3(0, 1, 0), float3(0, -1, 0), float3(0, 0, 1), float3(0, 0, -1) };
    const float2 Q[6] = { float2(-1, -1), float2(1, -1), float2(1, 1), float2(-1, -1), float2(1, 1), float2(-1, 1) };
    float3 n = N[id / 6];
    float3 u = abs(n.y) > 0.5 ? float3(1, 0, 0) : float3(0, 1, 0);
    float3 v = cross(n, u);
    float2 q = Q[id % 6];
    Inst k = inst[ii];
    float3 p = 0.5 * (n + q.x * u + q.y * v) * k.s.xyz;
    float cy = cos(k.a.w), sy = sin(k.a.w);
    p = float3(cy * p.x + sy * p.z, p.y, -sy * p.x + cy * p.z) + k.a.xyz;
    O o;
    o.pos = mul(vp, float4(p, 1));
    o.n = float3(cy * n.x + sy * n.z, n.y, -sy * n.x + cy * n.z);
    o.w = p;
    o.c = k.c;
    o.kind = k.s.w;
    return o;
}
float4 ps(O i) : SV_Target {
    float3 c = i.c.rgb;
    if (i.kind > 0.5 && i.kind < 1.5) {
        float k = abs(fmod(floor(i.w.x) + floor(i.w.z), 2.0));
        c = k > 0.5 ? c : c * 0.6;
    }
    float d = saturate(dot(normalize(i.n), normalize(light.xyz)));
    float s = i.kind > 1.5 ? 1 : 0.45 + 0.55 * d;
    return float4(c * s, 1);
}
"#;

#[cfg(windows)]
unsafe fn run() -> i32 {
    use openxr_sys as xr;
    use std::ffi::c_char;
    use windows::core::{Interface, PCSTR};
    use windows::Win32::Foundation::{HMODULE, LUID};
    use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
    use windows::Win32::Graphics::Direct3D::*;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::Common::*;
    use windows::Win32::Graphics::Dxgi::*;
    use xr::Handle as _;
    use xrmimic_core::math::{centre_eye, mat_mul, projection_from_fov, Fov, Pose, Quat, Vec3};

    let snapshot = std::env::var_os("XRMIMIC_TEST_SNAPSHOT").map(std::path::PathBuf::from);
    let args: Vec<String> = std::env::args().collect();
    let send_depth = !args.iter().any(|a| a == "nodepth");
    let input_mode = args.iter().any(|a| a == "input");
    let hands_mode = args.iter().any(|a| a == "hands");
    let features_mode = args.iter().any(|a| a == "features");
    // Where the features test puts the right controller (STAGE).
    const PUT: [f32; 3] = [0.3, 1.0, -0.5];
    if features_mode {
        std::env::set_var("XRMIMIC_TEST_MOVE", "0.5");
        std::env::set_var("XRMIMIC_TEST_HAND_PUT", format!("{},{},{}", PUT[0], PUT[1], PUT[2]));
    }
    if hands_mode {
        std::env::set_var("XRMIMIC_HANDS", "1");
        if std::env::var_os("XRMIMIC_TEST_INPUT").is_none() {
            std::env::set_var("XRMIMIC_TEST_INPUT", "right pinch_ext/value=1; left grasp_ext/value=1");
        }
        std::env::set_var("XRMIMIC_TEST_TAB", "input");
    } else if input_mode {
        // Set through the Input tab, which the window opens on.
        const SPEC: &str = "right trigger/value=0.7; right a/click=1; left thumbstick=0.5,-0.25; left squeeze/value=1";
        if std::env::var_os("XRMIMIC_TEST_INPUT").is_none() {
            std::env::set_var("XRMIMIC_TEST_INPUT", SPEC);
        }
        std::env::set_var("XRMIMIC_TEST_TAB", "input");
    } else if features_mode {
        std::env::set_var("XRMIMIC_TEST_TAB", "input");
    } else if args.iter().any(|a| a == "centre") || snapshot.is_some() {
        std::env::set_var("XRMIMIC_TEST_CENTRE_VIEW", "1");
    }
    if let Some(s) = &snapshot {
        let _ = std::fs::remove_file(s);
    }
    if snapshot.is_none() && std::env::var_os("XRMIMIC_TOPMOST").is_none() {
        std::env::set_var("XRMIMIC_TOPMOST", "1");
    }
    const NEAR: f32 = 0.05;
    const FAR: f32 = 100.0;

    // ------------------------------------------------ loader negotiation
    let info = xr::NegotiateLoaderInfo {
        struct_type: xr::NegotiateLoaderInfo::TYPE,
        struct_version: xr::NegotiateLoaderInfo::VERSION,
        struct_size: std::mem::size_of::<xr::NegotiateLoaderInfo>(),
        min_interface_version: 1,
        max_interface_version: 1,
        min_api_version: xr::Version::new(1, 0, 0),
        max_api_version: xr::Version::new(1, 0x3ff, 0xfff),
    };
    let mut req = xr::NegotiateRuntimeRequest {
        struct_type: xr::NegotiateRuntimeRequest::TYPE,
        struct_version: xr::NegotiateRuntimeRequest::VERSION,
        struct_size: std::mem::size_of::<xr::NegotiateRuntimeRequest>(),
        runtime_interface_version: 0,
        runtime_api_version: xr::Version::new(0, 0, 0),
        get_instance_proc_addr: None,
    };
    assert_eq!(xrmimic_runtime::xrNegotiateLoaderRuntimeInterface(&info, &mut req), xr::Result::SUCCESS);
    let gipa = req.get_instance_proc_addr.unwrap();
    macro_rules! func {
        ($inst:expr, $name:literal, $ty:ty) => {{
            let mut p = None;
            let r = gipa($inst, concat!($name, "\0").as_ptr() as *const c_char, &mut p);
            assert_eq!(r, xr::Result::SUCCESS, $name);
            std::mem::transmute::<xr::pfn::VoidFunction, $ty>(p.unwrap())
        }};
    }
    macro_rules! check {
        ($e:expr) => {{
            let r = $e;
            assert!(r.into_raw() >= 0, "{} -> {:?}", stringify!($e), r);
            r
        }};
    }

    // ------------------------------------------------ instance, system, device
    let create_instance = func!(xr::Instance::NULL, "xrCreateInstance", xr::pfn::CreateInstance);
    let exts = [
        c"XR_KHR_D3D11_enable".as_ptr(),
        c"XR_KHR_composition_layer_depth".as_ptr(),
        c"XR_EXT_hand_tracking".as_ptr(),
        c"XR_EXT_hand_interaction".as_ptr(),
    ];
    let mut app = xr::ApplicationInfo {
        application_name: [0; xr::MAX_APPLICATION_NAME_SIZE],
        application_version: 1,
        engine_name: [0; xr::MAX_ENGINE_NAME_SIZE],
        engine_version: 0,
        api_version: xr::Version::new(1, 0, 0),
    };
    for (d, s) in app.application_name.iter_mut().zip(b"XRMimic scene") {
        *d = *s as c_char;
    }
    let ci = xr::InstanceCreateInfo {
        ty: xr::InstanceCreateInfo::TYPE,
        next: std::ptr::null(),
        create_flags: xr::InstanceCreateFlags::EMPTY,
        application_info: app,
        enabled_api_layer_count: 0,
        enabled_api_layer_names: std::ptr::null(),
        enabled_extension_count: exts.len() as u32,
        enabled_extension_names: exts.as_ptr(),
    };
    let mut inst = xr::Instance::NULL;
    check!(create_instance(&ci, &mut inst));
    let get_system = func!(inst, "xrGetSystem", xr::pfn::GetSystem);
    let reqs = func!(inst, "xrGetD3D11GraphicsRequirementsKHR", xr::pfn::GetD3D11GraphicsRequirementsKHR);
    let create_session = func!(inst, "xrCreateSession", xr::pfn::CreateSession);
    let poll_event = func!(inst, "xrPollEvent", xr::pfn::PollEvent);
    let begin_session = func!(inst, "xrBeginSession", xr::pfn::BeginSession);
    let end_session = func!(inst, "xrEndSession", xr::pfn::EndSession);
    let request_exit = func!(inst, "xrRequestExitSession", xr::pfn::RequestExitSession);
    let destroy_session = func!(inst, "xrDestroySession", xr::pfn::DestroySession);
    let destroy_instance = func!(inst, "xrDestroyInstance", xr::pfn::DestroyInstance);
    let create_ref_space = func!(inst, "xrCreateReferenceSpace", xr::pfn::CreateReferenceSpace);
    let enum_views = func!(inst, "xrEnumerateViewConfigurationViews", xr::pfn::EnumerateViewConfigurationViews);
    let create_swapchain = func!(inst, "xrCreateSwapchain", xr::pfn::CreateSwapchain);
    let enum_images = func!(inst, "xrEnumerateSwapchainImages", xr::pfn::EnumerateSwapchainImages);
    let acquire = func!(inst, "xrAcquireSwapchainImage", xr::pfn::AcquireSwapchainImage);
    let wait_image = func!(inst, "xrWaitSwapchainImage", xr::pfn::WaitSwapchainImage);
    let release = func!(inst, "xrReleaseSwapchainImage", xr::pfn::ReleaseSwapchainImage);
    let wait_frame = func!(inst, "xrWaitFrame", xr::pfn::WaitFrame);
    let begin_frame = func!(inst, "xrBeginFrame", xr::pfn::BeginFrame);
    let end_frame = func!(inst, "xrEndFrame", xr::pfn::EndFrame);
    let locate_views = func!(inst, "xrLocateViews", xr::pfn::LocateViews);
    let string_to_path = func!(inst, "xrStringToPath", xr::pfn::StringToPath);
    let create_action_set = func!(inst, "xrCreateActionSet", xr::pfn::CreateActionSet);
    let create_action = func!(inst, "xrCreateAction", xr::pfn::CreateAction);
    let suggest = func!(inst, "xrSuggestInteractionProfileBindings", xr::pfn::SuggestInteractionProfileBindings);
    let attach = func!(inst, "xrAttachSessionActionSets", xr::pfn::AttachSessionActionSets);
    let sync_actions = func!(inst, "xrSyncActions", xr::pfn::SyncActions);
    let get_float = func!(inst, "xrGetActionStateFloat", xr::pfn::GetActionStateFloat);
    let get_bool = func!(inst, "xrGetActionStateBoolean", xr::pfn::GetActionStateBoolean);
    let get_vec2 = func!(inst, "xrGetActionStateVector2f", xr::pfn::GetActionStateVector2f);
    let create_action_space = func!(inst, "xrCreateActionSpace", xr::pfn::CreateActionSpace);
    let locate_space = func!(inst, "xrLocateSpace", xr::pfn::LocateSpace);
    let create_hand_tracker = func!(inst, "xrCreateHandTrackerEXT", xr::pfn::CreateHandTrackerEXT);
    let locate_joints = func!(inst, "xrLocateHandJointsEXT", xr::pfn::LocateHandJointsEXT);
    let current_profile = func!(inst, "xrGetCurrentInteractionProfile", xr::pfn::GetCurrentInteractionProfile);
    let path_to_string = func!(inst, "xrPathToString", xr::pfn::PathToString);
    let apply_haptic = func!(inst, "xrApplyHapticFeedback", xr::pfn::ApplyHapticFeedback);
    let stop_haptic = func!(inst, "xrStopHapticFeedback", xr::pfn::StopHapticFeedback);
    let bounds_rect = func!(inst, "xrGetReferenceSpaceBoundsRect", xr::pfn::GetReferenceSpaceBoundsRect);

    let sgi = xr::SystemGetInfo {
        ty: xr::SystemGetInfo::TYPE,
        next: std::ptr::null(),
        form_factor: xr::FormFactor::HEAD_MOUNTED_DISPLAY,
    };
    let mut system = xr::SystemId::NULL;
    check!(get_system(inst, &sgi, &mut system));
    let mut gr: xr::GraphicsRequirementsD3D11KHR = std::mem::zeroed();
    gr.ty = xr::GraphicsRequirementsD3D11KHR::TYPE;
    check!(reqs(inst, system, &mut gr));
    let luid = LUID { LowPart: gr.adapter_luid.LowPart, HighPart: gr.adapter_luid.HighPart };
    let factory: IDXGIFactory4 = CreateDXGIFactory1().unwrap();
    let adapter: IDXGIAdapter1 = factory.EnumAdapterByLuid(luid).unwrap();
    let mut device = None;
    let mut context = None;
    D3D11CreateDevice(
        &adapter,
        D3D_DRIVER_TYPE_UNKNOWN,
        HMODULE::default(),
        D3D11_CREATE_DEVICE_FLAG(0),
        Some(&[D3D_FEATURE_LEVEL_11_0]),
        D3D11_SDK_VERSION,
        Some(&mut device),
        None,
        Some(&mut context),
    )
    .unwrap();
    let device: ID3D11Device = device.unwrap();
    let context: ID3D11DeviceContext = context.unwrap();

    // ------------------------------------------------ the scene's pipeline
    let compile = |entry: PCSTR, target: PCSTR| -> Vec<u8> {
        let mut blob = None;
        let mut err = None;
        let r = D3DCompile(
            SHADER.as_ptr() as *const _,
            SHADER.len(),
            PCSTR::null(),
            None,
            None,
            entry,
            target,
            0,
            0,
            &mut blob,
            Some(&mut err),
        );
        if let Err(e) = r {
            let msg = err
                .map(|b| String::from_utf8_lossy(std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize())).into_owned())
                .unwrap_or_default();
            panic!("shader: {e:?} {msg}");
        }
        let b = blob.unwrap();
        std::slice::from_raw_parts(b.GetBufferPointer() as *const u8, b.GetBufferSize()).to_vec()
    };
    let (vs_code, ps_code) = (compile(PCSTR(c"vs".as_ptr() as _), PCSTR(c"vs_5_0".as_ptr() as _)), compile(PCSTR(c"ps".as_ptr() as _), PCSTR(c"ps_5_0".as_ptr() as _)));
    let mut vs = None;
    let mut ps = None;
    device.CreateVertexShader(&vs_code, None, Some(&mut vs)).unwrap();
    device.CreatePixelShader(&ps_code, None, Some(&mut ps)).unwrap();
    let buffer = |bytes: u32| {
        let mut b = None;
        device
            .CreateBuffer(
                &D3D11_BUFFER_DESC {
                    ByteWidth: bytes,
                    Usage: D3D11_USAGE_DYNAMIC,
                    BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                    CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                    ..Default::default()
                },
                None,
                Some(&mut b),
            )
            .unwrap();
        b.unwrap()
    };
    let eye_cb: ID3D11Buffer = buffer(80);
    let scene_cb: ID3D11Buffer = buffer(128 * 48);
    let mut raster = None;
    device
        .CreateRasterizerState(
            &D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                DepthClipEnable: true.into(),
                ..Default::default()
            },
            Some(&mut raster),
        )
        .unwrap();
    let mut depth_state = None;
    device
        .CreateDepthStencilState(
            &D3D11_DEPTH_STENCIL_DESC {
                DepthEnable: true.into(),
                DepthWriteMask: D3D11_DEPTH_WRITE_MASK_ALL,
                DepthFunc: D3D11_COMPARISON_LESS,
                ..Default::default()
            },
            Some(&mut depth_state),
        )
        .unwrap();
    let upload = |buf: &ID3D11Buffer, data: &[f32]| {
        let mut m = D3D11_MAPPED_SUBRESOURCE::default();
        context.Map(buf, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut m)).unwrap();
        std::ptr::copy_nonoverlapping(data.as_ptr(), m.pData as *mut f32, data.len());
        context.Unmap(buf, 0);
    };
    // The scene, in STAGE space (floor at y = 0), around a head at `h` metres.
    let scene = |h: f32, t: f32| -> Vec<[f32; 12]> {
        let mut v = Vec::new();
        let mut add = |p: [f32; 3], yaw: f32, s: [f32; 3], kind: f32, c: [f32; 3]| {
            v.push([p[0], p[1], p[2], yaw, s[0], s[1], s[2], kind, c[0], c[1], c[2], 1.0]);
        };
        add([0.0, -0.005, 0.0], 0.0, [30.0, 0.01, 30.0], 1.0, [0.82, 0.8, 0.74]);
        for i in 0..8 {
            let a = i as f32 * std::f32::consts::TAU / 8.0 + 0.2;
            let hue = i as f32 / 8.0;
            let c = [0.5 + 0.5 * (hue * 6.28).cos(), 0.5 + 0.5 * (hue * 6.28 + 2.1).cos(), 0.5 + 0.5 * (hue * 6.28 + 4.2).cos()];
            add([4.0 * a.sin(), 1.5, -4.0 * a.cos()], a, [0.4, 3.0, 0.4], 0.0, c);
        }
        // A table 1.2 m ahead, with three cubes (the middle one spins).
        add([0.0, 0.74, -1.2], 0.0, [1.2, 0.05, 0.7], 0.0, [0.55, 0.36, 0.2]);
        for (x, z) in [(-0.55, -0.9), (0.55, -0.9), (-0.55, -1.5), (0.55, -1.5)] {
            add([x, 0.36, z], 0.0, [0.05, 0.72, 0.05], 0.0, [0.4, 0.26, 0.15]);
        }
        add([-0.35, 0.845, -1.1], 0.4, [0.16, 0.16, 0.16], 0.0, [0.85, 0.2, 0.2]);
        add([0.0, 0.88, -1.25], t * 0.8, [0.22, 0.22, 0.22], 0.0, [0.95, 0.75, 0.2]);
        add([0.35, 0.82, -1.35], -0.3, [0.12, 0.12, 0.12], 0.0, [0.2, 0.4, 0.9]);
        // Far away: three big boxes.
        add([-5.0, 1.0, -9.0], 0.3, [2.0, 2.0, 2.0], 0.0, [0.6, 0.65, 0.7]);
        add([1.0, 1.5, -11.0], 0.0, [3.0, 3.0, 1.0], 0.0, [0.75, 0.7, 0.62]);
        add([6.0, 0.75, -8.0], -0.5, [1.5, 1.5, 1.5], 0.0, [0.55, 0.7, 0.55]);
        // Very near: a small cube 35 cm ahead, low left; a thin rod at 50 cm, right.
        add([-0.1, h - 0.12, -0.35], 0.5, [0.06, 0.06, 0.06], 2.0, [1.0, 0.9, 0.1]);
        add([0.09, h - 0.05, -0.5], 0.0, [0.012, 0.35, 0.012], 2.0, [0.95, 0.95, 0.95]);
        v
    };
    let light = [0.4f32, 0.9, 0.3, 0.0];
    let sky = [0.62f32, 0.75, 0.9, 1.0];
    let draw = |vp: &[f32; 16], insts: &[[f32; 12]], rtv: &ID3D11RenderTargetView, dsv: &ID3D11DepthStencilView, viewport: D3D11_VIEWPORT| {
        let mut eye = vp.to_vec();
        eye.extend(light);
        upload(&eye_cb, &eye);
        upload(&scene_cb, &insts.concat());
        context.OMSetRenderTargets(Some(&[Some(rtv.clone())]), Some(dsv));
        context.OMSetDepthStencilState(depth_state.as_ref(), 0);
        context.RSSetState(raster.as_ref());
        context.RSSetViewports(Some(&[viewport]));
        context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
        context.VSSetShader(vs.as_ref(), None);
        context.PSSetShader(ps.as_ref(), None);
        context.VSSetConstantBuffers(0, Some(&[Some(eye_cb.clone()), Some(scene_cb.clone())]));
        context.DrawInstanced(36, insts.len() as u32, 0, 0);
    };
    let view_proj = |pose: &Pose, fov: Fov| mat_mul(&projection_from_fov(fov, NEAR, FAR, true), &pose.inverse().to_matrix());

    // ------------------------------------------------ session
    let binding = xr::GraphicsBindingD3D11KHR {
        ty: xr::GraphicsBindingD3D11KHR::TYPE,
        next: std::ptr::null(),
        device: device.as_raw(),
    };
    let sci = xr::SessionCreateInfo {
        ty: xr::SessionCreateInfo::TYPE,
        next: &binding as *const _ as *const _,
        create_flags: xr::SessionCreateFlags::EMPTY,
        system_id: system,
    };
    let mut session = xr::Session::NULL;
    check!(create_session(inst, &sci, &mut session));

    // ------------------------------------------------ controller actions
    let path = |p: &str| {
        let c = std::ffi::CString::new(p).unwrap();
        let mut out = xr::Path::NULL;
        check!(string_to_path(inst, c.as_ptr(), &mut out));
        out
    };
    let fill_name = |dst: &mut [c_char], src: &str| {
        for (d, b) in dst.iter_mut().zip(src.bytes()) {
            *d = b as c_char;
        }
    };
    let mut asci: xr::ActionSetCreateInfo = std::mem::zeroed();
    asci.ty = xr::ActionSetCreateInfo::TYPE;
    fill_name(&mut asci.action_set_name, "scene");
    fill_name(&mut asci.localized_action_set_name, "Scene");
    let mut set = xr::ActionSet::NULL;
    check!(create_action_set(inst, &asci, &mut set));
    let hands = [path("/user/hand/left"), path("/user/hand/right")];
    let action = |name: &str, ty: xr::ActionType| {
        let mut aci: xr::ActionCreateInfo = std::mem::zeroed();
        aci.ty = xr::ActionCreateInfo::TYPE;
        aci.action_type = ty;
        aci.count_subaction_paths = 2;
        aci.subaction_paths = hands.as_ptr();
        fill_name(&mut aci.action_name, name);
        fill_name(&mut aci.localized_action_name, name);
        let mut a = xr::Action::NULL;
        check!(create_action(set, &aci, &mut a));
        a
    };
    let trigger = action("trigger", xr::ActionType::FLOAT_INPUT);
    let squeeze = action("squeeze", xr::ActionType::FLOAT_INPUT);
    let stick = action("stick", xr::ActionType::VECTOR2F_INPUT);
    let primary = action("primary", xr::ActionType::BOOLEAN_INPUT);
    let grip = action("grip", xr::ActionType::POSE_INPUT);
    let buzz = action("buzz", xr::ActionType::VIBRATION_OUTPUT);
    let bind = |a: xr::Action, p: &str| xr::ActionSuggestedBinding { action: a, binding: path(p) };
    let suggest_for = |profile: &str, list: Vec<xr::ActionSuggestedBinding>| {
        let sb = xr::InteractionProfileSuggestedBinding {
            ty: xr::InteractionProfileSuggestedBinding::TYPE,
            next: std::ptr::null(),
            interaction_profile: path(profile),
            count_suggested_bindings: list.len() as u32,
            suggested_bindings: list.as_ptr(),
        };
        check!(suggest(inst, &sb));
    };
    let mut touch = Vec::new();
    for side in ["left", "right"] {
        touch.push(bind(trigger, &format!("/user/hand/{side}/input/trigger/value")));
        touch.push(bind(squeeze, &format!("/user/hand/{side}/input/squeeze/value")));
        touch.push(bind(stick, &format!("/user/hand/{side}/input/thumbstick")));
        touch.push(bind(grip, &format!("/user/hand/{side}/input/grip/pose")));
        touch.push(bind(buzz, &format!("/user/hand/{side}/output/haptic")));
    }
    touch.push(bind(primary, "/user/hand/left/input/x/click"));
    touch.push(bind(primary, "/user/hand/right/input/a/click"));
    suggest_for("/interaction_profiles/oculus/touch_controller", touch);
    let mut simple = Vec::new();
    for side in ["left", "right"] {
        simple.push(bind(trigger, &format!("/user/hand/{side}/input/select/click")));
        simple.push(bind(grip, &format!("/user/hand/{side}/input/grip/pose")));
        simple.push(bind(buzz, &format!("/user/hand/{side}/output/haptic")));
    }
    suggest_for("/interaction_profiles/khr/simple_controller", simple);
    // Hands: the pinch and the grasp drive the trigger and squeeze actions.
    let mut hand = Vec::new();
    for side in ["left", "right"] {
        hand.push(bind(trigger, &format!("/user/hand/{side}/input/pinch_ext/value")));
        hand.push(bind(squeeze, &format!("/user/hand/{side}/input/grasp_ext/value")));
        hand.push(bind(grip, &format!("/user/hand/{side}/input/grip/pose")));
    }
    suggest_for("/interaction_profiles/ext/hand_interaction_ext", hand);
    let sai = xr::SessionActionSetsAttachInfo {
        ty: xr::SessionActionSetsAttachInfo::TYPE,
        next: std::ptr::null(),
        count_action_sets: 1,
        action_sets: &set,
    };
    check!(attach(session, &sai));
    let grip_spaces = [0usize, 1].map(|h| {
        let ci = xr::ActionSpaceCreateInfo {
            ty: xr::ActionSpaceCreateInfo::TYPE,
            next: std::ptr::null(),
            action: grip,
            subaction_path: hands[h],
            pose_in_action_space: xr::Posef {
                orientation: xr::Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
                position: xr::Vector3f { x: 0.0, y: 0.0, z: 0.0 },
            },
        };
        let mut sp = xr::Space::NULL;
        check!(create_action_space(session, &ci, &mut sp));
        sp
    });
    // A hand tracker per hand.
    let trackers = [xr::HandEXT::LEFT, xr::HandEXT::RIGHT].map(|hand| {
        let ci = xr::HandTrackerCreateInfoEXT {
            ty: xr::HandTrackerCreateInfoEXT::TYPE,
            next: std::ptr::null(),
            hand,
            hand_joint_set: xr::HandJointSetEXT::DEFAULT,
        };
        let mut t = xr::HandTrackerEXT::NULL;
        check!(create_hand_tracker(session, &ci, &mut t));
        t
    });
    // What the actions read on each hand: (trigger, squeeze, stick, primary).
    let read_hand = |h: usize| {
        let gi = |a: xr::Action| xr::ActionStateGetInfo {
            ty: xr::ActionStateGetInfo::TYPE,
            next: std::ptr::null(),
            action: a,
            subaction_path: hands[h],
        };
        let mut f: xr::ActionStateFloat = std::mem::zeroed();
        f.ty = xr::ActionStateFloat::TYPE;
        let _ = get_float(session, &gi(trigger), &mut f);
        let t = f.current_state;
        let _ = get_float(session, &gi(squeeze), &mut f);
        let sq = f.current_state;
        let mut v: xr::ActionStateVector2f = std::mem::zeroed();
        v.ty = xr::ActionStateVector2f::TYPE;
        let _ = get_vec2(session, &gi(stick), &mut v);
        let mut b: xr::ActionStateBoolean = std::mem::zeroed();
        b.ty = xr::ActionStateBoolean::TYPE;
        let _ = get_bool(session, &gi(primary), &mut b);
        (t, sq, [v.current_state.x, v.current_state.y], b.current_state.into())
    };
    // The newest session state change, if any.
    let poll_state = || -> Option<xr::SessionState> {
        let mut last = None;
        loop {
            let mut buf: xr::EventDataBuffer = std::mem::zeroed();
            buf.ty = xr::EventDataBuffer::TYPE;
            if poll_event(inst, &mut buf) != xr::Result::SUCCESS {
                return last;
            }
            if buf.ty == xr::StructureType::EVENT_DATA_SESSION_STATE_CHANGED {
                last = Some((*(&buf as *const _ as *const xr::EventDataSessionStateChanged)).state);
            }
        }
    };
    let wait_state = |want: xr::SessionState| {
        for _ in 0..1000 {
            if poll_state() == Some(want) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("no {want:?} event");
    };
    wait_state(xr::SessionState::READY);
    let sbi = xr::SessionBeginInfo {
        ty: xr::SessionBeginInfo::TYPE,
        next: std::ptr::null(),
        primary_view_configuration_type: xr::ViewConfigurationType::PRIMARY_STEREO,
    };
    check!(begin_session(session, &sbi));
    let rsci = xr::ReferenceSpaceCreateInfo {
        ty: xr::ReferenceSpaceCreateInfo::TYPE,
        next: std::ptr::null(),
        reference_space_type: xr::ReferenceSpaceType::STAGE,
        pose_in_reference_space: xr::Posef {
            orientation: xr::Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
            position: xr::Vector3f { x: 0.0, y: 0.0, z: 0.0 },
        },
    };
    let mut stage = xr::Space::NULL;
    check!(create_ref_space(session, &rsci, &mut stage));
    // The play area: STAGE's bounds; LOCAL has none.
    let mut stage_bounds = xr::Extent2Df { width: 0.0, height: 0.0 };
    let stage_result = bounds_rect(session, xr::ReferenceSpaceType::STAGE, &mut stage_bounds);
    let mut no_bounds = xr::Extent2Df { width: 0.0, height: 0.0 };
    let local_result = bounds_rect(session, xr::ReferenceSpaceType::LOCAL, &mut no_bounds);
    // A pulse on a hand, or on both (None).
    let pulse = |action: xr::Action, hand: Option<usize>, amplitude: f32, ms: i64| {
        let info = xr::HapticActionInfo {
            ty: xr::HapticActionInfo::TYPE,
            next: std::ptr::null(),
            action,
            subaction_path: hand.map(|h| hands[h]).unwrap_or(xr::Path::NULL),
        };
        let v = xr::HapticVibration {
            ty: xr::HapticVibration::TYPE,
            next: std::ptr::null(),
            duration: xr::Duration::from_nanos(ms * 1_000_000),
            frequency: 160.0,
            amplitude,
        };
        apply_haptic(session, &info, &v as *const _ as *const xr::HapticBaseHeader)
    };
    let mut haptic_results: Vec<(&str, xr::Result, xr::Result)> = Vec::new();
    let mut grips: [Option<Vec3>; 2] = [None, None];
    let mut first_left: Option<Vec3> = None;

    // ------------------------------------------------ swapchains: colour and depth, both eyes side by side
    let mut count = 0;
    let mut vcv: [xr::ViewConfigurationView; 2] = std::mem::zeroed();
    for v in &mut vcv {
        v.ty = xr::ViewConfigurationView::TYPE;
    }
    check!(enum_views(inst, system, xr::ViewConfigurationType::PRIMARY_STEREO, 2, &mut count, vcv.as_mut_ptr()));
    let (eye_w, eye_h) = (vcv[0].recommended_image_rect_width, vcv[0].recommended_image_rect_height);
    let make = |format: i64, usage: xr::SwapchainUsageFlags| -> (xr::Swapchain, Vec<ID3D11Texture2D>) {
        let ci = xr::SwapchainCreateInfo {
            ty: xr::SwapchainCreateInfo::TYPE,
            next: std::ptr::null(),
            create_flags: xr::SwapchainCreateFlags::EMPTY,
            usage_flags: usage,
            format,
            sample_count: 1,
            width: eye_w * 2,
            height: eye_h,
            face_count: 1,
            array_size: 1,
            mip_count: 1,
        };
        let mut sc = xr::Swapchain::NULL;
        check!(create_swapchain(session, &ci, &mut sc));
        let mut n = 0;
        check!(enum_images(sc, 0, &mut n, std::ptr::null_mut()));
        let mut imgs: Vec<xr::SwapchainImageD3D11KHR> = (0..n)
            .map(|_| xr::SwapchainImageD3D11KHR {
                ty: xr::SwapchainImageD3D11KHR::TYPE,
                next: std::ptr::null_mut(),
                texture: std::ptr::null_mut(),
            })
            .collect();
        check!(enum_images(sc, n, &mut n, imgs.as_mut_ptr() as *mut xr::SwapchainImageBaseHeader));
        (sc, imgs.iter().map(|i| ID3D11Texture2D::from_raw_borrowed(&i.texture).unwrap().clone()).collect())
    };
    let (color_sc, color_tex) = make(28, xr::SwapchainUsageFlags::COLOR_ATTACHMENT); // R8G8B8A8_UNORM
    let (depth_sc, depth_tex) = make(40, xr::SwapchainUsageFlags::DEPTH_STENCIL_ATTACHMENT); // D32_FLOAT
    let rtvs: Vec<ID3D11RenderTargetView> = color_tex
        .iter()
        .map(|t| {
            let mut v = None;
            device.CreateRenderTargetView(t, None, Some(&mut v)).unwrap();
            v.unwrap()
        })
        .collect();
    let dsvs: Vec<ID3D11DepthStencilView> = depth_tex
        .iter()
        .map(|t| {
            let mut v = None;
            device.CreateDepthStencilView(t, None, Some(&mut v)).unwrap();
            v.unwrap()
        })
        .collect();
    let acquire_wait = |sc: xr::Swapchain| -> usize {
        let mut idx = 0;
        check!(acquire(sc, std::ptr::null(), &mut idx));
        let wi = xr::SwapchainImageWaitInfo {
            ty: xr::SwapchainImageWaitInfo::TYPE,
            next: std::ptr::null(),
            timeout: xr::Duration::INFINITE,
        };
        check!(wait_image(sc, &wi));
        idx as usize
    };

    // ------------------------------------------------ frames
    let to_pose = |p: &xr::Posef| {
        let o = p.orientation;
        Pose::new(Quat { x: o.x, y: o.y, z: o.z, w: o.w }, Vec3::new(p.position.x, p.position.y, p.position.z))
    };
    let to_fov = |f: &xr::Fovf| Fov { left: f.angle_left, right: f.angle_right, up: f.angle_up, down: f.angle_down };
    let start = std::time::Instant::now();
    let mut head_h: Option<f32> = None;
    let mut last: Option<[(Pose, Fov); 2]> = None;
    let mut frames = 0u32;
    let mut shot_seen: Option<u32> = None;
    let mut stopping = false;
    let mut states: [(f32, f32, [f32; 2], bool); 2] = [(0.0, 0.0, [0.0; 2], false); 2];
    let mut controllers: Vec<[f32; 12]> = Vec::new();
    // Each hand's joints (STAGE space) when tracked.
    let mut joints: [Option<Vec<(Vec3, f32)>>; 2] = [None, None];
    let mut hand_profile_seen: Option<String> = None;
    loop {
        match poll_state() {
            Some(xr::SessionState::STOPPING) => {
                stopping = true;
                break;
            }
            Some(xr::SessionState::EXITING | xr::SessionState::LOSS_PENDING) => break,
            _ => {}
        }
        // Tests: run until the window has photographed itself (it starts
        // later on a busy machine), then half a second more so the app reads
        // what the window set; at least 400 frames, at most a minute.
        if let Some(path) = &snapshot {
            if path.exists() {
                shot_seen.get_or_insert(frames);
            }
            if (frames >= 400 && shot_seen.is_some_and(|f| frames >= f + 45)) || frames >= 90 * 60 {
                break;
            }
        }
        frames += 1;
        let mut fs: xr::FrameState = std::mem::zeroed();
        fs.ty = xr::FrameState::TYPE;
        check!(wait_frame(session, std::ptr::null(), &mut fs));
        check!(begin_frame(session, std::ptr::null()));
        let vli = xr::ViewLocateInfo {
            ty: xr::ViewLocateInfo::TYPE,
            next: std::ptr::null(),
            view_configuration_type: xr::ViewConfigurationType::PRIMARY_STEREO,
            display_time: fs.predicted_display_time,
            space: stage,
        };
        let mut vs_state: xr::ViewState = std::mem::zeroed();
        vs_state.ty = xr::ViewState::TYPE;
        let mut views: [xr::View; 2] = std::mem::zeroed();
        for v in &mut views {
            v.ty = xr::View::TYPE;
        }
        let mut n = 0;
        check!(locate_views(session, &vli, &mut vs_state, 2, &mut n, views.as_mut_ptr()));
        let eyes = [(to_pose(&views[0].pose), to_fov(&views[0].fov)), (to_pose(&views[1].pose), to_fov(&views[1].fov))];
        let h = *head_h.get_or_insert(eyes[0].0.position.y);
        // Still in tests, so the window's picture and the reference match.
        let t = if snapshot.is_some() { 0.0 } else { start.elapsed().as_secs_f32() };
        // Controllers: read the actions, draw each at its grip.
        let active = xr::ActiveActionSet { action_set: set, subaction_path: xr::Path::NULL };
        let si = xr::ActionsSyncInfo {
            ty: xr::ActionsSyncInfo::TYPE,
            next: std::ptr::null(),
            count_active_action_sets: 1,
            active_action_sets: &active,
        };
        let _ = sync_actions(session, &si);
        controllers.clear();
        for hand in 0..2 {
            let mut locs: [xr::HandJointLocationEXT; 26] = std::mem::zeroed();
            let mut hl = xr::HandJointLocationsEXT {
                ty: xr::HandJointLocationsEXT::TYPE,
                next: std::ptr::null_mut(),
                is_active: xr::FALSE,
                joint_count: 26,
                joint_locations: locs.as_mut_ptr(),
            };
            let li = xr::HandJointsLocateInfoEXT {
                ty: xr::HandJointsLocateInfoEXT::TYPE,
                next: std::ptr::null(),
                base_space: stage,
                time: fs.predicted_display_time,
            };
            let _ = locate_joints(trackers[hand], &li, &mut hl);
            joints[hand] = (hl.is_active == xr::TRUE).then(|| {
                locs.iter().map(|l| (Vec3::new(l.pose.position.x, l.pose.position.y, l.pose.position.z), l.radius)).collect()
            });
        }
        if features_mode && frames == 120 {
            haptic_results.push(("right pulse", pulse(buzz, Some(1), 0.8, 100), xr::Result::SUCCESS));
            haptic_results.push(("pulse on an input action", pulse(trigger, Some(1), 0.8, 100), xr::Result::ERROR_ACTION_TYPE_MISMATCH));
            let info = xr::HapticActionInfo {
                ty: xr::HapticActionInfo::TYPE,
                next: std::ptr::null(),
                action: buzz,
                subaction_path: xr::Path::NULL,
            };
            haptic_results.push(("stop both", stop_haptic(session, &info), xr::Result::SUCCESS));
            haptic_results.push(("left pulse", pulse(buzz, Some(0), 0.4, 300), xr::Result::SUCCESS));
        }
        for hand in 0..2 {
            let was = states[hand].0;
            states[hand] = read_hand(hand);
            // A trigger pulled: a pulse on that controller.
            if was < 0.5 && states[hand].0 >= 0.5 {
                let _ = pulse(buzz, Some(hand), 0.8, 100);
            }
            // A tracked hand is drawn as its joints, below, not as a controller.
            if let Some(js) = &joints[hand] {
                let skin = if hand == 0 { [0.93, 0.74, 0.6] } else { [0.88, 0.68, 0.55] };
                for (k, (p, r)) in js.iter().enumerate() {
                    let tip = matches!(k, 5 | 10 | 15 | 20 | 25);
                    let c = if tip { [0.75, 0.35, 0.3] } else { skin };
                    controllers.push([p.x, p.y, p.z, 0.0, r * 1.6, r * 1.6, r * 1.6, 0.0, c[0], c[1], c[2], 1.0]);
                }
                continue;
            }
            let (tr, sq, st, pr) = states[hand];
            let mut loc: xr::SpaceLocation = std::mem::zeroed();
            loc.ty = xr::SpaceLocation::TYPE;
            let _ = locate_space(grip_spaces[hand], stage, fs.predicted_display_time, &mut loc);
            if !loc.location_flags.contains(xr::SpaceLocationFlags::POSITION_VALID) {
                continue;
            }
            let (q, p) = (loc.pose.orientation, loc.pose.position);
            grips[hand] = Some(Vec3::new(p.x, p.y, p.z));
            if hand == 0 {
                first_left.get_or_insert(Vec3::new(p.x, p.y, p.z));
            }
            let yaw = (2.0 * (q.w * q.y + q.x * q.z)).atan2(1.0 - 2.0 * (q.x * q.x + q.y * q.y));
            let k = 1.0 + 0.5 * sq;
            let colour = if pr { [0.2, 0.85, 0.3] } else { [0.6 + 0.4 * tr, 0.6 * (1.0 - tr), 0.6 * (1.0 - tr)] };
            controllers.push([p.x, p.y, p.z, yaw, 0.05 * k, 0.04 * k, 0.12 * k, 0.0, colour[0], colour[1], colour[2], 1.0]);
            let (c, s_) = (yaw.cos(), yaw.sin());
            let (mx, mz) = (st[0] * 0.04, -st[1] * 0.04);
            let m = [p.x + c * mx + s_ * mz, p.y + 0.04 * k, p.z - s_ * mx + c * mz];
            controllers.push([m[0], m[1], m[2], 0.0, 0.015, 0.015, 0.015, 2.0, 1.0, 1.0, 1.0, 1.0]);
        }
        let mut ps = xr::InteractionProfileState {
            ty: xr::InteractionProfileState::TYPE,
            next: std::ptr::null_mut(),
            interaction_profile: xr::Path::NULL,
        };
        if current_profile(session, hands[1], &mut ps) == xr::Result::SUCCESS && ps.interaction_profile != xr::Path::NULL {
            let mut buf = [0 as c_char; 256];
            let mut n = 0;
            if path_to_string(inst, ps.interaction_profile, 256, &mut n, buf.as_mut_ptr()) == xr::Result::SUCCESS {
                hand_profile_seen = Some(std::ffi::CStr::from_ptr(buf.as_ptr()).to_string_lossy().into_owned());
            }
        }
        let mut insts = scene(h, t);
        insts.extend(controllers.iter().copied());
        let ci = acquire_wait(color_sc);
        let di = acquire_wait(depth_sc);
        context.ClearRenderTargetView(&rtvs[ci], &sky);
        context.ClearDepthStencilView(&dsvs[di], D3D11_CLEAR_DEPTH.0 as u32, 1.0, 0);
        for (e, (pose, fov)) in eyes.iter().enumerate() {
            let viewport = D3D11_VIEWPORT {
                TopLeftX: (e as u32 * eye_w) as f32,
                TopLeftY: 0.0,
                Width: eye_w as f32,
                Height: eye_h as f32,
                MinDepth: 0.0,
                MaxDepth: 1.0,
            };
            draw(&view_proj(pose, *fov), &insts, &rtvs[ci], &dsvs[di], viewport);
        }
        context.OMSetRenderTargets(None, None);
        check!(release(color_sc, std::ptr::null()));
        check!(release(depth_sc, std::ptr::null()));
        last = Some(eyes);
        let rect = |e: u32| xr::Rect2Di {
            offset: xr::Offset2Di { x: (e * eye_w) as i32, y: 0 },
            extent: xr::Extent2Di { width: eye_w as i32, height: eye_h as i32 },
        };
        let depth_infos: Vec<xr::CompositionLayerDepthInfoKHR> = (0..2)
            .map(|e| xr::CompositionLayerDepthInfoKHR {
                ty: xr::CompositionLayerDepthInfoKHR::TYPE,
                next: std::ptr::null(),
                sub_image: xr::SwapchainSubImage { swapchain: depth_sc, image_rect: rect(e), image_array_index: 0 },
                min_depth: 0.0,
                max_depth: 1.0,
                near_z: NEAR,
                far_z: FAR,
            })
            .collect();
        let pviews: Vec<xr::CompositionLayerProjectionView> = (0..2)
            .map(|e| xr::CompositionLayerProjectionView {
                ty: xr::CompositionLayerProjectionView::TYPE,
                next: if send_depth { &depth_infos[e] as *const _ as *const std::ffi::c_void } else { std::ptr::null() },
                pose: views[e].pose,
                fov: views[e].fov,
                sub_image: xr::SwapchainSubImage { swapchain: color_sc, image_rect: rect(e as u32), image_array_index: 0 },
            })
            .collect();
        let proj = xr::CompositionLayerProjection {
            ty: xr::CompositionLayerProjection::TYPE,
            next: std::ptr::null(),
            layer_flags: xr::CompositionLayerFlags::EMPTY,
            space: stage,
            view_count: 2,
            views: pviews.as_ptr(),
        };
        let layers: [*const xr::CompositionLayerBaseHeader; 1] = [&proj as *const _ as *const _];
        let fei = xr::FrameEndInfo {
            ty: xr::FrameEndInfo::TYPE,
            next: std::ptr::null(),
            display_time: fs.predicted_display_time,
            environment_blend_mode: xr::EnvironmentBlendMode::OPAQUE,
            layer_count: 1,
            layers: layers.as_ptr(),
        };
        check!(end_frame(session, &fei));
    }
    if !stopping {
        let _ = request_exit(session);
        wait_state(xr::SessionState::STOPPING);
    }
    check!(end_session(session));
    check!(destroy_session(session));
    check!(destroy_instance(inst));

    // ------------------------------------------------ hands: profile, actions, joints
    if hands_mode {
        let mut ps = xr::InteractionProfileState {
            ty: xr::InteractionProfileState::TYPE,
            next: std::ptr::null_mut(),
            interaction_profile: xr::Path::NULL,
        };
        // (Asked before the session ended: kept from the last frame below.)
        let _ = &mut ps;
        let d = |a: Vec3, b: Vec3| (a - b).length();
        let j = |h: usize, k: usize| joints[h].as_ref().map(|v| v[k].0);
        let right_pinch = j(1, 5).zip(j(1, 10)).map(|(a, b)| d(a, b));
        let left_fist = j(0, 15).zip(j(0, 0)).map(|(a, b)| d(a, b));
        let right_open = j(1, 15).zip(j(1, 0)).map(|(a, b)| d(a, b));
        let ok_list = [
            ("profile", hand_profile_seen.as_deref() == Some("/interaction_profiles/ext/hand_interaction_ext"), format!("{hand_profile_seen:?}"), "hand_interaction_ext".to_string()),
            ("both hands tracked", joints[0].is_some() && joints[1].is_some(), format!("{} {}", joints[0].is_some(), joints[1].is_some()), "true true".into()),
            ("right pinch -> trigger action", (states[1].0 - 1.0).abs() < 0.011, format!("{:.2}", states[1].0), "1.00".into()),
            ("left grasp -> squeeze action", (states[0].1 - 1.0).abs() < 0.011, format!("{:.2}", states[0].1), "1.00".into()),
            ("right thumb tip to index tip", right_pinch.is_some_and(|v| v < 0.015), format!("{right_pinch:?}"), "< 0.015 m".into()),
            ("left middle tip to palm (fist)", left_fist.is_some_and(|v| v < 0.06), format!("{left_fist:?}"), "< 0.06 m".into()),
            ("right middle tip to palm (open)", right_open.is_some_and(|v| v > 0.09), format!("{right_open:?}"), "> 0.09 m".into()),
        ];
        let mut ok = true;
        for (what, good, got, want) in &ok_list {
            println!("{} {what}: {got}, want {want}", if *good { "ok  " } else { "FAIL" });
            ok &= good;
        }
        println!("{}", if ok { "PASS" } else { "FAIL" });
        return if ok { 0 } else { 1 };
    }

    // ------------------------------------------------ haptics, play area, a controller put down
    if features_mode {
        let mut ok_list: Vec<(String, bool, String, String)> = Vec::new();
        for (what, got, want) in &haptic_results {
            ok_list.push((format!("haptics: {what}"), got == want, format!("{got:?}"), format!("{want:?}")));
        }
        if haptic_results.is_empty() {
            ok_list.push(("haptics sent".into(), false, "none".into(), "4 calls".into()));
        }
        ok_list.push((
            "STAGE bounds (the play area)".into(),
            stage_result == xr::Result::SUCCESS && stage_bounds.width == 2.5 && stage_bounds.height == 2.5,
            format!("{stage_result:?} {} x {}", stage_bounds.width, stage_bounds.height),
            "SUCCESS 2.5 x 2.5".into(),
        ));
        ok_list.push((
            "LOCAL bounds".into(),
            local_result == xr::Result::SPACE_BOUNDS_UNAVAILABLE,
            format!("{local_result:?}"),
            "SPACE_BOUNDS_UNAVAILABLE".into(),
        ));
        let put = Vec3::new(PUT[0], PUT[1], PUT[2]);
        ok_list.push((
            "right controller stays where it was put".into(),
            grips[1].is_some_and(|g| (g - put).length() < 0.01),
            format!("{:?}", grips[1]),
            format!("{put:?}"),
        ));
        let walked = grips[0].zip(first_left).map(|(a, b)| (a - b).length());
        ok_list.push((
            "left controller walked with the head".into(),
            walked.is_some_and(|d| d > 1.0),
            format!("{walked:?} m"),
            "> 1 m".into(),
        ));
        let mut ok = true;
        for (what, good, got, want) in &ok_list {
            println!("{} {what}: {got}, want {want}", if *good { "ok  " } else { "FAIL" });
            ok &= good;
        }
        println!("{}", if ok { "PASS" } else { "FAIL" });
        return if ok { 0 } else { 1 };
    }

    // ------------------------------------------------ the Input tab's values, as the app read them
    if input_mode {
        let near = |a: f32, b: f32| (a - b).abs() < 0.011;
        let ok_list = [
            ("right trigger", near(states[1].0, 0.7), format!("{:.2}", states[1].0), "0.70"),
            ("right A (primary)", states[1].3, states[1].3.to_string(), "true"),
            ("left stick", near(states[0].2[0], 0.5) && near(states[0].2[1], -0.25), format!("{:?}", states[0].2), "[0.5, -0.25]"),
            ("left squeeze", near(states[0].1, 1.0), format!("{:.2}", states[0].1), "1.00"),
            ("left trigger (untouched)", near(states[0].0, 0.0), format!("{:.2}", states[0].0), "0.00"),
            ("left X (untouched)", !states[0].3, states[0].3.to_string(), "false"),
            ("right squeeze (untouched)", near(states[1].1, 0.0), format!("{:.2}", states[1].1), "0.00"),
            ("right stick (untouched)", near(states[1].2[0], 0.0) && near(states[1].2[1], 0.0), format!("{:?}", states[1].2), "[0, 0]"),
        ];
        let mut ok = true;
        for (what, good, got, want) in &ok_list {
            println!("{} {what}: app read {got}, want {want}", if *good { "ok  " } else { "FAIL" });
            ok &= good;
        }
        println!("{}", if ok { "PASS" } else { "FAIL" });
        return if ok { 0 } else { 1 };
    }

    // ------------------------------------------------ compare with a real centre camera
    let Some(snapshot) = snapshot else { return 0 };
    let Ok(b) = std::fs::read(&snapshot) else {
        eprintln!("FAIL: no window snapshot at {}", snapshot.display());
        return 1;
    };
    let w = i32::from_le_bytes(b[18..22].try_into().unwrap());
    let h = -i32::from_le_bytes(b[22..26].try_into().unwrap());
    let px = |x: i32, y: i32| {
        let o = 54 + ((y * w + x) * 4) as usize;
        [b[o + 2], b[o + 1], b[o]]
    };
    let eyes = last.unwrap();
    let (cpose, cfov) = centre_eye((&eyes[0].0, eyes[0].1), (&eyes[1].0, eyes[1].1));
    let aspect = (cfov.right.tan() - cfov.left.tan()) / (cfov.up.tan() - cfov.down.tan());
    // The view sits under the 34 px toolbar; the centre picture is letterboxed in it.
    let (vw0, vh0) = (w as f32, (h - 34) as f32);
    let (mut cw, mut ch) = (vw0, vw0 / aspect);
    if ch > vh0 {
        ch = vh0;
        cw = vh0 * aspect;
    }
    let (x0, y0) = (((vw0 - cw) * 0.5).round() as i32, (34.0 + (vh0 - ch) * 0.5).round() as i32);
    let (rw, rh) = (cw.round() as u32, ch.round() as u32);
    let tex = |format: DXGI_FORMAT, bind: u32, usage: D3D11_USAGE, cpu: u32| {
        let mut t = None;
        device
            .CreateTexture2D(
                &D3D11_TEXTURE2D_DESC {
                    Width: rw,
                    Height: rh,
                    MipLevels: 1,
                    ArraySize: 1,
                    Format: format,
                    SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                    Usage: usage,
                    BindFlags: bind,
                    CPUAccessFlags: cpu,
                    MiscFlags: 0,
                },
                None,
                Some(&mut t),
            )
            .unwrap();
        t.unwrap()
    };
    let colour = tex(DXGI_FORMAT_R8G8B8A8_UNORM, D3D11_BIND_RENDER_TARGET.0 as u32, D3D11_USAGE_DEFAULT, 0);
    let depth = tex(DXGI_FORMAT_D32_FLOAT, D3D11_BIND_DEPTH_STENCIL.0 as u32, D3D11_USAGE_DEFAULT, 0);
    let staging = tex(DXGI_FORMAT_R8G8B8A8_UNORM, 0, D3D11_USAGE_STAGING, D3D11_CPU_ACCESS_READ.0 as u32);
    let mut rtv = None;
    device.CreateRenderTargetView(&colour, None, Some(&mut rtv)).unwrap();
    let mut dsv = None;
    device.CreateDepthStencilView(&depth, None, Some(&mut dsv)).unwrap();
    let (rtv, dsv) = (rtv.unwrap(), dsv.unwrap());
    context.ClearRenderTargetView(&rtv, &sky);
    context.ClearDepthStencilView(&dsv, D3D11_CLEAR_DEPTH.0 as u32, 1.0, 0);
    let viewport = D3D11_VIEWPORT { TopLeftX: 0.0, TopLeftY: 0.0, Width: rw as f32, Height: rh as f32, MinDepth: 0.0, MaxDepth: 1.0 };
    let mut insts = scene(head_h.unwrap(), 0.0);
    insts.extend(controllers.iter().copied());
    draw(&view_proj(&cpose, cfov), &insts, &rtv, &dsv, viewport);
    context.OMSetRenderTargets(None, None);
    context.CopyResource(&staging, &colour);
    let mut m = D3D11_MAPPED_SUBRESOURCE::default();
    context.Map(&staging, 0, D3D11_MAP_READ, 0, Some(&mut m)).unwrap();
    let pitch = m.RowPitch as usize;
    let data = std::slice::from_raw_parts(m.pData as *const u8, pitch * rh as usize).to_vec();
    context.Unmap(&staging, 0);
    let reference = |x: i32, y: i32| {
        let (x, y) = (x.clamp(0, rw as i32 - 1) as usize, y.clamp(0, rh as i32 - 1) as usize);
        let o = y * pitch + x * 4;
        [data[o], data[o + 1], data[o + 2]]
    };
    // A pixel differs when no reference pixel within one pixel of it is close
    // (allows for the letterbox's sub-pixel offset and filtering).
    let (mut bad, mut total) = (0u64, 0u64);
    let mut out = Vec::with_capacity((rw * rh * 4) as usize);
    for y in 0..rh as i32 {
        for x in 0..rw as i32 {
            let c = px(x0 + x, y0 + y);
            let mut best = 255;
            for dy in -1..=1 {
                for dx in -1..=1 {
                    let r = reference(x + dx, y + dy);
                    let d = (0..3).map(|k| (c[k] as i32 - r[k] as i32).abs()).max().unwrap();
                    best = best.min(d);
                }
            }
            total += 1;
            if best > 40 {
                bad += 1;
            }
            let r = reference(x, y);
            out.extend([r[2], r[1], r[0], 255]);
        }
    }
    let share = bad as f64 / total as f64 * 100.0;
    let reference_path = snapshot.with_file_name(format!(
        "{}-reference.bmp",
        snapshot.file_stem().unwrap().to_string_lossy()
    ));
    let _ = xrmimic_runtime_bmp(&reference_path, rw, rh, &out);
    println!(
        "centre view {} depth vs a real centre camera: {share:.2}% of {total} pixels differ (reference: {})",
        if send_depth { "with" } else { "without" },
        reference_path.display()
    );
    // With depth the view should be nearly the real thing; without, the near
    // objects alone shift (the share is printed, not judged).
    if send_depth && share > 2.0 {
        println!("FAIL: more than 2% differ");
        return 1;
    }
    println!("PASS");
    0
}

/// A top-down 32-bit BMP of BGRA pixels.
#[cfg(windows)]
fn xrmimic_runtime_bmp(path: &std::path::Path, w: u32, h: u32, bgra: &[u8]) -> std::io::Result<()> {
    let mut f = Vec::with_capacity(54 + bgra.len());
    f.extend(b"BM");
    f.extend((54 + bgra.len() as u32).to_le_bytes());
    f.extend([0u8; 4]);
    f.extend(54u32.to_le_bytes());
    f.extend(40u32.to_le_bytes());
    f.extend((w as i32).to_le_bytes());
    f.extend((-(h as i32)).to_le_bytes());
    f.extend(1u16.to_le_bytes());
    f.extend(32u16.to_le_bytes());
    f.extend([0u8; 24]);
    f.extend(bgra);
    std::fs::write(path, f)
}
