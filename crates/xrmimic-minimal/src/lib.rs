//! The smallest OpenXR runtime: the loader can negotiate with it, an app can
//! create an instance and find one head-mounted system. Nothing renders.

use openxr_sys as xr;
use xr::Handle as _;
use std::ffi::{c_char, CStr};

/// The only symbol the loader looks up by name.
#[no_mangle]
pub unsafe extern "system" fn xrNegotiateLoaderRuntimeInterface(
    info: *const xr::NegotiateLoaderInfo,
    request: *mut xr::NegotiateRuntimeRequest,
) -> xr::Result {
    let (info, req) = (&*info, &mut *request);
    if info.struct_type != xr::NegotiateLoaderInfo::TYPE
        || req.struct_type != xr::NegotiateRuntimeRequest::TYPE
        || info.min_interface_version > 1
        || info.max_interface_version < 1
    {
        return xr::Result::ERROR_INITIALIZATION_FAILED;
    }
    req.runtime_interface_version = 1;
    req.runtime_api_version = xr::Version::new(1, 0, 34);
    req.get_instance_proc_addr = Some(get_instance_proc_addr);
    xr::Result::SUCCESS
}

/// Every other entry point is handed out by name from here.
unsafe extern "system" fn get_instance_proc_addr(
    _instance: xr::Instance,
    name: *const c_char,
    out: *mut Option<xr::pfn::VoidFunction>,
) -> xr::Result {
    use std::mem::transmute as t;
    let name = CStr::from_ptr(name);
    log(&format!("xrGetInstanceProcAddr({})", name.to_string_lossy()));
    let f: xr::pfn::VoidFunction = match name.to_bytes() {
        b"xrGetInstanceProcAddr" => t(get_instance_proc_addr as xr::pfn::GetInstanceProcAddr),
        b"xrEnumerateInstanceExtensionProperties" => {
            t(enumerate_extensions as xr::pfn::EnumerateInstanceExtensionProperties)
        }
        b"xrCreateInstance" => t(create_instance as xr::pfn::CreateInstance),
        b"xrDestroyInstance" => t(destroy_instance as xr::pfn::DestroyInstance),
        b"xrGetInstanceProperties" => t(instance_properties as xr::pfn::GetInstanceProperties),
        b"xrGetSystem" => t(get_system as xr::pfn::GetSystem),
        b"xrGetSystemProperties" => t(system_properties as xr::pfn::GetSystemProperties),
        b"xrPollEvent" => t(poll_event as xr::pfn::PollEvent),
        b"xrResultToString" => t(result_to_string as xr::pfn::ResultToString),
        b"xrStructureTypeToString" => t(structure_type_to_string as xr::pfn::StructureTypeToString),
        b"xrEnumerateViewConfigurations" => {
            t(enumerate_view_configurations as xr::pfn::EnumerateViewConfigurations)
        }
        b"xrGetViewConfigurationProperties" => {
            t(view_configuration_properties as xr::pfn::GetViewConfigurationProperties)
        }
        b"xrEnumerateViewConfigurationViews" => {
            t(enumerate_views as xr::pfn::EnumerateViewConfigurationViews)
        }
        b"xrEnumerateEnvironmentBlendModes" => {
            t(enumerate_blend_modes as xr::pfn::EnumerateEnvironmentBlendModes)
        }
        other => match UNSUPPORTED.iter().position(|n| n.as_bytes() == other) {
            // The loader calls whatever pointer it got for a core function
            // without checking for null, so core functions must not be missing:
            // they get a stub that logs and says "unsupported".
            Some(i) => STUBS[i],
            None => {
                *out = None;
                return xr::Result::ERROR_FUNCTION_UNSUPPORTED;
            }
        },
    };
    *out = Some(f);
    xr::Result::SUCCESS
}

unsafe extern "system" fn enumerate_extensions(
    _layer: *const c_char,
    _capacity: u32,
    count: *mut u32,
    _props: *mut xr::ExtensionProperties,
) -> xr::Result {
    *count = 0; // no extensions: not even a graphics binding
    xr::Result::SUCCESS
}

