//! A native OpenXR client for testing xrmimic's compositing without a browser.
//!
//!   cargo run --release -p xrmimic-runtime --example quad_client       (shows it for 60 s)
//!   XRMIMIC_TEST_SNAPSHOT=<out.bmp> cargo run ... --example quad_client  (checks the picture)
//!   XRMIMIC_TEST_SNAPSHOT=<out.bmp> cargo run ... --example quad_client -- depth  (checks the depth view)
//!   ... -- centre / centre-nodepth  (checks the centre view, with and without depth)
//!   ... -- msaa  (any mode with 4x MSAA colour and depth swapchains)
//!   ... -- reproject / reproject-nodepth  (checks the window's reprojection)
//!   ... -- depthtest  (quad A depth-tested, XR_FB_composition_layer_depth_test:
//!          hidden behind the left eye's 1 m, shown in front of the right eye's 4 m)
//!
//! It talks to the runtime directly (negotiation and xrGetInstanceProcAddr, no
//! loader), renders with its own D3D11 device, and submits a blue projection
//! layer plus two quad layers:
//!   A: 0.6 x 0.6 m, opaque, 1.5 m straight ahead (LOCAL), top half yellow, bottom half red;
//!   B: 0.3 x 0.3 m, green, left eye only, 0.4 m to the right of A;
//! and, behind them, a cylinder layer: radius 2 m around the start point,
//! 1 rad wide, 0.5 m tall, left part magenta, right part cyan; and, behind
//! everything, an equirect layer at infinity (radius 0): 1.2 rad wide, from
//! 0.7 to 0.35 rad above the horizon, in quadrants orange / white over
//! purple / black; and, first of all, a cube layer at infinity turned 90
//! degrees to the left, transparent but for two patches below the horizon on
//! its +X face (which that turn brings ahead) and a red -X face (which it
//! takes out of view).
//! Each projection view carries a depth buffer (XR_KHR_composition_layer_depth):
//! the left eye's says 1 m everywhere with ordinary Z (0.1..100 m), the right
//! eye's 4 m with reversed infinite Z; `depth` mode checks the window's depth view.
//! `centre` modes submit only the projection layer: a blue wall 0.5 m ahead
//! with a yellow stripe left and a green stripe right, painted where each eye
//! sees them, and depth saying 0.5 m in both eyes (`centre-nodepth`: no depth).
//! The window's centre view must show the stripes where a camera between the
//! eyes sees them (with depth), or where the nearer eye sees them (without).
//! The headset window photographs itself (XRMIMIC_TEST_SNAPSHOT) during the
//! session; afterwards the client projects the quads with the eye poses and
//! fields of view it was given and checks the colours where they must appear.

#[cfg(not(windows))]
fn main() {
    eprintln!("quad_client needs Windows (D3D11)");
}

#[cfg(windows)]
fn main() {
    let code = unsafe { run() };
    std::process::exit(code);
}

