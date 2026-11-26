//! A native Vulkan OpenXR client (XR_KHR_vulkan_enable2) for checking that a
//! Vulkan app's frames reach the headset window on Windows.
//!
//!   XRMIMIC_TEST_SNAPSHOT=<out.bmp> cargo run --release -p xrmimic-runtime --example vk_client
//!   ... -- msaa       (4x MSAA eye swapchains, cleared to one colour each)
//!   ... -- depth      (D32_SFLOAT depth per eye; checks the window's depth view)
//!   ... -- depth16    (the same with D16_UNORM)
//!   ... -- depth32s8  (D32_SFLOAT_S8_UINT, the right eye with finite reversed Z
//!                      as Godot sends it: 4000 m at depth 0, 0.05 m at 1)
//!
//! It talks to the runtime directly (no loader) and makes its Vulkan instance
//! and device through xrCreateVulkanInstanceKHR / xrCreateVulkanDeviceKHR.
//! Each eye is a 256 x 256 swapchain painted by buffer copies: the left eye
//! red over yellow, the right eye green over cyan (so a picture upside down
//! or with the eyes swapped is caught), and a blue quad layer 0.6 m wide sits
//! 1.5 m straight ahead. In `depth` modes each eye's depth runs from 1 m at
//! the top row to 8 m at the bottom (uploaded row by row, so a copy that
//! mixes up rows shows), the left eye with ordinary Z (0.1..100 m) and the
//! right with reversed infinite Z.

#[cfg(not(windows))]
fn main() {
    eprintln!("vk_client checks the Windows headset window; on Linux use the Python suite or Godot");
}

#[cfg(windows)]
fn main() {
    let code = unsafe { run() };
    std::process::exit(code);
}