unsafe extern "system" fn create_instance(
    info: *const xr::InstanceCreateInfo,
    instance: *mut xr::Instance,
) -> xr::Result {
    if (*info).enabled_extension_count > 0 {
        return xr::Result::ERROR_EXTENSION_NOT_PRESENT;
    }
    *instance = xr::Instance::from_raw(1);
    xr::Result::SUCCESS
}

unsafe extern "system" fn destroy_instance(_instance: xr::Instance) -> xr::Result {
    xr::Result::SUCCESS
}

unsafe extern "system" fn instance_properties(
    _instance: xr::Instance,
    props: *mut xr::InstanceProperties,
) -> xr::Result {
    (*props).runtime_version = xr::Version::new(0, 1, 0);
    write_str(&mut (*props).runtime_name, "XRMimic minimal");
    xr::Result::SUCCESS
}

unsafe extern "system" fn get_system(
    _instance: xr::Instance,
    info: *const xr::SystemGetInfo,
    system: *mut xr::SystemId,
) -> xr::Result {
    if (*info).form_factor != xr::FormFactor::HEAD_MOUNTED_DISPLAY {
        return xr::Result::ERROR_FORM_FACTOR_UNSUPPORTED;
    }
    *system = xr::SystemId::from_raw(1);
    xr::Result::SUCCESS
}

unsafe extern "system" fn system_properties(
    _instance: xr::Instance,
    _system: xr::SystemId,
    props: *mut xr::SystemProperties,
) -> xr::Result {
    let p = &mut *props;
    p.system_id = xr::SystemId::from_raw(1);
    p.vendor_id = 0;
    write_str(&mut p.system_name, "XRMimic minimal headset");
    p.graphics_properties = xr::SystemGraphicsProperties {
        max_swapchain_image_width: 0,
        max_swapchain_image_height: 0,
        max_layer_count: 0,
    };
    p.tracking_properties = xr::SystemTrackingProperties {
        orientation_tracking: xr::TRUE,
        position_tracking: xr::TRUE,
    };
    xr::Result::SUCCESS
}

unsafe extern "system" fn poll_event(
    _instance: xr::Instance,
    _event: *mut xr::EventDataBuffer,
) -> xr::Result {
    xr::Result::EVENT_UNAVAILABLE
}

unsafe extern "system" fn result_to_string(
    _instance: xr::Instance,
    value: xr::Result,
    buffer: *mut c_char,
) -> xr::Result {
    let s = format!("{value:?}");
    let s = if s.starts_with(|c: char| c.is_ascii_digit() || c == '-') {
        format!("XR_UNKNOWN_RESULT_{s}")
    } else {
        format!("XR_{s}")
    };
    write_str(&mut *(buffer as *mut [c_char; xr::MAX_RESULT_STRING_SIZE]), &s);
    xr::Result::SUCCESS
}

unsafe extern "system" fn structure_type_to_string(
    _instance: xr::Instance,
    value: xr::StructureType,
    buffer: *mut c_char,
) -> xr::Result {
    let s = format!("XR_TYPE_{value:?}");
    write_str(&mut *(buffer as *mut [c_char; xr::MAX_STRUCTURE_NAME_SIZE]), &s);
    xr::Result::SUCCESS
}

