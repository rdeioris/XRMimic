//! Instance, system, session, space, swapchain and frame entry points.

use crate::gfx::{self, SessionGfx};
use crate::headset::Window;
use crate::shared::Shared;
use crate::state::*;
use crate::{time, xlog};
use openxr_sys as xr;
use std::collections::{HashMap, VecDeque};
use std::ffi::{c_char, CStr};
use std::sync::{Arc, Mutex};
use xr::Handle as _;
use xrmimic_core::math::{Fov, Pose, Quat, Vec3};
use xrmimic_core::sim::SimState;
use xrmimic_core::timing::FrameClock;

pub const SYSTEM_ID: u64 = 0x5153_494d; // "QSIM"

/// (name, version) of every extension we implement: the graphics backend's
/// plus the platform-neutral ones.
pub fn extensions() -> Vec<(&'static str, u32)> {
    // Always: what the graphics backend needs. Then whatever the device
    // settings (Device tab) tick, from the optional ones xrmimic implements.
    let mut v = gfx::EXTENSIONS.to_vec();
    let device = crate::devstore::current();
    for (name, version, _) in xrmimic_core::device::OPTIONAL_EXTENSIONS {
        if device.extensions.iter().any(|e| e == name) {
            v.push((name, *version));
        }
    }
    v
}

// ---------------------------------------------------------------- helpers

pub fn write_str<const N: usize>(dst: &mut [c_char; N], s: &str) {
    let n = s.len().min(N - 1);
    for (d, b) in dst.iter_mut().zip(&s.as_bytes()[..n]) {
        *d = *b as c_char;
    }
    dst[n] = 0;
}

