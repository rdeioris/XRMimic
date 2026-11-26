//! xrmimic: an OpenXR runtime that simulates a headset in a desktop window.
//!
//! The loader calls `xrNegotiateLoaderRuntimeInterface`; every other entry
//! point is handed out by name from `get_instance_proc_addr`.

#![allow(clippy::missing_safety_doc)]

pub mod log;
pub mod runtimes;
pub mod time;
pub mod trace;

mod api;
mod control;
mod headset;
mod devstore;
mod input;
mod shared;
mod simthread;
mod state;

// The headset in xrmimic-server instead of the app (Windows): the pipe and
// its messages, the app's side, the server's side.
#[cfg(windows)]
mod pipe;
#[cfg(windows)]
mod remote;
#[cfg(windows)]
pub mod server;

// The graphics backend: session binding, swapchain images, frame copies.
#[cfg(windows)]
mod d3d;
#[cfg(windows)]
use d3d as gfx;
// Vulkan: the whole backend on Linux; on Windows the app's side of it, its
// frames taken on to the D3D11 window by vkd3d.
mod vk;
#[cfg(windows)]
mod vkd3d;
#[cfg(target_os = "linux")]
use vk as gfx;

// The "headset": a desktop window on Windows, a headless thread on Linux.
#[cfg(windows)]
mod devicetab;
#[cfg(windows)]
mod inputtab;
mod handtrack;
#[cfg(windows)]
mod logview;
#[cfg(windows)]
mod window;
#[cfg(windows)]
mod depthwatch;
#[cfg(windows)]
use window as ui;
#[cfg(target_os = "linux")]
mod headless;
#[cfg(target_os = "linux")]
use headless as ui;

pub use exports::*;

mod exports {
    use crate::{api, input, xlog};
    use openxr_sys as xr;
    use std::ffi::{c_char, CStr};

    #[no_mangle]
    pub unsafe extern "system" fn xrNegotiateLoaderRuntimeInterface(
        info: *const xr::NegotiateLoaderInfo,
        request: *mut xr::NegotiateRuntimeRequest,
    ) -> xr::Result {
        if info.is_null() || request.is_null() {
            return xr::Result::ERROR_INITIALIZATION_FAILED;
        }
        let (info, req) = (&*info, &mut *request);
        xlog!(
            "negotiate: interface {}..{}, api {}..{}",
            info.min_interface_version,
            info.max_interface_version,
            info.min_api_version,
            info.max_api_version
        );
        if info.struct_type != xr::NegotiateLoaderInfo::TYPE
            || req.struct_type != xr::NegotiateRuntimeRequest::TYPE
            || info.min_interface_version > 1
            || info.max_interface_version < 1
        {
            return xr::Result::ERROR_INITIALIZATION_FAILED;
        }
        req.runtime_interface_version = 1;
        let want = xr::Version::new(1, 1, 0);
        req.runtime_api_version = if info.max_api_version.into_raw() >= want.into_raw() {
            want
        } else {
            xr::Version::new(1, 0, 34)
        };
        req.get_instance_proc_addr = Some(get_instance_proc_addr);
        xr::Result::SUCCESS
    }

    pub unsafe extern "system" fn get_instance_proc_addr(
        _instance: xr::Instance,
        name: *const c_char,
        out: *mut Option<xr::pfn::VoidFunction>,
    ) -> xr::Result {
        if name.is_null() || out.is_null() {
            return xr::Result::ERROR_VALIDATION_FAILURE;
        }
        let n = CStr::from_ptr(name).to_string_lossy();
        let f = lookup(&n);
        if crate::log::hit("xrGetInstanceProcAddr") || f.is_none() {
            xlog!("xrGetInstanceProcAddr({n}) -> {}", if f.is_some() { "ok" } else { "UNSUPPORTED" });
        }
        let instance = _instance;
        crate::traced!("xrGetInstanceProcAddr", [instance, name, out], || {
            *out = f;
            if f.is_some() {
                xr::Result::SUCCESS
            } else {
                xr::Result::ERROR_FUNCTION_UNSUPPORTED
            }
        })
    }