#[cfg(windows)]
unsafe fn run() -> i32 {
    use openxr_sys as xr;
    use std::ffi::c_char;
    use windows::core::Interface;
    use windows::Win32::Foundation::{HMODULE, LUID, RECT};
    use windows::Win32::Graphics::Direct3D::*;
    use windows::Win32::Graphics::Direct3D11::*;
    use windows::Win32::Graphics::Dxgi::*;
    use xr::Handle as _;
    use xrmimic_core::math::{
        cube_direction, depth_shade, depth_value, cylinder_height, cylinder_point, equirect_point, mul_point, quad_clip_matrix, Fov, Pose, Quat, Vec3,
        FAR_SPHERE_RADIUS,
    };

    // Without XRMIMIC_TEST_SNAPSHOT it only shows: the window stays on top of the
    // desktop for 60 s (look around with the mouse), no checks.
    let snapshot = std::env::var_os("XRMIMIC_TEST_SNAPSHOT").map(std::path::PathBuf::from);
    let depth_mode = std::env::args().any(|a| a == "depth");
    // `msaa`: every swapchain but the cube map has 4 samples (colour and depth).
    let samples: u32 = if std::env::args().any(|a| a == "msaa") { 4 } else { 1 };
    let centre_depth = std::env::args().any(|a| a == "centre");
    let centre_mode = centre_depth || std::env::args().any(|a| a == "centre-nodepth");
    // `reproject`: the frames claim eye poses 6 cm left of where the eyes are
    // (as if drawn before the head moved right); the window's reprojection
    // must move the stripes to where the eyes see them now, through depth
    // (`reproject-nodepth`: rotation only, so they stay where drawn).
    let reproject_depth = std::env::args().any(|a| a == "reproject");
    let reproject_mode = reproject_depth || std::env::args().any(|a| a == "reproject-nodepth");
    let stale = if reproject_mode { 0.06f32 } else { 0.0 };
    // Both paint stripes on a wall and send depth saying where the wall is.
    let wall_mode = centre_mode || reproject_mode;
    let wall_depth = centre_depth || reproject_depth;
    let colour_checks = !depth_mode && !wall_mode;
    let depth_test = std::env::args().any(|a| a == "depthtest");
    if centre_mode {
        std::env::set_var("XRMIMIC_TEST_CENTRE_VIEW", "1");
    }
    if reproject_mode {
        std::env::set_var("XRMIMIC_TEST_REPROJECT", "1");
    }
    // The centre modes' wall: its distance, and the stripes' centres and colours.
    const WALL: f32 = 0.5;
    const STRIPE_HALF: f32 = 0.015;
    let stripes = [(-0.15f32, [1.0f32, 1.0, 0.0, 1.0]), (0.15, [0.0, 1.0, 0.0, 1.0])];
    if depth_mode {
        // The window starts in its depth view.
        std::env::set_var("XRMIMIC_TEST_DEPTH_VIEW", "1");
    }
    if let Some(s) = &snapshot {
        let _ = std::fs::remove_file(s);
    }
    let frame_count = if snapshot.is_some() { 400 } else { 60 * 90 };
    if snapshot.is_none() && std::env::var_os("XRMIMIC_TOPMOST").is_none() {
        // Showing: keep the headset window in front of other windows.
        std::env::set_var("XRMIMIC_TOPMOST", "1");
    }

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
        c"XR_KHR_composition_layer_cylinder".as_ptr(),
        c"XR_KHR_composition_layer_equirect2".as_ptr(),
        c"XR_KHR_composition_layer_cube".as_ptr(),
        c"XR_KHR_composition_layer_depth".as_ptr(),
        c"XR_FB_composition_layer_depth_test".as_ptr(),
    ];
    let mut app = xr::ApplicationInfo {
        application_name: [0; xr::MAX_APPLICATION_NAME_SIZE],
        application_version: 1,
        engine_name: [0; xr::MAX_ENGINE_NAME_SIZE],
        engine_version: 0,
        api_version: xr::Version::new(1, 0, 0),
    };
    for (d, s) in app.application_name.iter_mut().zip(b"XRMimic quad_client") {
        *d = *s as c_char;
    }
    let ci = xr::InstanceCreateInfo {
        ty: xr::InstanceCreateInfo::TYPE,
        next: std::ptr::null(),
        create_flags: xr::InstanceCreateFlags::EMPTY,
        application_info: app,
        enabled_api_layer_count: 0,
        enabled_api_layer_names: std::ptr::null(),
        enabled_extension_count: 5,
        enabled_extension_names: exts.as_ptr(),
    };
    let mut instance = xr::Instance::NULL;
    check!(create_instance(&ci, &mut instance));
    let inst = instance;
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
    let context: ID3D11DeviceContext1 = context.unwrap().cast().unwrap();

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
    let next_state = |want: xr::SessionState| {
        for _ in 0..500 {
            let mut buf: xr::EventDataBuffer = std::mem::zeroed();
            buf.ty = xr::EventDataBuffer::TYPE;
            if poll_event(inst, &mut buf) == xr::Result::SUCCESS
                && buf.ty == xr::StructureType::EVENT_DATA_SESSION_STATE_CHANGED
            {
                let e = &*(&buf as *const _ as *const xr::EventDataSessionStateChanged);
                if e.state == want {
                    return;
                }
            } else {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        panic!("no {want:?} event");
    };
    next_state(xr::SessionState::READY);
    let sbi = xr::SessionBeginInfo {
        ty: xr::SessionBeginInfo::TYPE,
        next: std::ptr::null(),
        primary_view_configuration_type: xr::ViewConfigurationType::PRIMARY_STEREO,
    };
    check!(begin_session(session, &sbi));
    let identity = xr::Posef {
        orientation: xr::Quaternionf { x: 0.0, y: 0.0, z: 0.0, w: 1.0 },
        position: xr::Vector3f { x: 0.0, y: 0.0, z: 0.0 },
    };
    let rsci = xr::ReferenceSpaceCreateInfo {
        ty: xr::ReferenceSpaceCreateInfo::TYPE,
        next: std::ptr::null(),
        reference_space_type: xr::ReferenceSpaceType::LOCAL,
        pose_in_reference_space: identity,
    };
    let mut local = xr::Space::NULL;
    check!(create_ref_space(session, &rsci, &mut local));

    // ------------------------------------------------ swapchains
    let mut count = 0;
    let mut vcv: [xr::ViewConfigurationView; 2] = std::mem::zeroed();
    for v in &mut vcv {
        v.ty = xr::ViewConfigurationView::TYPE;
    }
    check!(enum_views(inst, system, xr::ViewConfigurationType::PRIMARY_STEREO, 2, &mut count, vcv.as_mut_ptr()));
    let (eye_w, eye_h) = (vcv[0].recommended_image_rect_width, vcv[0].recommended_image_rect_height);
    const RGBA8: i64 = 28; // DXGI_FORMAT_R8G8B8A8_UNORM
    // One render target per face of each image (faces 6: a cube map).
    let make_faces = |w: u32, h: u32, faces: u32| -> (xr::Swapchain, Vec<Vec<ID3D11RenderTargetView>>) {
        let ci = xr::SwapchainCreateInfo {
            ty: xr::SwapchainCreateInfo::TYPE,
            next: std::ptr::null(),
            create_flags: xr::SwapchainCreateFlags::EMPTY,
            usage_flags: xr::SwapchainUsageFlags::COLOR_ATTACHMENT,
            format: RGBA8,
            sample_count: if faces == 1 { samples } else { 1 },
            width: w,
            height: h,
            face_count: faces,
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
        let rtvs = imgs
            .iter()
            .map(|i| {
                let tex = ID3D11Texture2D::from_raw_borrowed(&i.texture).unwrap();
                (0..faces)
                    .map(|f| {
                        let desc = D3D11_RENDER_TARGET_VIEW_DESC {
                            Format: windows::Win32::Graphics::Dxgi::Common::DXGI_FORMAT(RGBA8 as i32),
                            ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2DARRAY,
                            Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 {
                                Texture2DArray: D3D11_TEX2D_ARRAY_RTV { MipSlice: 0, FirstArraySlice: f, ArraySize: 1 },
                            },
                        };
                        let mut rtv = None;
                        device.CreateRenderTargetView(tex, (faces > 1).then_some(&desc as *const _), Some(&mut rtv)).unwrap();
                        rtv.unwrap()
                    })
                    .collect()
            })
            .collect();
        (sc, rtvs)
    };
    // A D32_FLOAT depth swapchain and a depth-stencil view per image.
    let make_depth = |w: u32, h: u32| -> (xr::Swapchain, Vec<ID3D11DepthStencilView>) {
        let ci = xr::SwapchainCreateInfo {
            ty: xr::SwapchainCreateInfo::TYPE,
            next: std::ptr::null(),
            create_flags: xr::SwapchainCreateFlags::EMPTY,
            usage_flags: xr::SwapchainUsageFlags::DEPTH_STENCIL_ATTACHMENT,
            format: 40, // DXGI_FORMAT_D32_FLOAT
            sample_count: samples,
            width: w,
            height: h,
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
        let dsvs = imgs
            .iter()
            .map(|i| {
                let tex = ID3D11Texture2D::from_raw_borrowed(&i.texture).unwrap();
                let mut dsv = None;
                device.CreateDepthStencilView(tex, None, Some(&mut dsv)).unwrap();
                dsv.unwrap()
            })
            .collect();
        (sc, dsvs)
    };
    let make_swapchain = |w: u32, h: u32| {
        let (sc, rtvs) = make_faces(w, h, 1);
        (sc, rtvs.into_iter().map(|mut v| v.remove(0)).collect::<Vec<_>>())
    };
    let (proj_sc, proj_rtvs) = make_swapchain(eye_w * 2, eye_h);
    let (quad_sc, quad_rtvs) = make_swapchain(256, 256);
    let (green_sc, green_rtvs) = make_swapchain(64, 64);
    let (cyl_sc, cyl_rtvs) = make_swapchain(256, 64);
    let (cyl_r, cyl_angle, cyl_aspect) = (2.0f32, 1.0f32, 4.0f32);
    let cyl_pose = Pose::IDENTITY;
    let (eq_sc, eq_rtvs) = make_swapchain(128, 64);
    let (eq_h, eq_upper, eq_lower) = (1.2f32, 0.7f32, 0.35f32);
    let (cube_sc, cube_rtvs) = make_faces(64, 64, 6);
    // Depth per eye: (swapchain, views, min, max, near, far, what it says in metres).
    let inf = f32::INFINITY;
    let (left_m, right_m) = if wall_mode { (WALL, WALL) } else { (1.0, 4.0) };
    let depth_eyes = [(make_depth(eye_w, eye_h), 0.1f32, 100.0f32, left_m), (make_depth(eye_w, eye_h), inf, 0.1, right_m)];
    let cube_first: Vec<ID3D11RenderTargetView> = cube_rtvs.iter().map(|v| v[0].clone()).collect();
    // Turned 90 degrees left: the +X face comes ahead and -X goes behind.
    let cube_turn = Quat::from_yaw_pitch_roll(std::f32::consts::FRAC_PI_2, 0.0, 0.0);

    // ------------------------------------------------ frames
    let quad_a = Pose::new(Quat::IDENTITY, Vec3::new(0.0, 0.0, -1.5));
    let quad_b = Pose::new(Quat::IDENTITY, Vec3::new(0.4, 0.0, -1.5));
    let to_xr = |p: &Pose| xr::Posef {
        orientation: xr::Quaternionf { x: p.orientation.x, y: p.orientation.y, z: p.orientation.z, w: p.orientation.w },
        position: xr::Vector3f { x: p.position.x, y: p.position.y, z: p.position.z },
    };
    let mut last_views: [xr::View; 2] = std::mem::zeroed();
    let fill = |sc: xr::Swapchain, rtvs: &[ID3D11RenderTargetView], paint: &dyn Fn(&ID3D11RenderTargetView)| {
        let mut idx = 0;
        check!(acquire(sc, std::ptr::null(), &mut idx));
        let wi = xr::SwapchainImageWaitInfo {
            ty: xr::SwapchainImageWaitInfo::TYPE,
            next: std::ptr::null(),
            timeout: xr::Duration::INFINITE,
        };
        check!(wait_image(sc, &wi));
        paint(&rtvs[idx as usize]);
        check!(release(sc, std::ptr::null()));
    };
    // Tests: run until the window has photographed itself (it starts later
    // on a busy machine) and a few frames more; at least 400, at most a minute.
    let mut shot_seen: Option<u32> = None;
    for frame in 0u32.. {
        if let Some(path) = &snapshot {
            if path.exists() {
                shot_seen.get_or_insert(frame);
            }
            if (frame >= 400 && shot_seen.is_some_and(|f| frame >= f + 45)) || frame >= 90 * 60 {
                break;
            }
        } else if frame >= frame_count {
            break;
        }
        let mut fs: xr::FrameState = std::mem::zeroed();
        fs.ty = xr::FrameState::TYPE;
        check!(wait_frame(session, std::ptr::null(), &mut fs));
        check!(begin_frame(session, std::ptr::null()));
        let vli = xr::ViewLocateInfo {
            ty: xr::ViewLocateInfo::TYPE,
            next: std::ptr::null(),
            view_configuration_type: xr::ViewConfigurationType::PRIMARY_STEREO,
            display_time: fs.predicted_display_time,
            space: local,
        };
        let mut vs: xr::ViewState = std::mem::zeroed();
        vs.ty = xr::ViewState::TYPE;
        let mut views: [xr::View; 2] = std::mem::zeroed();
        for v in &mut views {
            v.ty = xr::View::TYPE;
        }
        let mut n = 0;
        check!(locate_views(session, &vli, &mut vs, 2, &mut n, views.as_mut_ptr()));
        last_views = views;
        // What the frame says it was drawn from (reproject modes: stale poses).
        let mut views = views;
        for v in &mut views {
            v.pose.position.x -= stale;
        }
        fill(proj_sc, &proj_rtvs, &|rtv| {
            context.ClearRenderTargetView(rtv, &[0.0, 0.0, 1.0, 1.0]);
            if wall_mode {
                // Each stripe where this frame's eye sees it on the wall.
                let column = |e: usize, wx: f32| {
                    let v = &views[e];
                    let o = v.pose.orientation;
                    let eye = Pose::new(
                        Quat { x: o.x, y: o.y, z: o.z, w: o.w },
                        Vec3::new(v.pose.position.x, v.pose.position.y, v.pose.position.z),
                    );
                    let l = eye.inverse().transform_point(Vec3::new(wx, 0.0, -WALL));
                    let (tl, tr) = (v.fov.angle_left.tan(), v.fov.angle_right.tan());
                    ((l.x / -l.z - tl) / (tr - tl) * eye_w as f32).round() as i32
                };
                for e in 0..2 {
                    for (x, colour) in &stripes {
                        let off = (e as u32 * eye_w) as i32;
                        let r = RECT {
                            left: off + column(e, x - STRIPE_HALF),
                            top: 0,
                            right: off + column(e, x + STRIPE_HALF),
                            bottom: eye_h as i32,
                        };
                        context.ClearView(rtv, colour, Some(&[r]));
                    }
                }
            }
        });
        fill(quad_sc, &quad_rtvs, &|rtv| {
            context.ClearView(rtv, &[1.0, 1.0, 0.0, 1.0], Some(&[RECT { left: 0, top: 0, right: 256, bottom: 128 }]));
            context.ClearView(rtv, &[1.0, 0.0, 0.0, 1.0], Some(&[RECT { left: 0, top: 128, right: 256, bottom: 256 }]));
        });
        fill(green_sc, &green_rtvs, &|rtv| context.ClearRenderTargetView(rtv, &[0.0, 1.0, 0.0, 1.0]));
        fill(cyl_sc, &cyl_rtvs, &|rtv| {
            context.ClearView(rtv, &[1.0, 0.0, 1.0, 1.0], Some(&[RECT { left: 0, top: 0, right: 128, bottom: 64 }]));
            context.ClearView(rtv, &[0.0, 1.0, 1.0, 1.0], Some(&[RECT { left: 128, top: 0, right: 256, bottom: 64 }]));
        });
        fill(cube_sc, &cube_first, &|rtv| {
            let faces = &cube_rtvs[cube_first.iter().position(|r| r == rtv).unwrap()];
            for f in faces {
                context.ClearRenderTargetView(f, &[0.0, 0.0, 0.0, 0.0]);
            }
            // +X face: pink patch on the image's left, brown on its right, low down.
            context.ClearView(&faces[0], &[1.0, 0.5, 0.5, 1.0], Some(&[RECT { left: 8, top: 46, right: 22, bottom: 62 }]));
            context.ClearView(&faces[0], &[0.5, 0.25, 0.0, 1.0], Some(&[RECT { left: 42, top: 46, right: 56, bottom: 62 }]));
            // -X face: all red (shows ahead only if the turn were ignored).
            context.ClearRenderTargetView(&faces[1], &[1.0, 0.0, 0.0, 1.0]);
        });
        fill(eq_sc, &eq_rtvs, &|rtv| {
            let q = |l, t, c: [f32; 4]| {
                context.ClearView(rtv, &c, Some(&[RECT { left: l, top: t, right: l + 64, bottom: t + 32 }]))
            };
            q(0, 0, [1.0, 0.5, 0.0, 1.0]);
            q(64, 0, [1.0, 1.0, 1.0, 1.0]);
            q(0, 32, [0.5, 0.0, 1.0, 1.0]);
            q(64, 32, [0.0, 0.0, 0.0, 1.0]);
        });
        let depth_infos: Vec<xr::CompositionLayerDepthInfoKHR> = depth_eyes
            .iter()
            .map(|((sc, dsvs), near, far, metres)| {
                let mut idx = 0;
                check!(acquire(*sc, std::ptr::null(), &mut idx));
                let wi = xr::SwapchainImageWaitInfo {
                    ty: xr::SwapchainImageWaitInfo::TYPE,
                    next: std::ptr::null(),
                    timeout: xr::Duration::INFINITE,
                };
                check!(wait_image(*sc, &wi));
                let v = depth_value(*metres, 0.0, 1.0, *near, *far);
                context.ClearDepthStencilView(&dsvs[idx as usize], D3D11_CLEAR_DEPTH.0 as u32, v, 0);
                check!(release(*sc, std::ptr::null()));
                xr::CompositionLayerDepthInfoKHR {
                    ty: xr::CompositionLayerDepthInfoKHR::TYPE,
                    next: std::ptr::null(),
                    sub_image: xr::SwapchainSubImage {
                        swapchain: *sc,
                        image_rect: xr::Rect2Di {
                            offset: xr::Offset2Di { x: 0, y: 0 },
                            extent: xr::Extent2Di { width: eye_w as i32, height: eye_h as i32 },
                        },
                        image_array_index: 0,
                    },
                    min_depth: 0.0,
                    max_depth: 1.0,
                    near_z: *near,
                    far_z: *far,
                }
            })
            .collect();
        let pviews: Vec<xr::CompositionLayerProjectionView> = (0..2)
            .map(|e| xr::CompositionLayerProjectionView {
                ty: xr::CompositionLayerProjectionView::TYPE,
                next: if wall_mode && !wall_depth {
                    std::ptr::null()
                } else {
                    &depth_infos[e] as *const _ as *const std::ffi::c_void
                },
                pose: views[e].pose,
                fov: views[e].fov,
                sub_image: xr::SwapchainSubImage {
                    swapchain: proj_sc,
                    image_rect: xr::Rect2Di {
                        offset: xr::Offset2Di { x: (e as u32 * eye_w) as i32, y: 0 },
                        extent: xr::Extent2Di { width: eye_w as i32, height: eye_h as i32 },
                    },
                    image_array_index: 0,
                },
            })
            .collect();
        let proj = xr::CompositionLayerProjection {
            ty: xr::CompositionLayerProjection::TYPE,
            next: std::ptr::null(),
            layer_flags: xr::CompositionLayerFlags::EMPTY,
            space: local,
            view_count: 2,
            views: pviews.as_ptr(),
        };
        let quad = |sc: xr::Swapchain, w: i32, pose: &Pose, size: f32, eyes: xr::EyeVisibility| xr::CompositionLayerQuad {
            ty: xr::CompositionLayerQuad::TYPE,
            next: std::ptr::null(),
            layer_flags: xr::CompositionLayerFlags::EMPTY,
            space: local,
            eye_visibility: eyes,
            sub_image: xr::SwapchainSubImage {
                swapchain: sc,
                image_rect: xr::Rect2Di {
                    offset: xr::Offset2Di { x: 0, y: 0 },
                    extent: xr::Extent2Di { width: w, height: w },
                },
                image_array_index: 0,
            },
            pose: to_xr(pose),
            size: xr::Extent2Df { width: size, height: size },
        };
        let mut qa = quad(quad_sc, 256, &quad_a, 0.6, xr::EyeVisibility::BOTH);
        let dt = xr::CompositionLayerDepthTestFB {
            ty: xr::CompositionLayerDepthTestFB::TYPE,
            next: std::ptr::null(),
            depth_mask: xr::FALSE,
            compare_op: xr::CompareOpFB::LESS,
        };
        if depth_test {
            qa.next = &dt as *const _ as *const std::ffi::c_void;
        }
        let qb = quad(green_sc, 64, &quad_b, 0.3, xr::EyeVisibility::LEFT);
        let cyl = xr::CompositionLayerCylinderKHR {
            ty: xr::CompositionLayerCylinderKHR::TYPE,
            next: std::ptr::null(),
            layer_flags: xr::CompositionLayerFlags::EMPTY,
            space: local,
            eye_visibility: xr::EyeVisibility::BOTH,
            sub_image: xr::SwapchainSubImage {
                swapchain: cyl_sc,
                image_rect: xr::Rect2Di {
                    offset: xr::Offset2Di { x: 0, y: 0 },
                    extent: xr::Extent2Di { width: 256, height: 64 },
                },
                image_array_index: 0,
            },
            pose: to_xr(&cyl_pose),
            radius: cyl_r,
            central_angle: cyl_angle,
            aspect_ratio: cyl_aspect,
        };
        let eq = xr::CompositionLayerEquirect2KHR {
            ty: xr::CompositionLayerEquirect2KHR::TYPE,
            next: std::ptr::null(),
            layer_flags: xr::CompositionLayerFlags::EMPTY,
            space: local,
            eye_visibility: xr::EyeVisibility::BOTH,
            sub_image: xr::SwapchainSubImage {
                swapchain: eq_sc,
                image_rect: xr::Rect2Di {
                    offset: xr::Offset2Di { x: 0, y: 0 },
                    extent: xr::Extent2Di { width: 128, height: 64 },
                },
                image_array_index: 0,
            },
            pose: to_xr(&Pose::IDENTITY),
            radius: 0.0,
            central_horizontal_angle: eq_h,
            upper_vertical_angle: eq_upper,
            lower_vertical_angle: eq_lower,
        };
        let cube = xr::CompositionLayerCubeKHR {
            ty: xr::CompositionLayerCubeKHR::TYPE,
            next: std::ptr::null(),
            layer_flags: xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA,
            space: local,
            eye_visibility: xr::EyeVisibility::BOTH,
            swapchain: cube_sc,
            image_array_index: 0,
            orientation: to_xr(&Pose::new(cube_turn, Vec3::ZERO)).orientation,
        };
        // Back to front: projection, the cube, the equirect, the cylinder, then the quads.
        let layers: [*const xr::CompositionLayerBaseHeader; 6] = [
            &proj as *const _ as *const _,
            &cube as *const _ as *const _,
            &eq as *const _ as *const _,
            &cyl as *const _ as *const _,
            &qa as *const _ as *const _,
            &qb as *const _ as *const _,
        ];
        let fei = xr::FrameEndInfo {
            ty: xr::FrameEndInfo::TYPE,
            next: std::ptr::null(),
            display_time: fs.predicted_display_time,
            environment_blend_mode: xr::EnvironmentBlendMode::OPAQUE,
            // The centre and reproject modes check the projection layer alone.
            layer_count: if wall_mode { 1 } else { 6 },
            layers: layers.as_ptr(),
        };
        check!(end_frame(session, &fei));
    }
    check!(request_exit(session));
    next_state(xr::SessionState::STOPPING);
    check!(end_session(session));
    check!(destroy_session(session));
    check!(destroy_instance(inst));

    // ------------------------------------------------ check the window's picture
    let Some(snapshot) = snapshot else {
        println!("shown for 60 s (set XRMIMIC_TEST_SNAPSHOT=<out.bmp> to check the picture)");
        return 0;
    };
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
    // The view sits under the 34 px toolbar, letterboxed to the frame's aspect.
    let (vx, vy, vw0, vh0) = (0.0f32, 34.0f32, w as f32, (h - 34) as f32);
    let aspect = (eye_w * 2) as f32 / eye_h as f32;
    let (mut vw, mut vh) = (vw0, vw0 / aspect);
    if vh > vh0 {
        vh = vh0;
        vw = vh0 * aspect;
    }
    let (x0, y0) = (vx + (vw0 - vw) * 0.5, vy + (vh0 - vh) * 0.5);
    let eye_pose = |e: usize| {
        let p = last_views[e].pose;
        Pose::new(
            Quat { x: p.orientation.x, y: p.orientation.y, z: p.orientation.z, w: p.orientation.w },
            Vec3::new(p.position.x, p.position.y, p.position.z),
        )
    };
    let eye_fov = |e: usize| {
        let f = last_views[e].fov;
        Fov { left: f.angle_left, right: f.angle_right, up: f.angle_up, down: f.angle_down }
    };
    // Where a point appears in eye e: (u, v) in a quad's unit square, or a
    // point already in the layer's own space for a cylinder (size 1).
    let at_point = |e: usize, pose: &Pose, size: f32, p: Vec3| {
        let m = quad_clip_matrix(&eye_pose(e), eye_fov(e), pose, (size, size));
        let c = mul_point(&m, p);
        let (nx, ny) = (c[0] / c[3], c[1] / c[3]);
        let x = x0 + e as f32 * vw * 0.5 + (nx + 1.0) * 0.5 * vw * 0.5;
        let y = y0 + (1.0 - ny) * 0.5 * vh;
        (x.round() as i32, y.round() as i32)
    };
    let at = |e: usize, quad: &Pose, size: f32, u: f32, v: f32| at_point(e, quad, size, Vec3::new(u, v, 0.0));
    let cyl_h = cylinder_height(cyl_r, cyl_angle, cyl_aspect);
    let on_cyl = |e: usize, u: f32, v: f32| at_point(e, &cyl_pose, 1.0, cylinder_point(cyl_r, cyl_angle, cyl_h, u, v));
    // The sphere at infinity is centred on each eye.
    let on_eq = |e: usize, u: f32, v: f32| {
        let centre = Pose::new(Quat::IDENTITY, eye_pose(e).position);
        at_point(e, &centre, 1.0, equirect_point(FAR_SPHERE_RADIUS, eq_h, eq_upper, eq_lower, u, v))
    };
    // The cube at infinity is centred on each eye and turned by cube_turn.
    let on_cube = |e: usize, face: usize, u: f32, v: f32| {
        let centre = Pose::new(cube_turn, eye_pose(e).position);
        let d = cube_direction(face, u, v);
        let r = FAR_SPHERE_RADIUS;
        at_point(e, &centre, 1.0, Vec3::new(d.x * r, d.y * r, d.z * r))
    };
    let near = |c: [u8; 3], want: [u8; 3]| c.iter().zip(want).all(|(a, b)| (*a as i32 - b as i32).abs() < 40);
    let mut ok = true;
    let mut expect = |what: &str, (x, y): (i32, i32), want: [u8; 3]| {
        let c = px(x, y);
        let good = near(c, want);
        println!("{} {what} at ({x}, {y}): rgb {c:?}, want {want:?}", if good { "ok  " } else { "FAIL" });
        ok &= good;
    };
    if depth_mode {
        // The depth view replaces each eye's picture with its depth as greys.
        for (e, (_, _, _, metres)) in depth_eyes.iter().enumerate() {
            let g = (depth_shade(*metres) * 255.0).round() as u8;
            for &(fx, fy) in &[(0.5f32, 0.5f32), (0.15, 0.2), (0.85, 0.85)] {
                let x = x0 + e as f32 * vw * 0.5 + fx * vw * 0.5;
                let y = y0 + fy * vh;
                let what = format!("{} eye depth, {metres} m, at ({fx}, {fy}) of the eye", if e == 0 { "left" } else { "right" });
                expect(&what, (x.round() as i32, y.round() as i32), [g, g, g]);
            }
        }
    }
    if centre_mode {
        // One picture from between the eyes, letterboxed by its tangents.
        let (c, f) = xrmimic_core::math::centre_eye((&eye_pose(0), eye_fov(0)), (&eye_pose(1), eye_fov(1)));
        let (tl, tr, tu, td) = (f.left.tan(), f.right.tan(), f.up.tan(), f.down.tan());
        let aspect = (tr - tl) / (tu - td);
        let (mut cw, mut ch) = (vw0, vw0 / aspect);
        if ch > vh0 {
            ch = vh0;
            cw = vh0 * aspect;
        }
        let (cx0, cy0) = (vx + (vw0 - cw) * 0.5, vy + (vh0 - ch) * 0.5);
        let on_centre = |p: Vec3| {
            let l = c.inverse().transform_point(p);
            let (tx, ty) = (l.x / -l.z, l.y / -l.z);
            ((cx0 + (tx - tl) / (tr - tl) * cw).round() as i32, (cy0 + (tu - ty) / (tu - td) * ch).round() as i32)
        };
        for (i, (x, colour)) in stripes.iter().enumerate() {
            let want = colour.map(|v| (v * 255.0) as u8);
            let want = [want[0], want[1], want[2]];
            let blue = [0, 0, 255];
            // The stripe's eye: the left one on the left, the right one on the right.
            let e = i;
            // Where that eye alone would put it: shifted by the eye's offset from the centre.
            let shift = c.position.x - eye_pose(e).position.x;
            let true_spot = on_centre(Vec3::new(*x, -0.15, -WALL));
            let eye_spot = on_centre(Vec3::new(x + shift, -0.15, -WALL));
            let side = if i == 0 { "left" } else { "right" };
            if centre_depth {
                expect(&format!("centre view, {side} stripe where the centre sees it"), true_spot, want);
                expect(&format!("centre view, {side} stripe not where its eye sees it"), eye_spot, blue);
            } else {
                expect(&format!("centre view (no depth), {side} stripe where its eye sees it"), eye_spot, want);
                expect(&format!("centre view (no depth), {side} stripe not at the true spot"), true_spot, blue);
            }
        }
        expect("centre view, wall straight ahead", on_centre(Vec3::new(0.0, 0.1, -WALL)), [0, 0, 255]);
    }
    if reproject_mode {
        // Each eye where it is now: the stripes where it sees them (with
        // depth), or where the stale eye drew them (rotation only).
        let blue = [0, 0, 255];
        for e in 0..2 {
            let eye = if e == 0 { "left" } else { "right" };
            for (i, (x, colour)) in stripes.iter().enumerate() {
                let want = colour.map(|v| (v * 255.0) as u8);
                let want = [want[0], want[1], want[2]];
                let side = if i == 0 { "left" } else { "right" };
                let true_spot = at_point(e, &Pose::IDENTITY, 1.0, Vec3::new(*x, -0.15, -WALL));
                let stale_spot = at_point(e, &Pose::IDENTITY, 1.0, Vec3::new(x + stale, -0.15, -WALL));
                if reproject_depth {
                    expect(&format!("reprojected {eye} eye, {side} stripe where the eye sees it now"), true_spot, want);
                    expect(&format!("reprojected {eye} eye, {side} stripe gone from where it was drawn"), stale_spot, blue);
                } else {
                    expect(&format!("reprojected {eye} eye (no depth), {side} stripe where it was drawn"), stale_spot, want);
                    expect(&format!("reprojected {eye} eye (no depth), {side} stripe not moved"), true_spot, blue);
                }
            }
        }
    }
    for e in (0..2).filter(|_| colour_checks) {
        let eye = if e == 0 { "left" } else { "right" };
        if depth_test && e == 0 {
            // 1.5 m away, behind the left eye's 1 m: the cylinder behind it shows.
            expect("left eye, depth-tested quad A hidden (cylinder behind)", at(e, &quad_a, 0.6, -0.25, 0.25), [255, 0, 255]);
            continue;
        }
        expect(&format!("{eye} eye, quad A top half"), at(e, &quad_a, 0.6, 0.0, 0.25), [255, 255, 0]);
        expect(&format!("{eye} eye, quad A bottom half"), at(e, &quad_a, 0.6, 0.0, -0.25), [255, 0, 0]);
        expect(&format!("{eye} eye, beside quad A"), at(e, &quad_a, 0.6, -0.9, -0.9), [0, 0, 255]);
        expect(&format!("{eye} eye, cylinder left part"), on_cyl(e, 0.1, 0.15), [255, 0, 255]);
        expect(&format!("{eye} eye, cylinder right part"), on_cyl(e, 0.9, 0.15), [0, 255, 255]);
        expect(&format!("{eye} eye, above the cylinder"), on_cyl(e, 0.1, -0.3), [0, 0, 255]);
        expect(&format!("{eye} eye, equirect top left"), on_eq(e, 0.25, 0.25), [255, 128, 0]);
        expect(&format!("{eye} eye, equirect top right"), on_eq(e, 0.75, 0.25), [255, 255, 255]);
        expect(&format!("{eye} eye, equirect bottom left"), on_eq(e, 0.25, 0.75), [128, 0, 255]);
        expect(&format!("{eye} eye, equirect bottom right"), on_eq(e, 0.75, 0.75), [0, 0, 0]);
        expect(&format!("{eye} eye, below the equirect"), on_eq(e, 0.5, 1.15), [0, 0, 255]);
        expect(&format!("{eye} eye, cube +X face left patch"), on_cube(e, 0, 0.234, 0.84), [255, 128, 128]);
        expect(&format!("{eye} eye, cube +X face right patch"), on_cube(e, 0, 0.766, 0.84), [128, 64, 0]);
        expect(&format!("{eye} eye, cube between the patches (transparent)"), on_cube(e, 0, 0.5, 0.84), [0, 0, 255]);
    }
    if colour_checks {
        expect("left eye, quad B", at(0, &quad_b, 0.3, 0.0, 0.0), [0, 255, 0]);
        // B is left-eye only: in the right eye the cylinder behind it shows there.
        expect("right eye, where quad B would be", at(1, &quad_b, 0.3, 0.0, 0.0), [0, 255, 255]);
    }
    println!("{}", if ok { "PASS" } else { "FAIL" });
    if ok {
        0
    } else {
        1
    }
}