pub fn read_str(src: &[c_char]) -> String {
    let bytes: Vec<u8> = src.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// Two-call idiom for plain values.
pub unsafe fn fill<T: Copy>(capacity: u32, count: *mut u32, out: *mut T, items: &[T]) -> xr::Result {
    if count.is_null() {
        return xr::Result::ERROR_VALIDATION_FAILURE;
    }
    *count = items.len() as u32;
    if capacity == 0 {
        return xr::Result::SUCCESS;
    }
    if (capacity as usize) < items.len() {
        return xr::Result::ERROR_SIZE_INSUFFICIENT;
    }
    std::ptr::copy_nonoverlapping(items.as_ptr(), out, items.len());
    xr::Result::SUCCESS
}

pub fn to_xr(p: &Pose) -> xr::Posef {
    xr::Posef {
        orientation: xr::Quaternionf {
            x: p.orientation.x,
            y: p.orientation.y,
            z: p.orientation.z,
            w: p.orientation.w,
        },
        position: xr::Vector3f { x: p.position.x, y: p.position.y, z: p.position.z },
    }
}

pub fn from_xr(p: &xr::Posef) -> Pose {
    Pose::new(
        Quat { x: p.orientation.x, y: p.orientation.y, z: p.orientation.z, w: p.orientation.w }.normalized(),
        Vec3::new(p.position.x, p.position.y, p.position.z),
    )
}

fn fov_xr(f: Fov) -> xr::Fovf {
    xr::Fovf { angle_left: f.left, angle_right: f.right, angle_up: f.up, angle_down: f.down }
}

/// Walk an input `next` chain for a structure of type `ty`.
pub unsafe fn find_in_chain(mut next: *const std::ffi::c_void, ty: xr::StructureType) -> *const xr::BaseInStructure {
    while !next.is_null() {
        let b = next as *const xr::BaseInStructure;
        if (*b).ty == ty {
            return b;
        }
        next = (*b).next as *const _;
    }
    std::ptr::null()
}

fn tracked_flags() -> xr::SpaceLocationFlags {
    xr::SpaceLocationFlags::ORIENTATION_VALID
        | xr::SpaceLocationFlags::POSITION_VALID
        | xr::SpaceLocationFlags::ORIENTATION_TRACKED
        | xr::SpaceLocationFlags::POSITION_TRACKED
}

// ---------------------------------------------------------------- instance

pub unsafe extern "system" fn enumerate_api_layers(
    _capacity: u32,
    count: *mut u32,
    _props: *mut xr::ApiLayerProperties,
) -> xr::Result {
    crate::traced!("xrEnumerateApiLayerProperties", [_capacity, count, _props], || {
        *count = 0;
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn enumerate_extensions(
    layer: *const c_char,
    capacity: u32,
    count: *mut u32,
    props: *mut xr::ExtensionProperties,
) -> xr::Result {
    crate::traced!("xrEnumerateInstanceExtensionProperties", [layer, capacity, count, props], || {
        let exts = extensions();
        if !layer.is_null() {
            return xr::Result::ERROR_API_LAYER_NOT_PRESENT;
        }
        *count = exts.len() as u32;
        if capacity == 0 {
            return xr::Result::SUCCESS;
        }
        if (capacity as usize) < exts.len() {
            return xr::Result::ERROR_SIZE_INSUFFICIENT;
        }
        for (i, (name, ver)) in exts.iter().enumerate() {
            let p = &mut *props.add(i);
            write_str(&mut p.extension_name, name);
            p.extension_version = *ver;
        }
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn create_instance(
    info: *const xr::InstanceCreateInfo,
    out: *mut xr::Instance,
) -> xr::Result {
    crate::traced!("xrCreateInstance", [info, out], || {
        let info = &*info;
        let app = read_str(&info.application_info.application_name);
        let engine = read_str(&info.application_info.engine_name);
        let supported = extensions();
        let mut exts = Vec::new();
        for i in 0..info.enabled_extension_count as usize {
            let e = CStr::from_ptr(*info.enabled_extension_names.add(i)).to_string_lossy().into_owned();
            if !supported.iter().any(|(n, _)| *n == e) {
                xlog!("xrCreateInstance: app wants unsupported extension {e}");
                return xr::Result::ERROR_EXTENSION_NOT_PRESENT;
            }
            exts.push(e);
        }
        xlog!(
            "xrCreateInstance: app '{app}' engine '{engine}' api {} extensions {exts:?}",
            info.application_info.api_version
        );
        let api = info.application_info.api_version;
        if api.major() != 1 || api.minor() > 1 {
            return xr::Result::ERROR_API_VERSION_UNSUPPORTED;
        }
        let mut rt = rt();
        let h = rt.new_handle();
        rt.instances.insert(
            h,
            InstanceData { events: VecDeque::new(), app_name: app, extensions: exts, suggested: HashMap::new() },
        );
        *out = xr::Instance::from_raw(h);
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn destroy_instance(instance: xr::Instance) -> xr::Result {
    crate::traced!("xrDestroyInstance", [instance], || {
        let mut rt = rt();
        if !rt.instances.contains_key(&instance.into_raw()) {
            return xr::Result::ERROR_HANDLE_INVALID;
        }
        rt.drop_instance(instance.into_raw());
        for (name, n) in crate::log::counts() {
            xlog!("  calls {name}: {n}");
        }
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn instance_properties(
    instance: xr::Instance,
    props: *mut xr::InstanceProperties,
) -> xr::Result {
    crate::traced!("xrGetInstanceProperties", [instance, props], || {
        if !rt().instances.contains_key(&instance.into_raw()) {
            return xr::Result::ERROR_HANDLE_INVALID;
        }
        (*props).runtime_version = xr::Version::new(0, 1, 0);
        write_str(&mut (*props).runtime_name, "XRMimic");
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn poll_event(instance: xr::Instance, buf: *mut xr::EventDataBuffer) -> xr::Result {
    crate::traced!("xrPollEvent", [instance, buf], || {
        let mut rt = rt();
        // A closed window asks the running session to stop.
        let closing: Vec<u64> = rt
            .sessions
            .iter()
            .filter(|(_, s)| {
                s.instance == instance.into_raw()
                    && s.running
                    && !s.exit_requested
                    && s.gfx.shared.close_requested.load(std::sync::atomic::Ordering::SeqCst)
            })
            .map(|(k, _)| *k)
            .collect();
        for s in closing {
            xlog!("window closed: stopping session {s}");
            rt.sessions.get_mut(&s).unwrap().exit_requested = true;
            rt.set_state(s, xr::SessionState::STOPPING);
        }
        // Controllers put down or picked up: the interaction profile follows.
        let mine: Vec<u64> =
            rt.sessions.iter().filter(|(_, s)| s.instance == instance.into_raw()).map(|(k, _)| *k).collect();
        let play_area = crate::devstore::current().play_area;
        for s in mine {
            crate::input::refresh_profile(&mut rt, s, false);
            // The Device tab changed the play area: STAGE's bounds change.
            let sd = rt.sessions.get_mut(&s).unwrap();
            if sd.stage_bounds != play_area {
                sd.stage_bounds = play_area;
                let (instance, time) = (sd.instance, crate::time::now_ns());
                rt.push_event(instance, Event::StageChanged { session: s, time });
            }
        }
        let Some(inst) = rt.instances.get_mut(&instance.into_raw()) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        let Some(e) = inst.events.pop_front() else {
            return xr::Result::EVENT_UNAVAILABLE;
        };
        match e {
            Event::SessionState { session, state, time } => {
                (buf as *mut xr::EventDataSessionStateChanged).write(xr::EventDataSessionStateChanged {
                    ty: xr::EventDataSessionStateChanged::TYPE,
                    next: std::ptr::null(),
                    session: xr::Session::from_raw(session),
                    state,
                    time: xr::Time::from_nanos(time),
                });
                xlog!("event: session {session} state {state:?}");
            }
            Event::ProfileChanged { session } => {
                (buf as *mut xr::EventDataInteractionProfileChanged).write(
                    xr::EventDataInteractionProfileChanged {
                        ty: xr::EventDataInteractionProfileChanged::TYPE,
                        next: std::ptr::null(),
                        session: xr::Session::from_raw(session),
                    },
                );
                xlog!("event: interaction profile changed");
            }
            Event::StageChanged { session, time } => {
                (buf as *mut xr::EventDataReferenceSpaceChangePending).write(xr::EventDataReferenceSpaceChangePending {
                    ty: xr::EventDataReferenceSpaceChangePending::TYPE,
                    next: std::ptr::null(),
                    session: xr::Session::from_raw(session),
                    reference_space_type: xr::ReferenceSpaceType::STAGE,
                    change_time: xr::Time::from_nanos(time),
                    pose_valid: xr::TRUE,
                    pose_in_previous_space: to_xr(&Pose::IDENTITY),
                });
                xlog!("event: STAGE bounds changed");
            }
        }
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn result_to_string(
    _instance: xr::Instance,
    value: xr::Result,
    buffer: *mut c_char,
) -> xr::Result {
    crate::traced!("xrResultToString", [_instance, value, buffer], || {
        let s = format!("{value:?}");
        let s = if s.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
            format!("XR_UNKNOWN_{}_{s}", if value.into_raw() < 0 { "FAILURE" } else { "SUCCESS" })
        } else {
            format!("XR_{s}")
        };
        write_str(&mut *(buffer as *mut [c_char; xr::MAX_RESULT_STRING_SIZE]), &s);
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn structure_type_to_string(
    _instance: xr::Instance,
    value: xr::StructureType,
    buffer: *mut c_char,
) -> xr::Result {
    crate::traced!("xrStructureTypeToString", [_instance, value, buffer], || {
        let s = format!("XR_TYPE_{value:?}");
        write_str(&mut *(buffer as *mut [c_char; xr::MAX_STRUCTURE_NAME_SIZE]), &s);
        xr::Result::SUCCESS
    })
}

// ---------------------------------------------------------------- system

pub unsafe extern "system" fn get_system(
    instance: xr::Instance,
    info: *const xr::SystemGetInfo,
    out: *mut xr::SystemId,
) -> xr::Result {
    crate::traced!("xrGetSystem", [instance, info, out], || {
        if !rt().instances.contains_key(&instance.into_raw()) {
            return xr::Result::ERROR_HANDLE_INVALID;
        }
        if (*info).form_factor != xr::FormFactor::HEAD_MOUNTED_DISPLAY {
            return xr::Result::ERROR_FORM_FACTOR_UNSUPPORTED;
        }
        *out = xr::SystemId::from_raw(SYSTEM_ID);
        xr::Result::SUCCESS
    })
}

pub fn check_system(instance: xr::Instance, system: xr::SystemId) -> Option<xr::Result> {
    if !rt().instances.contains_key(&instance.into_raw()) {
        return Some(xr::Result::ERROR_HANDLE_INVALID);
    }
    if system.into_raw() != SYSTEM_ID {
        return Some(xr::Result::ERROR_SYSTEM_INVALID);
    }
    None
}

pub unsafe extern "system" fn system_properties(
    instance: xr::Instance,
    system: xr::SystemId,
    props: *mut xr::SystemProperties,
) -> xr::Result {
    crate::traced!("xrGetSystemProperties", [instance, system, props], || {
        if let Some(e) = check_system(instance, system) {
            return e;
        }
        let d = crate::devstore::current();
        let p = &mut *props;
        p.system_id = system;
        p.vendor_id = d.vendor_id;
        write_str(&mut p.system_name, &d.name);
        p.graphics_properties = xr::SystemGraphicsProperties {
            max_swapchain_image_width: d.max_eye_width * 2,
            max_swapchain_image_height: d.max_eye_height,
            max_layer_count: 16,
        };
        p.tracking_properties =
            xr::SystemTrackingProperties { orientation_tracking: xr::TRUE, position_tracking: xr::TRUE };
        // XR_EXT_hand_tracking: hands are tracked when the extension is offered.
        let ht = find_in_chain(p.next as *const _, xr::StructureType::SYSTEM_HAND_TRACKING_PROPERTIES_EXT)
            as *mut xr::SystemHandTrackingPropertiesEXT;
        if !ht.is_null() {
            (*ht).supports_hand_tracking = d.extensions.iter().any(|e| e == "XR_EXT_hand_tracking").into();
        }
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn enumerate_view_configurations(
    instance: xr::Instance,
    system: xr::SystemId,
    capacity: u32,
    count: *mut u32,
    out: *mut xr::ViewConfigurationType,
) -> xr::Result {
    crate::traced!("xrEnumerateViewConfigurations", [instance, system, capacity, count, out], || {
        if let Some(e) = check_system(instance, system) {
            return e;
        }
        fill(capacity, count, out, &[xr::ViewConfigurationType::PRIMARY_STEREO])
    })
}

pub unsafe extern "system" fn view_configuration_properties(
    instance: xr::Instance,
    system: xr::SystemId,
    ty: xr::ViewConfigurationType,
    props: *mut xr::ViewConfigurationProperties,
) -> xr::Result {
    crate::traced!("xrGetViewConfigurationProperties", [instance, system, ty, props], || {
        if let Some(e) = check_system(instance, system) {
            return e;
        }
        if ty != xr::ViewConfigurationType::PRIMARY_STEREO {
            return xr::Result::ERROR_VIEW_CONFIGURATION_TYPE_UNSUPPORTED;
        }
        (*props).view_configuration_type = ty;
        (*props).fov_mutable = xr::FALSE;
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn enumerate_views(
    instance: xr::Instance,
    system: xr::SystemId,
    ty: xr::ViewConfigurationType,
    capacity: u32,
    count: *mut u32,
    out: *mut xr::ViewConfigurationView,
) -> xr::Result {
    crate::traced!("xrEnumerateViewConfigurationViews", [instance, system, ty, capacity, count, out], || {
        if let Some(e) = check_system(instance, system) {
            return e;
        }
        if ty != xr::ViewConfigurationType::PRIMARY_STEREO {
            return xr::Result::ERROR_VIEW_CONFIGURATION_TYPE_UNSUPPORTED;
        }
        *count = 2;
        if capacity == 0 {
            return xr::Result::SUCCESS;
        }
        if capacity < 2 {
            return xr::Result::ERROR_SIZE_INSUFFICIENT;
        }
        let d = crate::devstore::current();
        for i in 0..2 {
            let v = &mut *out.add(i);
            v.recommended_image_rect_width = d.eye_width;
            v.recommended_image_rect_height = d.eye_height;
            v.max_image_rect_width = d.max_eye_width;
            v.max_image_rect_height = d.max_eye_height;
            v.recommended_swapchain_sample_count = 1;
            v.max_swapchain_sample_count = 4;
        }
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn enumerate_blend_modes(
    instance: xr::Instance,
    system: xr::SystemId,
    ty: xr::ViewConfigurationType,
    capacity: u32,
    count: *mut u32,
    out: *mut xr::EnvironmentBlendMode,
) -> xr::Result {
    crate::traced!("xrEnumerateEnvironmentBlendModes", [instance, system, ty, capacity, count, out], || {
        if let Some(e) = check_system(instance, system) {
            return e;
        }
        if ty != xr::ViewConfigurationType::PRIMARY_STEREO {
            return xr::Result::ERROR_VIEW_CONFIGURATION_TYPE_UNSUPPORTED;
        }
        fill(capacity, count, out, &[xr::EnvironmentBlendMode::OPAQUE])
    })
}

// ---------------------------------------------------------------- session

pub unsafe extern "system" fn create_session(
    instance: xr::Instance,
    info: *const xr::SessionCreateInfo,
    out: *mut xr::Session,
) -> xr::Result {
    crate::traced!("xrCreateSession", [instance, info, out], || {
        if let Some(e) = check_system(instance, (*info).system_id) {
            return e;
        }
        let shared = Arc::new(Shared::default());
        // XRMIMIC_HANDS=1: start with tracked hands instead of controllers.
        if std::env::var("XRMIMIC_HANDS").is_ok_and(|v| v == "1") {
            shared.hands.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        let gfx = match SessionGfx::from_session_create((*info).next, shared.clone()) {
            Ok(g) => g,
            Err(e) => return e,
        };
        let mut rt = rt();
        let app = rt.instances.get(&instance.into_raw()).map(|i| i.app_name.clone()).unwrap_or_default();
        *shared.app_name.lock().unwrap() = app;
        let device = crate::devstore::current();
        let sim = Arc::new(Mutex::new(SimState::new(&device)));
        let window = Window::spawn(&gfx, sim.clone(), shared);
        let hz = std::env::var("XRMIMIC_REFRESH").ok().and_then(|s| s.parse().ok()).unwrap_or(device.default_refresh);
        let now = time::now_ns();
        let h = rt.new_handle();
        rt.sessions.insert(
            h,
            SessionData {
                instance: instance.into_raw(),
                state: xr::SessionState::UNKNOWN,
                running: false,
                exit_requested: false,
                gfx,
                sim,
                window: Some(window),
                clock: FrameClock::new(hz, now),
                last_wake: now,
                frame_begun: false,
                frames_waited: 0,
                stats: Default::default(),
                frames_ended: 0,
                frames_shown: 0,
                attached_sets: Vec::new(),
                profile: 0,
                hands: false,
                snapshot: Default::default(),
                previous: Default::default(),
                sync_time: now,
                active_sets: Vec::new(),
                change_times: HashMap::new(),
                stage_bounds: crate::devstore::current().play_area,
            },
        );
        xlog!("session {h} created ({hz} Hz)");
        rt.set_state(h, xr::SessionState::IDLE);
        rt.set_state(h, xr::SessionState::READY);
        *out = xr::Session::from_raw(h);
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn destroy_session(session: xr::Session) -> xr::Result {
    crate::traced!("xrDestroySession", [session], || {
        let window = {
            let mut rt = rt();
            let Some(s) = rt.sessions.get_mut(&session.into_raw()) else {
                return xr::Result::ERROR_HANDLE_INVALID;
            };
            let w = s.window.take();
            rt.drop_session(session.into_raw());
            w
        };
        // Join the window thread outside the runtime lock.
        if let Some(w) = window {
            w.stop();
        }
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn begin_session(session: xr::Session, info: *const xr::SessionBeginInfo) -> xr::Result {
    crate::traced!("xrBeginSession", [session, info], || {
        let mut rt = rt();
        let h = session.into_raw();
        let Some(s) = rt.sessions.get_mut(&h) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        if s.running {
            return xr::Result::ERROR_SESSION_RUNNING;
        }
        if s.state != xr::SessionState::READY {
            return xr::Result::ERROR_SESSION_NOT_READY;
        }
        if (*info).primary_view_configuration_type != xr::ViewConfigurationType::PRIMARY_STEREO {
            return xr::Result::ERROR_VIEW_CONFIGURATION_TYPE_UNSUPPORTED;
        }
        s.running = true;
        rt.set_state(h, xr::SessionState::SYNCHRONIZED);
        rt.set_state(h, xr::SessionState::VISIBLE);
        rt.set_state(h, xr::SessionState::FOCUSED);
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn end_session(session: xr::Session) -> xr::Result {
    crate::traced!("xrEndSession", [session], || {
        let mut rt = rt();
        let h = session.into_raw();
        let Some(s) = rt.sessions.get_mut(&h) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        if !s.running {
            return xr::Result::ERROR_SESSION_NOT_RUNNING;
        }
        if s.state != xr::SessionState::STOPPING {
            return xr::Result::ERROR_SESSION_NOT_STOPPING;
        }
        s.running = false;
        let exit = s.exit_requested;
        rt.set_state(h, xr::SessionState::IDLE);
        if exit {
            rt.set_state(h, xr::SessionState::EXITING);
        } else {
            rt.set_state(h, xr::SessionState::READY);
        }
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn request_exit_session(session: xr::Session) -> xr::Result {
    crate::traced!("xrRequestExitSession", [session], || {
        let mut rt = rt();
        let h = session.into_raw();
        let Some(s) = rt.sessions.get_mut(&h) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        if !s.running {
            return xr::Result::ERROR_SESSION_NOT_RUNNING;
        }
        s.exit_requested = true;
        rt.set_state(h, xr::SessionState::STOPPING);
        xr::Result::SUCCESS
    })
}

// ---------------------------------------------------------------- spaces

fn session_has_local_floor(rt: &Runtime, session: u64) -> bool {
    rt.sessions
        .get(&session)
        .and_then(|s| rt.instances.get(&s.instance))
        .is_some_and(|i| i.extensions.iter().any(|e| e == "XR_EXT_local_floor"))
}

pub unsafe extern "system" fn enumerate_reference_spaces(
    session: xr::Session,
    capacity: u32,
    count: *mut u32,
    out: *mut xr::ReferenceSpaceType,
) -> xr::Result {
    crate::traced!("xrEnumerateReferenceSpaces", [session, capacity, count, out], || {
        let rt = rt();
        if !rt.sessions.contains_key(&session.into_raw()) {
            return xr::Result::ERROR_HANDLE_INVALID;
        }
        let mut v = vec![
            xr::ReferenceSpaceType::VIEW,
            xr::ReferenceSpaceType::LOCAL,
            xr::ReferenceSpaceType::STAGE,
        ];
        if session_has_local_floor(&rt, session.into_raw()) {
            v.push(xr::ReferenceSpaceType::LOCAL_FLOOR);
        }
        drop(rt);
        fill(capacity, count, out, &v)
    })
}

pub unsafe extern "system" fn create_reference_space(
    session: xr::Session,
    info: *const xr::ReferenceSpaceCreateInfo,
    out: *mut xr::Space,
) -> xr::Result {
    crate::traced!("xrCreateReferenceSpace", [session, info, out], || {
        let mut rt = rt();
        let h = session.into_raw();
        if !rt.sessions.contains_key(&h) {
            return xr::Result::ERROR_HANDLE_INVALID;
        }
        let ty = (*info).reference_space_type;
        let ok = matches!(
            ty,
            xr::ReferenceSpaceType::VIEW | xr::ReferenceSpaceType::LOCAL | xr::ReferenceSpaceType::STAGE
        ) || (ty == xr::ReferenceSpaceType::LOCAL_FLOOR && session_has_local_floor(&rt, h));
        xlog!("xrCreateReferenceSpace {ty:?}{}", if ok { "" } else { " (unsupported)" });
        if !ok {
            return xr::Result::ERROR_REFERENCE_SPACE_UNSUPPORTED;
        }
        let id = rt.new_handle();
        rt.spaces.insert(
            id,
            SpaceData {
                session: h,
                kind: SpaceKind::Reference(ty),
                offset: from_xr(&(*info).pose_in_reference_space),
            },
        );
        *out = xr::Space::from_raw(id);
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn reference_space_bounds(
    session: xr::Session,
    ty: xr::ReferenceSpaceType,
    bounds: *mut xr::Extent2Df,
) -> xr::Result {
    crate::traced!("xrGetReferenceSpaceBoundsRect", [session, ty, bounds], || {
        // The play area as the session was last told it: a change reaches it
        // with a reference space change event, polled with the others.
        let Some([w, d]) = rt().sessions.get(&session.into_raw()).map(|s| s.stage_bounds) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        if ty == xr::ReferenceSpaceType::STAGE && w > 0.0 && d > 0.0 {
            *bounds = xr::Extent2Df { width: w, height: d };
            xr::Result::SUCCESS
        } else {
            *bounds = xr::Extent2Df { width: 0.0, height: 0.0 };
            xr::Result::SPACE_BOUNDS_UNAVAILABLE
        }
    })
}

/// Pose of a space's origin in STAGE space, or None if it is not tracked now.
pub fn space_pose(rt: &Runtime, space: u64) -> Option<Pose> {
    let sp = rt.spaces.get(&space)?;
    let session = rt.sessions.get(&sp.session)?;
    let sim = session.sim.lock().unwrap();
    let base = match sp.kind {
        SpaceKind::Reference(xr::ReferenceSpaceType::VIEW) => sim.head_pose(),
        SpaceKind::Reference(xr::ReferenceSpaceType::LOCAL) => sim.local_space_pose(sim.eye_height_m),
        SpaceKind::Reference(_) => Pose::IDENTITY, // STAGE and LOCAL_FLOOR: the floor under the start point
        SpaceKind::Action { action, subaction } => {
            crate::input::action_pose(rt, session, &sim, action, subaction)?
        }
    };
    Some(base.compose(&sp.offset))
}

unsafe fn write_location(rt: &Runtime, space: u64, base: u64, loc: &mut xr::SpaceLocation) -> xr::Result {
    if !rt.spaces.contains_key(&space) || !rt.spaces.contains_key(&base) {
        return xr::Result::ERROR_HANDLE_INVALID;
    }
    match (space_pose(rt, space), space_pose(rt, base)) {
        (Some(s), Some(b)) => {
            loc.location_flags = tracked_flags();
            loc.pose = to_xr(&Pose::relative(&b, &s));
        }
        _ => {
            loc.location_flags = xr::SpaceLocationFlags::EMPTY;
            loc.pose = to_xr(&Pose::IDENTITY);
        }
    }
    // Velocities, if asked for, are reported as unknown.
    let v = find_in_chain(loc.next as *const _, xr::StructureType::SPACE_VELOCITY);
    if !v.is_null() {
        (*(v as *mut xr::SpaceVelocity)).velocity_flags = xr::SpaceVelocityFlags::EMPTY;
    }
    xr::Result::SUCCESS
}

pub unsafe extern "system" fn locate_space(
    space: xr::Space,
    base: xr::Space,
    _time: xr::Time,
    loc: *mut xr::SpaceLocation,
) -> xr::Result {
    crate::traced!("xrLocateSpace", [space, base, _time, loc], || write_location(&rt(), space.into_raw(), base.into_raw(), &mut *loc))
}

pub unsafe extern "system" fn locate_spaces(
    session: xr::Session,
    info: *const xr::SpacesLocateInfo,
    out: *mut xr::SpaceLocations,
) -> xr::Result {
    crate::traced!("xrLocateSpaces", [session, info, out], || {
        let rt = rt();
        if !rt.sessions.contains_key(&session.into_raw()) {
            return xr::Result::ERROR_HANDLE_INVALID;
        }
        let info = &*info;
        let out = &mut *out;
        if out.location_count != info.space_count {
            return xr::Result::ERROR_VALIDATION_FAILURE;
        }
        let base = info.base_space.into_raw();
        let base_pose = space_pose(&rt, base);
        for i in 0..info.space_count as usize {
            let s = (*info.spaces.add(i)).into_raw();
            let d = &mut *out.locations.add(i);
            match (space_pose(&rt, s), base_pose) {
                (Some(p), Some(b)) => {
                    d.location_flags = tracked_flags();
                    d.pose = to_xr(&Pose::relative(&b, &p));
                }
                _ => {
                    d.location_flags = xr::SpaceLocationFlags::EMPTY;
                    d.pose = to_xr(&Pose::IDENTITY);
                }
            }
        }
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn destroy_space(space: xr::Space) -> xr::Result {
    crate::traced!("xrDestroySpace", [space], || {
        if rt().spaces.remove(&space.into_raw()).is_some() {
            xr::Result::SUCCESS
        } else {
            xr::Result::ERROR_HANDLE_INVALID
        }
    })
}

// ---------------------------------------------------------------- swapchains

pub unsafe extern "system" fn enumerate_swapchain_formats(
    session: xr::Session,
    capacity: u32,
    count: *mut u32,
    out: *mut i64,
) -> xr::Result {
    crate::traced!("xrEnumerateSwapchainFormats", [session, capacity, count, out], || {
        if !rt().sessions.contains_key(&session.into_raw()) {
            return xr::Result::ERROR_HANDLE_INVALID;
        }
        fill(capacity, count, out, &gfx::swapchain_formats())
    })
}

pub unsafe extern "system" fn create_swapchain(
    session: xr::Session,
    info: *const xr::SwapchainCreateInfo,
    out: *mut xr::Swapchain,
) -> xr::Result {
    crate::traced!("xrCreateSwapchain", [session, info, out], || {
        let mut rt = rt();
        let h = session.into_raw();
        let Some(s) = rt.sessions.get(&h) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        match s.gfx.create_swapchain(h, &*info) {
            Ok(sc) => {
                let id = rt.new_handle();
                rt.swapchains.insert(id, sc);
                *out = xr::Swapchain::from_raw(id);
                xr::Result::SUCCESS
            }
            Err(e) => e,
        }
    })
}

pub unsafe extern "system" fn destroy_swapchain(swapchain: xr::Swapchain) -> xr::Result {
    crate::traced!("xrDestroySwapchain", [swapchain], || {
        if rt().swapchains.remove(&swapchain.into_raw()).is_some() {
            xr::Result::SUCCESS
        } else {
            xr::Result::ERROR_HANDLE_INVALID
        }
    })
}

pub unsafe extern "system" fn enumerate_swapchain_images(
    swapchain: xr::Swapchain,
    capacity: u32,
    count: *mut u32,
    out: *mut xr::SwapchainImageBaseHeader,
) -> xr::Result {
    crate::traced!("xrEnumerateSwapchainImages", [swapchain, capacity, count, out], || {
        let rt = rt();
        let Some(sc) = rt.swapchains.get(&swapchain.into_raw()) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        *count = sc.images.len() as u32;
        if capacity == 0 {
            return xr::Result::SUCCESS;
        }
        if (capacity as usize) < sc.images.len() {
            return xr::Result::ERROR_SIZE_INSUFFICIENT;
        }
        gfx::write_images(sc, out)
    })
}

pub unsafe extern "system" fn acquire_image(
    swapchain: xr::Swapchain,
    _info: *const xr::SwapchainImageAcquireInfo,
    index: *mut u32,
) -> xr::Result {
    crate::traced!("xrAcquireSwapchainImage", [swapchain, _info, index], || {
        let mut rt = rt();
        let Some(sc) = rt.swapchains.get_mut(&swapchain.into_raw()) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        if sc.acquired.len() >= sc.images.len() {
            return xr::Result::ERROR_CALL_ORDER_INVALID;
        }
        let i = sc.next_index;
        sc.next_index = (i + 1) % sc.images.len() as u32;
        sc.acquired.push_back(i);
        *index = i;
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn wait_image(
    swapchain: xr::Swapchain,
    _info: *const xr::SwapchainImageWaitInfo,
) -> xr::Result {
    crate::traced!("xrWaitSwapchainImage", [swapchain, _info], || {
        let rt = rt();
        let Some(sc) = rt.swapchains.get(&swapchain.into_raw()) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        if sc.acquired.is_empty() {
            return xr::Result::ERROR_CALL_ORDER_INVALID;
        }
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn release_image(
    swapchain: xr::Swapchain,
    _info: *const xr::SwapchainImageReleaseInfo,
) -> xr::Result {
    crate::traced!("xrReleaseSwapchainImage", [swapchain, _info], || {
        let mut rt = rt();
        let Some(sc) = rt.swapchains.get_mut(&swapchain.into_raw()) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        let Some(i) = sc.acquired.pop_front() else {
            return xr::Result::ERROR_CALL_ORDER_INVALID;
        };
        sc.last_released = Some(i);
        xr::Result::SUCCESS
    })
}

// ---------------------------------------------------------------- frames

pub unsafe extern "system" fn wait_frame(
    session: xr::Session,
    _info: *const xr::FrameWaitInfo,
    state: *mut xr::FrameState,
) -> xr::Result {
    crate::traced!("xrWaitFrame", [session, _info, state], || {
        let (wake, period, render) = {
            let mut rt = rt();
            let Some(s) = rt.sessions.get_mut(&session.into_raw()) else {
                return xr::Result::ERROR_HANDLE_INVALID;
            };
            if !s.running {
                return xr::Result::ERROR_SESSION_NOT_RUNNING;
            }
            let now = time::now_ns();
            let wake = if std::env::var_os("XRMIMIC_VSYNC_GRID").is_some() {
                s.clock.wake_after(s.last_wake, now)
            } else {
                s.clock.wake_free(s.last_wake, now)
            };
            let st = &mut s.stats;
            if st.ended > 0 {
                st.to_next_wait += now - st.ended;
                st.skipped += ((wake - s.last_wake - s.clock.period_ns / 2) / s.clock.period_ns).max(0);
            }
            st.wait_called = now;
            if std::env::var_os("XRMIMIC_TRACE_FRAMES").is_some() {
                xlog!("trace wait-call (wake in {:.2} ms)", (wake - now) as f64 / 1e6);
            }
            s.last_wake = wake;
            s.frames_waited += 1;
            let render = matches!(s.state, xr::SessionState::VISIBLE | xr::SessionState::FOCUSED);
            (wake, s.clock.period_ns, render)
        };
        time::sleep_until(wake);
        if let Some(s) = rt().sessions.get_mut(&session.into_raw()) {
            let now = time::now_ns();
            s.stats.blocked += now - s.stats.wait_called;
            if std::env::var_os("XRMIMIC_TRACE_FRAMES").is_some() {
                xlog!("trace wait-return");
            }
            s.stats.wait_returned = now;
        }
        let st = &mut *state;
        st.predicted_display_time = xr::Time::from_nanos(wake + period);
        st.predicted_display_period = xr::Duration::from_nanos(period);
        st.should_render = render.into();
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn begin_frame(session: xr::Session, _info: *const xr::FrameBeginInfo) -> xr::Result {
    crate::traced!("xrBeginFrame", [session, _info], || {
        let mut rt = rt();
        let Some(s) = rt.sessions.get_mut(&session.into_raw()) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        if !s.running {
            return xr::Result::ERROR_SESSION_NOT_RUNNING;
        }
        if s.frame_begun {
            // The previous frame was never ended: it is discarded.
            return xr::Result::FRAME_DISCARDED;
        }
        s.frame_begun = true;
        let now = time::now_ns();
        s.stats.to_begin += now - s.stats.wait_returned;
        if std::env::var_os("XRMIMIC_TRACE_FRAMES").is_some() {
            xlog!("trace begin");
        }
        s.stats.begun = now;
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn end_frame(session: xr::Session, info: *const xr::FrameEndInfo) -> xr::Result {
    crate::traced!("xrEndFrame", [session, info], || {
        let mut rt = rt();
        let h = session.into_raw();
        let Some(s) = rt.sessions.get(&h) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        if !s.running {
            return xr::Result::ERROR_SESSION_NOT_RUNNING;
        }
        if !s.frame_begun {
            return xr::Result::ERROR_CALL_ORDER_INVALID;
        }
        let info = &*info;
        let mut eyes = Vec::new();
        // The projection layer's eye poses (STAGE space) and fields of view, and
        // the quad layers: what the headset window needs to draw quads.
        let mut views: Vec<(Pose, Fov)> = Vec::new();
        let mut quads: Vec<(gfx::EyeSource, crate::shared::QuadMeta)> = Vec::new();
        // Depth buffers chained to the projection views (XR_KHR_composition_layer_depth).
        let mut depths: Vec<(usize, gfx::EyeSource, crate::shared::DepthInfo)> = Vec::new();
        let mut layer_types = Vec::new();
        for i in 0..info.layer_count as usize {
            let l = *info.layers.add(i);
            if l.is_null() {
                return xr::Result::ERROR_LAYER_INVALID;
            }
            layer_types.push((*l).ty);
            let flat = match (*l).ty {
                xr::StructureType::COMPOSITION_LAYER_QUAD => {
                    let q = &*(l as *const xr::CompositionLayerQuad);
                    Some((
                        q.layer_flags,
                        q.space,
                        q.eye_visibility,
                        q.sub_image,
                        q.pose,
                        crate::shared::Shape::Quad { width: q.size.width, height: q.size.height },
                    ))
                }
                xr::StructureType::COMPOSITION_LAYER_CYLINDER_KHR => {
                    let c = &*(l as *const xr::CompositionLayerCylinderKHR);
                    let ok = c.radius.is_finite() && c.radius > 0.0 && c.central_angle > 0.0 && c.aspect_ratio > 0.0;
                    ok.then(|| {
                        (
                            c.layer_flags,
                            c.space,
                            c.eye_visibility,
                            c.sub_image,
                            c.pose,
                            crate::shared::Shape::Cylinder {
                                radius: c.radius,
                                central_angle: c.central_angle.min(std::f32::consts::TAU),
                                height: xrmimic_core::math::cylinder_height(c.radius, c.central_angle, c.aspect_ratio),
                            },
                        )
                    })
                }
                xr::StructureType::COMPOSITION_LAYER_EQUIRECT2_KHR => {
                    let e = &*(l as *const xr::CompositionLayerEquirect2KHR);
                    use std::f32::consts::{FRAC_PI_2, TAU};
                    let upper = e.upper_vertical_angle.clamp(-FRAC_PI_2, FRAC_PI_2);
                    let lower = e.lower_vertical_angle.clamp(-FRAC_PI_2, FRAC_PI_2);
                    let ok = !e.radius.is_nan() && e.radius >= 0.0 && e.central_horizontal_angle > 0.0 && upper > lower;
                    // 0 or +inf: a sphere at infinity, drawn far away and
                    // centred on the eye so it shows no parallax.
                    let infinite = e.radius == 0.0 || e.radius.is_infinite();
                    ok.then(|| {
                        (
                            e.layer_flags,
                            e.space,
                            e.eye_visibility,
                            e.sub_image,
                            e.pose,
                            crate::shared::Shape::Equirect {
                                radius: if infinite { xrmimic_core::math::FAR_SPHERE_RADIUS } else { e.radius },
                                h_angle: e.central_horizontal_angle.min(TAU),
                                upper,
                                lower,
                                infinite,
                            },
                        )
                    })
                }
                xr::StructureType::COMPOSITION_LAYER_CUBE_KHR => {
                    let c = &*(l as *const xr::CompositionLayerCubeKHR);
                    match rt.swapchains.get(&c.swapchain.into_raw()) {
                        // Only a 6-face swapchain holds a cube map.
                        Some(sc) if sc.faces == 6 => Some((
                            c.layer_flags,
                            c.space,
                            c.eye_visibility,
                            xr::SwapchainSubImage {
                                swapchain: c.swapchain,
                                image_rect: xr::Rect2Di {
                                    offset: xr::Offset2Di { x: 0, y: 0 },
                                    extent: xr::Extent2Di { width: sc.width as i32, height: sc.height as i32 },
                                },
                                // The first face of the cube at that array index.
                                image_array_index: c.image_array_index * 6,
                            },
                            xr::Posef { orientation: c.orientation, position: xr::Vector3f { x: 0.0, y: 0.0, z: 0.0 } },
                            crate::shared::Shape::Cube,
                        )),
                        _ => None,
                    }
                }
                _ => None,
            };
            if let Some((flags, space, eye_visibility, sub_image, pose, shape)) = flat {
                if quads.len() >= 8 {
                    continue;
                }
                let Some(sc) = rt.swapchains.get(&sub_image.swapchain.into_raw()) else {
                    return xr::Result::ERROR_HANDLE_INVALID;
                };
                let Some(idx) = sc.last_released else {
                    return xr::Result::ERROR_LAYER_INVALID;
                };
                let Some(base) = space_pose(&rt, space.into_raw()) else {
                    continue; // its space is not located right now: not shown
                };
                use crate::shared::Blend;
                let dt = find_in_chain((*l).next, xr::StructureType::COMPOSITION_LAYER_DEPTH_TEST_FB)
                    as *const xr::CompositionLayerDepthTestFB;
                let depth_test = (!dt.is_null()).then(|| (*dt).compare_op.into_raw());
                let f = flags;
                let blend = if !f.contains(xr::CompositionLayerFlags::BLEND_TEXTURE_SOURCE_ALPHA) {
                    Blend::Opaque
                } else if f.contains(xr::CompositionLayerFlags::UNPREMULTIPLIED_ALPHA) {
                    Blend::Straight
                } else {
                    Blend::Premultiplied
                };
                quads.push((
                    gfx::eye_source(sc, idx, &sub_image),
                    crate::shared::QuadMeta {
                        pose: base.compose(&from_xr(&pose)),
                        shape,
                        left: eye_visibility != xr::EyeVisibility::RIGHT,
                        right: eye_visibility != xr::EyeVisibility::LEFT,
                        blend,
                        depth_test,
                    },
                ));
                continue;
            }
            if (*l).ty != xr::StructureType::COMPOSITION_LAYER_PROJECTION || !eyes.is_empty() {
                continue;
            }
            let p = &*(l as *const xr::CompositionLayerProjection);
            if p.view_count != 2 {
                return xr::Result::ERROR_VALIDATION_FAILURE;
            }
            for v in 0..2 {
                let view = &*p.views.add(v);
                let sub = &view.sub_image;
                let Some(sc) = rt.swapchains.get(&sub.swapchain.into_raw()) else {
                    return xr::Result::ERROR_HANDLE_INVALID;
                };
                let Some(idx) = sc.last_released else {
                    return xr::Result::ERROR_LAYER_INVALID;
                };
                eyes.push(gfx::eye_source(sc, idx, sub));
                let di = find_in_chain(view.next, xr::StructureType::COMPOSITION_LAYER_DEPTH_INFO_KHR)
                    as *const xr::CompositionLayerDepthInfoKHR;
                if !di.is_null() {
                    let d = &*di;
                    let unit = 0.0..=1.0;
                    let ok = unit.contains(&d.min_depth)
                        && unit.contains(&d.max_depth)
                        && d.min_depth < d.max_depth
                        && d.near_z >= 0.0
                        && d.far_z >= 0.0
                        && d.near_z != d.far_z;
                    if !ok {
                        return xr::Result::ERROR_VALIDATION_FAILURE;
                    }
                    let Some(dsc) = rt.swapchains.get(&d.sub_image.swapchain.into_raw()) else {
                        return xr::Result::ERROR_HANDLE_INVALID;
                    };
                    if let Some(didx) = dsc.last_released {
                        let info =
                            crate::shared::DepthInfo { min: d.min_depth, max: d.max_depth, near: d.near_z, far: d.far_z };
                        depths.push((v, gfx::eye_source(dsc, didx, &d.sub_image), info));
                    }
                }
                if let Some(base) = space_pose(&rt, p.space.into_raw()) {
                    let f = view.fov;
                    views.push((
                        base.compose(&from_xr(&view.pose)),
                        Fov { left: f.angle_left, right: f.angle_right, up: f.angle_up, down: f.angle_down },
                    ));
                }
            }
        }
        let s = rt.sessions.get_mut(&h).unwrap();
        s.frame_begun = false;
        s.frames_ended += 1;
        {
            let now = time::now_ns();
            let st = &mut s.stats;
            st.to_end += now - st.begun;
            if std::env::var_os("XRMIMIC_TRACE_FRAMES").is_some() {
                xlog!("frame {}: begin->end {:.2} ms, cycle {:.2} ms", s.frames_ended, (now - st.begun) as f64 / 1e6, (now - st.ended) as f64 / 1e6);
            }
            st.ended = now;
            st.n += 1;
            if st.n == 90 {
                let ms = |v: i64| v as f64 / st.n as f64 / 1e6;
                xlog!(
                    "frame timing over {} frames: blocked in wait {:.2} ms, wait->begin {:.2} ms, begin->end {:.2} ms, end->next wait {:.2} ms, missed vsyncs {}",
                    st.n, ms(st.blocked), ms(st.to_begin), ms(st.to_end), ms(st.to_next_wait), st.skipped
                );
                *st = FrameStats { ended: st.ended, ..Default::default() };
            }
        }
        let n = s.frames_ended;
        if crate::log::hit("endframe-detail") {
            xlog!(
                "xrEndFrame #{n}: {} layers {layer_types:?}, eye rects {:?}",
                info.layer_count,
                eyes.iter().map(|e| (e.rect.offset.x, e.rect.offset.y, e.rect.extent.width, e.rect.extent.height)).collect::<Vec<_>>()
            );
        }
        if matches!(n, 1 | 10 | 100) || n % 1000 == 0 {
            xlog!("frames submitted: {n}");
        }
        if !eyes.is_empty() {
            s.frames_shown += 1;
            if s.frames_shown == 1 {
                xlog!("first frame with a projection layer: #{n}");
            }
        }
        s.gfx.submit(&eyes, &views, &quads, &depths, s.frames_shown);
        // Tests only: behave as if the user closed the headset window after N frames.
        if std::env::var("XRMIMIC_TEST_CLOSE_AFTER").ok().and_then(|v| v.parse::<u64>().ok()) == Some(n) {
            s.gfx.shared.close_requested.store(true, std::sync::atomic::Ordering::SeqCst);
        }
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn locate_views(
    session: xr::Session,
    info: *const xr::ViewLocateInfo,
    state: *mut xr::ViewState,
    capacity: u32,
    count: *mut u32,
    views: *mut xr::View,
) -> xr::Result {
    crate::traced!("xrLocateViews", [session, info, state, capacity, count, views], || {
        let rt = rt();
        let h = session.into_raw();
        let Some(s) = rt.sessions.get(&h) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        let info = &*info;
        if info.view_configuration_type != xr::ViewConfigurationType::PRIMARY_STEREO {
            return xr::Result::ERROR_VIEW_CONFIGURATION_TYPE_UNSUPPORTED;
        }
        *count = 2;
        if capacity == 0 {
            return xr::Result::SUCCESS;
        }
        if capacity < 2 {
            return xr::Result::ERROR_SIZE_INSUFFICIENT;
        }
        let Some(base) = space_pose(&rt, info.space.into_raw()) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        // IPD and field of view follow the Device tab at once.
        let device = crate::devstore::current();
        let mut sim = s.sim.lock().unwrap();
        sim.ipd = device.ipd;
        for eye in 0..2 {
            let v = &mut *views.add(eye);
            v.pose = to_xr(&Pose::relative(&base, &sim.eye_pose(eye)));
            v.fov = fov_xr(device.fov(eye));
        }
        (*state).view_state_flags = xr::ViewStateFlags::ORIENTATION_VALID
            | xr::ViewStateFlags::POSITION_VALID
            | xr::ViewStateFlags::ORIENTATION_TRACKED
            | xr::ViewStateFlags::POSITION_TRACKED;
        xr::Result::SUCCESS
    })
}