/// The OpenXR two-call idiom: report the count, fill the array if it is big enough.
unsafe fn fill<T: Copy>(capacity: u32, count: *mut u32, out: *mut T, items: &[T]) -> xr::Result {
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

unsafe extern "system" fn enumerate_view_configurations(
    _instance: xr::Instance,
    _system: xr::SystemId,
    capacity: u32,
    count: *mut u32,
    out: *mut xr::ViewConfigurationType,
) -> xr::Result {
    fill(capacity, count, out, &[xr::ViewConfigurationType::PRIMARY_STEREO])
}

unsafe extern "system" fn view_configuration_properties(
    _instance: xr::Instance,
    _system: xr::SystemId,
    ty: xr::ViewConfigurationType,
    props: *mut xr::ViewConfigurationProperties,
) -> xr::Result {
    if ty != xr::ViewConfigurationType::PRIMARY_STEREO {
        return xr::Result::ERROR_VIEW_CONFIGURATION_TYPE_UNSUPPORTED;
    }
    (*props).view_configuration_type = ty;
    (*props).fov_mutable = xr::FALSE;
    xr::Result::SUCCESS
}

unsafe extern "system" fn enumerate_views(
    _instance: xr::Instance,
    _system: xr::SystemId,
    ty: xr::ViewConfigurationType,
    capacity: u32,
    count: *mut u32,
    out: *mut xr::ViewConfigurationView,
) -> xr::Result {
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
    // Write field by field: the app owns `ty` and `next`.
    for i in 0..2 {
        let v = &mut *out.add(i);
        v.recommended_image_rect_width = 1832;
        v.recommended_image_rect_height = 1920;
        v.max_image_rect_width = 4096;
        v.max_image_rect_height = 4096;
        v.recommended_swapchain_sample_count = 1;
        v.max_swapchain_sample_count = 1;
    }
    xr::Result::SUCCESS
}

unsafe extern "system" fn enumerate_blend_modes(
    _instance: xr::Instance,
    _system: xr::SystemId,
    ty: xr::ViewConfigurationType,
    capacity: u32,
    count: *mut u32,
    out: *mut xr::EnvironmentBlendMode,
) -> xr::Result {
    if ty != xr::ViewConfigurationType::PRIMARY_STEREO {
        return xr::Result::ERROR_VIEW_CONFIGURATION_TYPE_UNSUPPORTED;
    }
    fill(capacity, count, out, &[xr::EnvironmentBlendMode::OPAQUE])
}

/// Core functions this runtime does not implement (OpenXR 1.0 plus xrLocateSpaces from 1.1).
const UNSUPPORTED: [&str; 41] = [
    "xrCreateSession",
    "xrDestroySession",
    "xrBeginSession",
    "xrEndSession",
    "xrRequestExitSession",
    "xrEnumerateReferenceSpaces",
    "xrCreateReferenceSpace",
    "xrGetReferenceSpaceBoundsRect",
    "xrCreateActionSpace",
    "xrLocateSpace",
    "xrLocateSpaces",
    "xrDestroySpace",
    "xrEnumerateSwapchainFormats",
    "xrCreateSwapchain",
    "xrDestroySwapchain",
    "xrEnumerateSwapchainImages",
    "xrAcquireSwapchainImage",
    "xrWaitSwapchainImage",
    "xrReleaseSwapchainImage",
    "xrWaitFrame",
    "xrBeginFrame",
    "xrEndFrame",
    "xrLocateViews",
    "xrStringToPath",
    "xrPathToString",
    "xrCreateActionSet",
    "xrDestroyActionSet",
    "xrCreateAction",
    "xrDestroyAction",
    "xrSuggestInteractionProfileBindings",
    "xrAttachSessionActionSets",
    "xrGetCurrentInteractionProfile",
    "xrGetActionStateBoolean",
    "xrGetActionStateFloat",
    "xrGetActionStateVector2f",
    "xrGetActionStatePose",
    "xrSyncActions",
    "xrEnumerateBoundSourcesForAction",
    "xrGetInputSourceLocalizedName",
    "xrApplyHapticFeedback",
    "xrStopHapticFeedback",
];

/// Every OpenXR function returns XrResult and, on x64, the caller cleans up its
/// arguments, so one argument-less stub per name can stand in for any signature.
unsafe extern "system" fn stub<const I: usize>() -> xr::Result {
    log(&format!("{} -> ERROR_FUNCTION_UNSUPPORTED", UNSUPPORTED[I]));
    xr::Result::ERROR_FUNCTION_UNSUPPORTED
}

macro_rules! stubs {
    ($($i:literal)*) => {
        [$(unsafe { std::mem::transmute::<unsafe extern "system" fn() -> xr::Result, xr::pfn::VoidFunction>(stub::<$i>) }),*]
    };
}

static STUBS: [xr::pfn::VoidFunction; 41] = stubs![
    0 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20
    21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 39 40
];

/// Append a line to `XRMIMIC_LOG`, or `%TEMP%\xrmimic-minimal.log`.
fn log(line: &str) {
    use std::io::Write;
    let path = std::env::var_os("XRMIMIC_LOG")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::env::temp_dir().join("xrmimic-minimal.log"));
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(path) {
        let _ = writeln!(f, "[pid {}] {line}", std::process::id());
    }
}