#[cfg(windows)]
unsafe fn run() -> i32 {
    use ash::vk;
    use ash::vk::Handle as _;
    use openxr_sys as xr;
    use xr::Handle as _;
    use std::ffi::{c_char, c_void};
    use xrmimic_core::math::{depth_shade, depth_value};

    let args: Vec<String> = std::env::args().collect();
    let msaa = args.iter().any(|a| a == "msaa");
    let depth16 = args.iter().any(|a| a == "depth16");
    let d32s8 = args.iter().any(|a| a == "depth32s8");
    let depth_mode = depth16 || d32s8 || args.iter().any(|a| a == "depth");
    if depth_mode {
        std::env::set_var("XRMIMIC_TEST_DEPTH_VIEW", "1");
    }
    let snapshot = std::env::var_os("XRMIMIC_TEST_SNAPSHOT").map(std::path::PathBuf::from);
    if let Some(s) = &snapshot {
        let _ = std::fs::remove_file(s);
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

    // ------------------------------------------------ instance and system
    let create_instance = func!(xr::Instance::NULL, "xrCreateInstance", xr::pfn::CreateInstance);
    let exts = [c"XR_KHR_vulkan_enable2".as_ptr(), c"XR_KHR_composition_layer_depth".as_ptr()];
    let mut app = xr::ApplicationInfo {
        application_name: [0; xr::MAX_APPLICATION_NAME_SIZE],
        application_version: 1,
        engine_name: [0; xr::MAX_ENGINE_NAME_SIZE],
        engine_version: 0,
        api_version: xr::Version::new(1, 0, 0),
    };
    for (d, s) in app.application_name.iter_mut().zip(b"XRMimic vk_client") {
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
    let vk_reqs = func!(inst, "xrGetVulkanGraphicsRequirements2KHR", xr::pfn::GetVulkanGraphicsRequirements2KHR);
    let create_vk_instance = func!(inst, "xrCreateVulkanInstanceKHR", xr::pfn::CreateVulkanInstanceKHR);
    let get_vk_device = func!(inst, "xrGetVulkanGraphicsDevice2KHR", xr::pfn::GetVulkanGraphicsDevice2KHR);
    let create_vk_device = func!(inst, "xrCreateVulkanDeviceKHR", xr::pfn::CreateVulkanDeviceKHR);
    let create_session = func!(inst, "xrCreateSession", xr::pfn::CreateSession);
    let poll_event = func!(inst, "xrPollEvent", xr::pfn::PollEvent);
    let begin_session = func!(inst, "xrBeginSession", xr::pfn::BeginSession);
    let end_session = func!(inst, "xrEndSession", xr::pfn::EndSession);
    let request_exit = func!(inst, "xrRequestExitSession", xr::pfn::RequestExitSession);
    let destroy_session = func!(inst, "xrDestroySession", xr::pfn::DestroySession);
    let destroy_instance = func!(inst, "xrDestroyInstance", xr::pfn::DestroyInstance);
    let create_ref_space = func!(inst, "xrCreateReferenceSpace", xr::pfn::CreateReferenceSpace);
    let enum_formats = func!(inst, "xrEnumerateSwapchainFormats", xr::pfn::EnumerateSwapchainFormats);
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
    let mut gr: xr::GraphicsRequirementsVulkanKHR = std::mem::zeroed();
    gr.ty = xr::GraphicsRequirementsVulkanKHR::TYPE;
    check!(vk_reqs(inst, system, &mut gr));

    // ------------------------------------------------ Vulkan through the runtime
    let entry = ash::Entry::load().expect("vulkan-1.dll");
    let vk_gipa: xr::platform::VkGetInstanceProcAddr = std::mem::transmute(entry.static_fn().get_instance_proc_addr);
    let app_info = vk::ApplicationInfo::default().api_version(vk::make_api_version(0, 1, 1, 0));
    let vk_ici = vk::InstanceCreateInfo::default().application_info(&app_info);
    let xr_ici = xr::VulkanInstanceCreateInfoKHR {
        ty: xr::VulkanInstanceCreateInfoKHR::TYPE,
        next: std::ptr::null(),
        system_id: system,
        create_flags: xr::VulkanInstanceCreateFlagsKHR::EMPTY,
        pfn_get_instance_proc_addr: Some(vk_gipa),
        vulkan_create_info: &vk_ici as *const _ as *const c_void,
        vulkan_allocator: std::ptr::null(),
    };
    let mut raw_instance: xr::platform::VkInstance = std::ptr::null();
    let mut vk_result: xr::platform::VkResult = 0;
    check!(create_vk_instance(inst, &xr_ici, &mut raw_instance, &mut vk_result));
    assert_eq!(vk_result, 0, "vkCreateInstance");
    let instance = ash::Instance::load(entry.static_fn(), vk::Instance::from_raw(raw_instance as u64));
    let dgi = xr::VulkanGraphicsDeviceGetInfoKHR {
        ty: xr::VulkanGraphicsDeviceGetInfoKHR::TYPE,
        next: std::ptr::null(),
        system_id: system,
        vulkan_instance: raw_instance,
    };
    let mut raw_physical: xr::platform::VkPhysicalDevice = std::ptr::null();
    check!(get_vk_device(inst, &dgi, &mut raw_physical));
    let physical = vk::PhysicalDevice::from_raw(raw_physical as u64);
    let family = instance
        .get_physical_device_queue_family_properties(physical)
        .iter()
        .position(|q| q.queue_flags.contains(vk::QueueFlags::GRAPHICS))
        .expect("a graphics queue") as u32;
    let priorities = [1.0f32];
    let queues = [vk::DeviceQueueCreateInfo::default().queue_family_index(family).queue_priorities(&priorities)];
    let vk_dci = vk::DeviceCreateInfo::default().queue_create_infos(&queues);
    let xr_dci = xr::VulkanDeviceCreateInfoKHR {
        ty: xr::VulkanDeviceCreateInfoKHR::TYPE,
        next: std::ptr::null(),
        system_id: system,
        create_flags: xr::VulkanDeviceCreateFlagsKHR::EMPTY,
        pfn_get_instance_proc_addr: Some(vk_gipa),
        vulkan_physical_device: raw_physical,
        vulkan_create_info: &vk_dci as *const _ as *const c_void,
        vulkan_allocator: std::ptr::null(),
    };
    let mut raw_device: xr::platform::VkDevice = std::ptr::null();
    check!(create_vk_device(inst, &xr_dci, &mut raw_device, &mut vk_result));
    assert_eq!(vk_result, 0, "vkCreateDevice");
    let device = ash::Device::load(instance.fp_v1_0(), vk::Device::from_raw(raw_device as u64));
    let queue = device.get_device_queue(family, 0);
    let mem_props = instance.get_physical_device_memory_properties(physical);
    let pool = device
        .create_command_pool(
            &vk::CommandPoolCreateInfo::default()
                .queue_family_index(family)
                .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER),
            None,
        )
        .unwrap();
    let cmd = device
        .allocate_command_buffers(&vk::CommandBufferAllocateInfo::default().command_pool(pool).command_buffer_count(1))
        .unwrap()[0];
    let fence = device.create_fence(&vk::FenceCreateInfo::default(), None).unwrap();

    // ------------------------------------------------ session
    let binding = xr::GraphicsBindingVulkanKHR {
        ty: xr::GraphicsBindingVulkanKHR::TYPE,
        next: std::ptr::null(),
        instance: raw_instance,
        physical_device: raw_physical,
        device: raw_device,
        queue_family_index: family,
        queue_index: 0,
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
        for _ in 0..2000 {
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
    let rci = xr::ReferenceSpaceCreateInfo {
        ty: xr::ReferenceSpaceCreateInfo::TYPE,
        next: std::ptr::null(),
        reference_space_type: xr::ReferenceSpaceType::LOCAL,
        pose_in_reference_space: xr::Posef::IDENTITY,
    };
    let mut space = xr::Space::NULL;
    check!(create_ref_space(session, &rci, &mut space));

    // ------------------------------------------------ swapchains
    let mut n = 0u32;
    check!(enum_formats(session, 0, &mut n, std::ptr::null_mut()));
    let mut formats = vec![0i64; n as usize];
    check!(enum_formats(session, n, &mut n, formats.as_mut_ptr()));
    let colour = vk::Format::R8G8B8A8_UNORM;
    let depth_format = if depth16 {
        vk::Format::D16_UNORM
    } else if d32s8 {
        vk::Format::D32_SFLOAT_S8_UINT
    } else {
        vk::Format::D32_SFLOAT
    };
    for f in [colour, depth_format] {
        if !formats.contains(&(f.as_raw() as i64)) {
            eprintln!("FAIL: the runtime does not offer {f:?} (offers {formats:?})");
            return 1;
        }
    }
    println!("ok   swapchain formats are Vulkan formats: {formats:?}");
    const EYE: u32 = 256;
    const QUAD: u32 = 64;
    let make_swapchain = |format: vk::Format, w: u32, samples: u32, usage: xr::SwapchainUsageFlags| {
        let ci = xr::SwapchainCreateInfo {
            ty: xr::SwapchainCreateInfo::TYPE,
            next: std::ptr::null(),
            create_flags: xr::SwapchainCreateFlags::EMPTY,
            usage_flags: usage | xr::SwapchainUsageFlags::TRANSFER_DST,
            format: format.as_raw() as i64,
            sample_count: samples,
            width: w,
            height: w,
            face_count: 1,
            array_size: 1,
            mip_count: 1,
        };
        let mut sc = xr::Swapchain::NULL;
        check!(create_swapchain(session, &ci, &mut sc));
        let mut n = 0u32;
        check!(enum_images(sc, 0, &mut n, std::ptr::null_mut()));
        let mut imgs = vec![
            xr::SwapchainImageVulkanKHR { ty: xr::SwapchainImageVulkanKHR::TYPE, next: std::ptr::null_mut(), image: 0 };
            n as usize
        ];
        check!(enum_images(sc, n, &mut n, imgs.as_mut_ptr() as *mut xr::SwapchainImageBaseHeader));
        (sc, imgs.iter().map(|i| vk::Image::from_raw(i.image)).collect::<Vec<_>>())
    };
    let samples = if msaa { 4 } else { 1 };
    let eyes: Vec<_> =
        (0..2).map(|_| make_swapchain(colour, EYE, samples, xr::SwapchainUsageFlags::COLOR_ATTACHMENT)).collect();
    let depths: Vec<_> = if depth_mode {
        (0..2).map(|_| make_swapchain(depth_format, EYE, 1, xr::SwapchainUsageFlags::DEPTH_STENCIL_ATTACHMENT)).collect()
    } else {
        Vec::new()
    };
    let quad = make_swapchain(colour, QUAD, 1, xr::SwapchainUsageFlags::COLOR_ATTACHMENT);

    // The eyes' pictures: top half and bottom half colours, in host buffers.
    let patterns = [([255u8, 0, 0], [255u8, 255, 0]), ([0u8, 255, 0], [0u8, 255, 255])];
    let host = |bytes: &[u8]| {
        let buf = device
            .create_buffer(
                &vk::BufferCreateInfo::default().size(bytes.len() as u64).usage(vk::BufferUsageFlags::TRANSFER_SRC),
                None,
            )
            .unwrap();
        let req = device.get_buffer_memory_requirements(buf);
        let want = vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT;
        let ty = (0..mem_props.memory_type_count)
            .find(|&i| {
                req.memory_type_bits & (1 << i) != 0 && mem_props.memory_types[i as usize].property_flags.contains(want)
            })
            .unwrap();
        let mem = device
            .allocate_memory(&vk::MemoryAllocateInfo::default().allocation_size(req.size).memory_type_index(ty), None)
            .unwrap();
        device.bind_buffer_memory(buf, mem, 0).unwrap();
        let p = device.map_memory(mem, 0, bytes.len() as u64, vk::MemoryMapFlags::empty()).unwrap();
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
        device.unmap_memory(mem);
        buf
    };
    let pictures: Vec<vk::Buffer> = patterns
        .iter()
        .map(|(top, bottom)| {
            let mut px = Vec::with_capacity((EYE * EYE * 4) as usize);
            for y in 0..EYE {
                let c = if y < EYE / 2 { top } else { bottom };
                for _ in 0..EYE {
                    px.extend_from_slice(&[c[0], c[1], c[2], 255]);
                }
            }
            host(&px)
        })
        .collect();

    // Depth: the left eye with ordinary Z (0.1..100 m), the right with
    // reversed Z (infinite, or Godot's finite 4000..0.05 m); both from 1 m at
    // the top row to 8 m at the bottom.
    let depth_eyes = [(0.1f32, 100.0f32), if d32s8 { (4000.0f32, 0.05f32) } else { (f32::INFINITY, 0.1) }];
    let row_metres = |y: f32| 1.0 + 7.0 * y / (EYE - 1) as f32;
    let depth_pictures: Vec<vk::Buffer> = if depth_mode {
        depth_eyes
            .iter()
            .map(|&(near, far)| {
                let mut bytes = Vec::new();
                for y in 0..EYE {
                    let z = depth_value(row_metres(y as f32), 0.0, 1.0, near, far);
                    for _ in 0..EYE {
                        if depth16 {
                            bytes.extend_from_slice(&((z * 65535.0).round() as u16).to_le_bytes());
                        } else {
                            bytes.extend_from_slice(&z.to_le_bytes());
                        }
                    }
                }
                host(&bytes)
            })
            .collect()
    } else {
        Vec::new()
    };

    let colour_range = vk::ImageSubresourceRange {
        aspect_mask: vk::ImageAspectFlags::COLOR,
        base_mip_level: 0,
        level_count: 1,
        base_array_layer: 0,
        layer_count: 1,
    };
    // Layout changes take both aspects of a depth/stencil image.
    let depth_aspects =
        if d32s8 { vk::ImageAspectFlags::DEPTH | vk::ImageAspectFlags::STENCIL } else { vk::ImageAspectFlags::DEPTH };
    let depth_range = vk::ImageSubresourceRange { aspect_mask: depth_aspects, ..colour_range };
    let to = |image, range, from: vk::ImageLayout, to: vk::ImageLayout| {
        vk::ImageMemoryBarrier::default()
            .src_access_mask(vk::AccessFlags::MEMORY_WRITE)
            .dst_access_mask(vk::AccessFlags::MEMORY_READ | vk::AccessFlags::MEMORY_WRITE)
            .old_layout(from)
            .new_layout(to)
            .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
            .image(image)
            .subresource_range(range)
    };
    let barrier = |b: vk::ImageMemoryBarrier| {
        device.cmd_pipeline_barrier(
            cmd,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::PipelineStageFlags::ALL_COMMANDS,
            vk::DependencyFlags::empty(),
            &[],
            &[],
            &[b],
        );
    };
    let attach = vk::ImageLayout::COLOR_ATTACHMENT_OPTIMAL;
    let dst = vk::ImageLayout::TRANSFER_DST_OPTIMAL;
    let acquire_image = |sc: xr::Swapchain| {
        let mut idx = 0u32;
        check!(acquire(sc, std::ptr::null(), &mut idx));
        let wi = xr::SwapchainImageWaitInfo {
            ty: xr::SwapchainImageWaitInfo::TYPE,
            next: std::ptr::null(),
            timeout: xr::Duration::INFINITE,
        };
        check!(wait_image(sc, &wi));
        idx as usize
    };
    let sub_image = |sc: xr::Swapchain, w: u32| xr::SwapchainSubImage {
        swapchain: sc,
        image_rect: xr::Rect2Di { offset: xr::Offset2Di { x: 0, y: 0 }, extent: xr::Extent2Di { width: w as i32, height: w as i32 } },
        image_array_index: 0,
    };

    // ------------------------------------------------ frames
    let frame_count = if snapshot.is_some() { 400 } else { 60 * 60 };
    let mut last_views = [xr::View { ty: xr::View::TYPE, next: std::ptr::null_mut(), pose: xr::Posef::IDENTITY, fov: std::mem::zeroed() }; 2];
    for _ in 0..frame_count {
        let mut fs = xr::FrameState { ty: xr::FrameState::TYPE, next: std::ptr::null_mut(), ..std::mem::zeroed() };
        check!(wait_frame(session, std::ptr::null(), &mut fs));
        check!(begin_frame(session, std::ptr::null()));
        let vli = xr::ViewLocateInfo {
            ty: xr::ViewLocateInfo::TYPE,
            next: std::ptr::null(),
            view_configuration_type: xr::ViewConfigurationType::PRIMARY_STEREO,
            display_time: fs.predicted_display_time,
            space,
        };
        let mut vs = xr::ViewState { ty: xr::ViewState::TYPE, next: std::ptr::null_mut(), view_state_flags: xr::ViewStateFlags::EMPTY };
        let mut views = last_views;
        let mut vn = 0u32;
        check!(locate_views(session, &vli, &mut vs, 2, &mut vn, views.as_mut_ptr()));
        last_views = views;

        // Paint every swapchain this frame uses, then hand them back.
        let eye_idx: Vec<usize> = eyes.iter().map(|(sc, _)| acquire_image(*sc)).collect();
        let depth_idx: Vec<usize> = depths.iter().map(|(sc, _)| acquire_image(*sc)).collect();
        let quad_idx = acquire_image(quad.0);
        device.reset_command_buffer(cmd, vk::CommandBufferResetFlags::empty()).unwrap();
        device.begin_command_buffer(cmd, &vk::CommandBufferBeginInfo::default()).unwrap();
        for e in 0..2 {
            let img = eyes[e].1[eye_idx[e]];
            barrier(to(img, colour_range, attach, dst));
            if msaa {
                let (top, _) = patterns[e];
                let c = vk::ClearColorValue { float32: [top[0] as f32 / 255.0, top[1] as f32 / 255.0, top[2] as f32 / 255.0, 1.0] };
                device.cmd_clear_color_image(cmd, img, dst, &c, &[colour_range]);
            } else {
                let region = vk::BufferImageCopy {
                    buffer_offset: 0,
                    buffer_row_length: 0,
                    buffer_image_height: 0,
                    image_subresource: vk::ImageSubresourceLayers {
                        aspect_mask: vk::ImageAspectFlags::COLOR,
                        mip_level: 0,
                        base_array_layer: 0,
                        layer_count: 1,
                    },
                    image_offset: vk::Offset3D::default(),
                    image_extent: vk::Extent3D { width: EYE, height: EYE, depth: 1 },
                };
                device.cmd_copy_buffer_to_image(cmd, pictures[e], img, dst, &[region]);
            }
            barrier(to(img, colour_range, dst, attach));
        }
        for (e, (_, imgs)) in depths.iter().enumerate() {
            let img = imgs[depth_idx[e]];
            let ds = vk::ImageLayout::DEPTH_STENCIL_ATTACHMENT_OPTIMAL;
            barrier(to(img, depth_range, ds, dst));
            let region = vk::BufferImageCopy {
                buffer_offset: 0,
                buffer_row_length: 0,
                buffer_image_height: 0,
                image_subresource: vk::ImageSubresourceLayers {
                    aspect_mask: vk::ImageAspectFlags::DEPTH,
                    mip_level: 0,
                    base_array_layer: 0,
                    layer_count: 1,
                },
                image_offset: vk::Offset3D::default(),
                image_extent: vk::Extent3D { width: EYE, height: EYE, depth: 1 },
            };
            device.cmd_copy_buffer_to_image(cmd, depth_pictures[e], img, dst, &[region]);
            barrier(to(img, depth_range, dst, ds));
        }
        let qimg = quad.1[quad_idx];
        barrier(to(qimg, colour_range, attach, dst));
        device.cmd_clear_color_image(cmd, qimg, dst, &vk::ClearColorValue { float32: [0.0, 0.0, 1.0, 1.0] }, &[colour_range]);
        barrier(to(qimg, colour_range, dst, attach));
        device.end_command_buffer(cmd).unwrap();
        let cmds = [cmd];
        device.queue_submit(queue, &[vk::SubmitInfo::default().command_buffers(&cmds)], fence).unwrap();
        device.wait_for_fences(&[fence], true, u64::MAX).unwrap();
        device.reset_fences(&[fence]).unwrap();
        for (sc, _) in eyes.iter().chain(depths.iter()).chain(std::iter::once(&quad)) {
            check!(release(*sc, std::ptr::null()));
        }

        let depth_infos: Vec<xr::CompositionLayerDepthInfoKHR> = depths
            .iter()
            .enumerate()
            .map(|(e, (sc, _))| xr::CompositionLayerDepthInfoKHR {
                ty: xr::CompositionLayerDepthInfoKHR::TYPE,
                next: std::ptr::null(),
                sub_image: sub_image(*sc, EYE),
                min_depth: 0.0,
                max_depth: 1.0,
                near_z: depth_eyes[e].0,
                far_z: depth_eyes[e].1,
            })
            .collect();
        let proj_views: Vec<xr::CompositionLayerProjectionView> = (0..2)
            .map(|e| xr::CompositionLayerProjectionView {
                ty: xr::CompositionLayerProjectionView::TYPE,
                next: depth_infos.get(e).map_or(std::ptr::null(), |d| d as *const _ as *const c_void),
                pose: views[e].pose,
                fov: views[e].fov,
                sub_image: sub_image(eyes[e].0, EYE),
            })
            .collect();
        let proj = xr::CompositionLayerProjection {
            ty: xr::CompositionLayerProjection::TYPE,
            next: std::ptr::null(),
            layer_flags: xr::CompositionLayerFlags::EMPTY,
            space,
            view_count: 2,
            views: proj_views.as_ptr(),
        };
        let quad_layer = xr::CompositionLayerQuad {
            ty: xr::CompositionLayerQuad::TYPE,
            next: std::ptr::null(),
            layer_flags: xr::CompositionLayerFlags::EMPTY,
            space,
            eye_visibility: xr::EyeVisibility::BOTH,
            sub_image: sub_image(quad.0, QUAD),
            pose: xr::Posef { orientation: xr::Quaternionf::IDENTITY, position: xr::Vector3f { x: 0.0, y: 0.0, z: -1.5 } },
            size: xr::Extent2Df { width: 0.6, height: 0.6 },
        };
        let layers: Vec<*const xr::CompositionLayerBaseHeader> = if depth_mode {
            vec![&proj as *const _ as *const _]
        } else {
            vec![&proj as *const _ as *const _, &quad_layer as *const _ as *const _]
        };
        let fei = xr::FrameEndInfo {
            ty: xr::FrameEndInfo::TYPE,
            next: std::ptr::null(),
            display_time: fs.predicted_display_time,
            environment_blend_mode: xr::EnvironmentBlendMode::OPAQUE,
            layer_count: layers.len() as u32,
            layers: layers.as_ptr(),
        };
        check!(end_frame(session, &fei));
    }
    check!(request_exit(session));
    next_state(xr::SessionState::STOPPING);
    check!(end_session(session));
    device.device_wait_idle().unwrap();
    check!(destroy_session(session));
    check!(destroy_instance(inst));

    // ------------------------------------------------ the window's picture
    let Some(snapshot) = snapshot else {
        println!("shown (set XRMIMIC_TEST_SNAPSHOT=<out.bmp> to check the picture)");
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
    let (vw0, vh0) = (w as f32, (h - 34) as f32);
    let aspect = 2.0f32;
    let (mut vw, mut vh) = (vw0, vw0 / aspect);
    if vh > vh0 {
        vh = vh0;
        vw = vh0 * aspect;
    }
    let (x0, y0) = ((vw0 - vw) * 0.5, 34.0 + (vh0 - vh) * 0.5);
    let at = |e: usize, fx: f32, fy: f32| {
        ((x0 + e as f32 * vw * 0.5 + fx * vw * 0.5).round() as i32, (y0 + fy * vh).round() as i32)
    };
    let near = |c: [u8; 3], want: [u8; 3]| c.iter().zip(want).all(|(a, b)| (*a as i32 - b as i32).abs() < 40);
    let mut ok = true;
    let mut expect = |what: String, (x, y): (i32, i32), want: [u8; 3]| {
        let c = px(x, y);
        let good = near(c, want);
        println!("{} {what} at ({x}, {y}): rgb {c:?}, want {want:?}", if good { "ok  " } else { "FAIL" });
        ok &= good;
    };
    let side = ["left", "right"];
    for e in 0..2 {
        if depth_mode {
            for (fx, fy) in [(0.5f32, 0.1f32), (0.15, 0.3), (0.5, 0.5), (0.85, 0.7), (0.3, 0.9)] {
                let metres = row_metres(fy * EYE as f32 - 0.5);
                let g = (depth_shade(metres) * 255.0).round() as u8;
                expect(format!("{} eye depth, {metres:.2} m, at ({fx}, {fy})", side[e]), at(e, fx, fy), [g, g, g]);
            }
        } else if msaa {
            expect(format!("{} eye (MSAA, resolved)", side[e]), at(e, 0.15, 0.15), patterns[e].0);
            expect(format!("{} eye, lower corner (MSAA)", side[e]), at(e, 0.85, 0.85), patterns[e].0);
        } else {
            expect(format!("{} eye, top half", side[e]), at(e, 0.15, 0.15), patterns[e].0);
            expect(format!("{} eye, bottom half", side[e]), at(e, 0.15, 0.85), patterns[e].1);
        }
        if !depth_mode {
            // The quad straight ahead covers the middle of each eye.
            expect(format!("{} eye, blue quad ahead", side[e]), at(e, 0.5 + if e == 0 { 0.02 } else { -0.02 }, 0.5), [0, 0, 255]);
        }
    }
    println!("{}", if ok { "PASS" } else { "FAIL" });
    if ok {
        0
    } else {
        1
    }
}