    fn lookup(name: &str) -> Option<xr::pfn::VoidFunction> {
        use std::mem::transmute as t;
        type V = xr::pfn::VoidFunction;
        macro_rules! table {
            ($($n:literal => $f:path as $p:ty),* $(,)?) => {
                match name {
                    $($n => Some(unsafe { t::<$p, V>($f) }),)*
                    _ => crate::gfx::lookup(name),
                }
            };
        }
        table! {
            "xrGetInstanceProcAddr" => get_instance_proc_addr as xr::pfn::GetInstanceProcAddr,
            "xrEnumerateApiLayerProperties" => api::enumerate_api_layers as xr::pfn::EnumerateApiLayerProperties,
            "xrEnumerateInstanceExtensionProperties" => api::enumerate_extensions as xr::pfn::EnumerateInstanceExtensionProperties,
            "xrCreateInstance" => api::create_instance as xr::pfn::CreateInstance,
            "xrDestroyInstance" => api::destroy_instance as xr::pfn::DestroyInstance,
            "xrGetInstanceProperties" => api::instance_properties as xr::pfn::GetInstanceProperties,
            "xrPollEvent" => api::poll_event as xr::pfn::PollEvent,
            "xrCreateHandTrackerEXT" => crate::handtrack::create_hand_tracker as xr::pfn::CreateHandTrackerEXT,
            "xrDestroyHandTrackerEXT" => crate::handtrack::destroy_hand_tracker as xr::pfn::DestroyHandTrackerEXT,
            "xrLocateHandJointsEXT" => crate::handtrack::locate_hand_joints as xr::pfn::LocateHandJointsEXT,
            "xrResultToString" => api::result_to_string as xr::pfn::ResultToString,
            "xrStructureTypeToString" => api::structure_type_to_string as xr::pfn::StructureTypeToString,
            "xrGetSystem" => api::get_system as xr::pfn::GetSystem,
            "xrGetSystemProperties" => api::system_properties as xr::pfn::GetSystemProperties,
            "xrEnumerateEnvironmentBlendModes" => api::enumerate_blend_modes as xr::pfn::EnumerateEnvironmentBlendModes,
            "xrEnumerateViewConfigurations" => api::enumerate_view_configurations as xr::pfn::EnumerateViewConfigurations,
            "xrGetViewConfigurationProperties" => api::view_configuration_properties as xr::pfn::GetViewConfigurationProperties,
            "xrEnumerateViewConfigurationViews" => api::enumerate_views as xr::pfn::EnumerateViewConfigurationViews,
            "xrCreateSession" => api::create_session as xr::pfn::CreateSession,
            "xrDestroySession" => api::destroy_session as xr::pfn::DestroySession,
            "xrBeginSession" => api::begin_session as xr::pfn::BeginSession,
            "xrEndSession" => api::end_session as xr::pfn::EndSession,
            "xrRequestExitSession" => api::request_exit_session as xr::pfn::RequestExitSession,
            "xrEnumerateReferenceSpaces" => api::enumerate_reference_spaces as xr::pfn::EnumerateReferenceSpaces,
            "xrCreateReferenceSpace" => api::create_reference_space as xr::pfn::CreateReferenceSpace,
            "xrGetReferenceSpaceBoundsRect" => api::reference_space_bounds as xr::pfn::GetReferenceSpaceBoundsRect,
            "xrCreateActionSpace" => input::create_action_space as xr::pfn::CreateActionSpace,
            "xrLocateSpace" => api::locate_space as xr::pfn::LocateSpace,
            "xrLocateSpaces" => api::locate_spaces as xr::pfn::LocateSpaces,
            "xrLocateSpacesKHR" => api::locate_spaces as xr::pfn::LocateSpaces,
            "xrDestroySpace" => api::destroy_space as xr::pfn::DestroySpace,
            "xrEnumerateSwapchainFormats" => api::enumerate_swapchain_formats as xr::pfn::EnumerateSwapchainFormats,
            "xrCreateSwapchain" => api::create_swapchain as xr::pfn::CreateSwapchain,
            "xrDestroySwapchain" => api::destroy_swapchain as xr::pfn::DestroySwapchain,
            "xrEnumerateSwapchainImages" => api::enumerate_swapchain_images as xr::pfn::EnumerateSwapchainImages,
            "xrAcquireSwapchainImage" => api::acquire_image as xr::pfn::AcquireSwapchainImage,
            "xrWaitSwapchainImage" => api::wait_image as xr::pfn::WaitSwapchainImage,
            "xrReleaseSwapchainImage" => api::release_image as xr::pfn::ReleaseSwapchainImage,
            "xrWaitFrame" => api::wait_frame as xr::pfn::WaitFrame,
            "xrBeginFrame" => api::begin_frame as xr::pfn::BeginFrame,
            "xrEndFrame" => api::end_frame as xr::pfn::EndFrame,
            "xrLocateViews" => api::locate_views as xr::pfn::LocateViews,
            "xrStringToPath" => input::string_to_path as xr::pfn::StringToPath,
            "xrPathToString" => input::path_to_string as xr::pfn::PathToString,
            "xrCreateActionSet" => input::create_action_set as xr::pfn::CreateActionSet,
            "xrDestroyActionSet" => input::destroy_action_set as xr::pfn::DestroyActionSet,
            "xrCreateAction" => input::create_action as xr::pfn::CreateAction,
            "xrDestroyAction" => input::destroy_action as xr::pfn::DestroyAction,
            "xrSuggestInteractionProfileBindings" => input::suggest_bindings as xr::pfn::SuggestInteractionProfileBindings,
            "xrAttachSessionActionSets" => input::attach_action_sets as xr::pfn::AttachSessionActionSets,
            "xrGetCurrentInteractionProfile" => input::current_interaction_profile as xr::pfn::GetCurrentInteractionProfile,
            "xrGetActionStateBoolean" => input::action_state_boolean as xr::pfn::GetActionStateBoolean,
            "xrGetActionStateFloat" => input::action_state_float as xr::pfn::GetActionStateFloat,
            "xrGetActionStateVector2f" => input::action_state_vector2f as xr::pfn::GetActionStateVector2f,
            "xrGetActionStatePose" => input::action_state_pose as xr::pfn::GetActionStatePose,
            "xrSyncActions" => input::sync_actions as xr::pfn::SyncActions,
            "xrEnumerateBoundSourcesForAction" => input::enumerate_bound_sources as xr::pfn::EnumerateBoundSourcesForAction,
            "xrGetInputSourceLocalizedName" => input::input_source_name as xr::pfn::GetInputSourceLocalizedName,
            "xrApplyHapticFeedback" => input::apply_haptic as xr::pfn::ApplyHapticFeedback,
            "xrStopHapticFeedback" => input::stop_haptic as xr::pfn::StopHapticFeedback,
        }
    }
}

/// Run `f`, turning a panic into XR_ERROR_RUNTIME_FAILURE (unwinding out of an
/// `extern "system"` function would abort the host process).
pub(crate) fn guard(name: &'static str, f: impl FnOnce() -> openxr_sys::Result) -> openxr_sys::Result {
    let detailed = log::hit(name);
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(r) => {
            if detailed || r.into_raw() < 0 {
                xlog!("{name} -> {r:?}");
            }
            r
        }
        Err(e) => {
            let msg = e
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| e.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_default();
            xlog!("{name} PANICKED: {msg}");
            openxr_sys::Result::ERROR_RUNTIME_FAILURE
        }
    }
}