fn write_str<const N: usize>(dst: &mut [c_char; N], s: &str) {
    let n = s.len().min(N - 1);
    for (d, b) in dst.iter_mut().zip(&s.as_bytes()[..n]) {
        *d = *b as c_char;
    }
    dst[n] = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Do what the loader and an app do: negotiate, look up, create, query.
    #[test]
    fn loader_and_app_round_trip() {
        unsafe {
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
            assert_eq!(xrNegotiateLoaderRuntimeInterface(&info, &mut req), xr::Result::SUCCESS);
            let gipa = req.get_instance_proc_addr.unwrap();

            let mut f = None;
            let r = gipa(xr::Instance::NULL, c"xrGetSystem".as_ptr(), &mut f);
            assert_eq!(r, xr::Result::SUCCESS);
            let get_system: xr::pfn::GetSystem = std::mem::transmute(f.unwrap());

            // Core functions are never missing (the loader would call null);
            // unimplemented ones answer "unsupported" when called.
            for name in UNSUPPORTED {
                let c = std::ffi::CString::new(name).unwrap();
                let mut f = None;
                assert_eq!(gipa(xr::Instance::NULL, c.as_ptr(), &mut f), xr::Result::SUCCESS, "{name}");
                let g: unsafe extern "system" fn() -> xr::Result = std::mem::transmute(f.unwrap());
                assert_eq!(g(), xr::Result::ERROR_FUNCTION_UNSUPPORTED, "{name}");
            }
            let r = gipa(xr::Instance::NULL, c"xrCreateHandTrackerEXT".as_ptr(), &mut f);
            assert_eq!(r, xr::Result::ERROR_FUNCTION_UNSUPPORTED, "extensions stay unlisted");

            let mut count = 0;
            let mut views = [std::mem::zeroed::<xr::ViewConfigurationView>(); 2];
            let stereo = xr::ViewConfigurationType::PRIMARY_STEREO;
            let sys = xr::SystemId::from_raw(1);
            let inst = xr::Instance::from_raw(1);
            assert_eq!(enumerate_views(inst, sys, stereo, 0, &mut count, views.as_mut_ptr()), xr::Result::SUCCESS);
            assert_eq!(count, 2);
            assert_eq!(enumerate_views(inst, sys, stereo, 2, &mut count, views.as_mut_ptr()), xr::Result::SUCCESS);
            assert_eq!(views[1].recommended_image_rect_width, 1832);

            let mut buf = [0 as c_char; xr::MAX_RESULT_STRING_SIZE];
            result_to_string(inst, xr::Result::ERROR_SIZE_INSUFFICIENT, buf.as_mut_ptr());
            assert_eq!(CStr::from_ptr(buf.as_ptr()).to_str().unwrap(), "XR_ERROR_SIZE_INSUFFICIENT");

            let get_info = xr::SystemGetInfo {
                ty: xr::SystemGetInfo::TYPE,
                next: std::ptr::null(),
                form_factor: xr::FormFactor::HEAD_MOUNTED_DISPLAY,
            };
            let mut sys = xr::SystemId::NULL;
            let r = get_system(xr::Instance::from_raw(1), &get_info, &mut sys);
            assert_eq!(r, xr::Result::SUCCESS);
            assert_eq!(sys.into_raw(), 1);
        }
    }
}
