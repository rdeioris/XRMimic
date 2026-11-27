//! A record of every OpenXR call: name, arguments, outputs, result and time.
//!
//! Entries go to an in-memory ring (the headset window's Logs tab reads it)
//! and, with `XRMIMIC_TRACE_FILE`, to a file. `XRMIMIC_TRACE=0` turns it off.

use openxr_sys as xr;
use std::collections::VecDeque;
use std::ffi::{c_char, c_void, CStr};
use std::fmt::Write as _;
use std::io::Write as _;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::Instant;
use xr::Handle as _;

pub const CAPACITY: usize = 50_000;
const MAX_ARG: usize = 16 * 1024;

pub struct Entry {
    pub seq: u64,
    /// Milliseconds since the runtime was loaded.
    pub t_ms: f64,
    pub thread: u32,
    pub name: &'static str,
    pub args: String,
    pub result: xr::Result,
    pub outs: String,
    pub dur_us: u32,
}

/// Calls an app makes on every frame: the frame loop, view and space
/// locations, action sync and state, swapchain image handling, and event
/// polls that found nothing. Everything else (setup, teardown, events that
/// were delivered) is what the "hide per-frame calls" filter keeps.
pub fn is_per_frame(name: &str, result: xr::Result) -> bool {
    match name {
        "xrWaitFrame" | "xrBeginFrame" | "xrEndFrame" | "xrLocateViews" | "xrLocateSpace" | "xrLocateSpaces"
        | "xrLocateSpacesKHR" | "xrSyncActions" | "xrGetActionStateBoolean" | "xrGetActionStateFloat"
        | "xrGetActionStateVector2f" | "xrGetActionStatePose" | "xrAcquireSwapchainImage"
        | "xrWaitSwapchainImage" | "xrReleaseSwapchainImage" | "xrLocateHandJointsEXT" => true,
        "xrPollEvent" => result == xr::Result::EVENT_UNAVAILABLE,
        _ => false,
    }
}

impl Entry {
    pub fn per_frame(&self) -> bool {
        is_per_frame(self.name, self.result)
    }

    pub fn line(&self) -> String {
        let mut s = format!(
            "#{} {:.3} ms [t{}] {}({}) -> {:?} ({} us)",
            self.seq, self.t_ms, self.thread, self.name, self.args, self.result, self.dur_us
        );
        if !self.outs.is_empty() {
            let _ = write!(s, " => {}", self.outs);
        }
        s
    }
}

pub struct Ring {
    pub entries: VecDeque<Arc<Entry>>,
    pub seq: u64,
    file: Option<std::io::LineWriter<std::fs::File>>,
}

pub static RING: LazyLock<Mutex<Ring>> = LazyLock::new(|| {
    let file = std::env::var_os("XRMIMIC_TRACE_FILE")
        .and_then(|p| std::fs::OpenOptions::new().create(true).append(true).open(p).ok())
        .map(std::io::LineWriter::new);
    Mutex::new(Ring { entries: VecDeque::with_capacity(4096), seq: 0, file })
});

static START: LazyLock<Instant> = LazyLock::new(Instant::now);
static ENABLED: LazyLock<bool> = LazyLock::new(|| std::env::var("XRMIMIC_TRACE").map(|v| v != "0").unwrap_or(true));

fn thread_number() -> u32 {
    static NEXT: AtomicU32 = AtomicU32::new(1);
    thread_local!(static N: u32 = NEXT.fetch_add(1, Ordering::Relaxed));
    N.with(|n| *n)
}

/// The entries from `after` (exclusive) on, oldest first.
pub fn snapshot() -> Vec<Arc<Entry>> {
    RING.lock().unwrap_or_else(|e| e.into_inner()).entries.iter().cloned().collect()
}

pub fn clear() {
    RING.lock().unwrap_or_else(|e| e.into_inner()).entries.clear();
}

/// How an argument shows up in the trace: `before` describes what the app
/// passed in, `after` what the runtime wrote back (outputs only).
pub trait TraceArg {
    fn before(&self) -> String;
    fn after(&self) -> Option<String> {
        None
    }
}

/// Run an entry point, recording the call.
pub fn call(name: &'static str, args: &[(&'static str, &dyn TraceArg)], f: impl FnOnce() -> xr::Result) -> xr::Result {
    if !*ENABLED {
        return crate::guard(name, f);
    }
    let mut a = String::new();
    for (i, (n, v)) in args.iter().enumerate() {
        if i > 0 {
            a.push_str(", ");
        }
        let _ = write!(a, "{n}={}", clip(v.before()));
    }
    let t0 = Instant::now();
    let r = crate::guard(name, f);
    let dur = t0.elapsed();
    let mut outs = String::new();
    // Outputs are only written on plain success (not e.g. XR_EVENT_UNAVAILABLE).
    if r == xr::Result::SUCCESS {
        for (n, v) in args {
            if let Some(o) = v.after() {
                if !outs.is_empty() {
                    outs.push_str(", ");
                }
                let _ = write!(outs, "{n}={}", clip(o));
            }
        }
    }
    let t_ms = t0.duration_since(*START).as_secs_f64() * 1e3;
    let mut ring = RING.lock().unwrap_or_else(|e| e.into_inner());
    ring.seq += 1;
    let e = Arc::new(Entry {
        seq: ring.seq,
        t_ms,
        thread: thread_number(),
        name,
        args: a,
        result: r,
        outs,
        dur_us: dur.as_micros().min(u32::MAX as u128) as u32,
    });
    if let Some(f) = ring.file.as_mut() {
        let _ = writeln!(f, "{}", e.line());
    }
    if ring.entries.len() >= CAPACITY {
        ring.entries.pop_front();
    }
    ring.entries.push_back(e);
    r
}

fn clip(mut s: String) -> String {
    if s.len() > MAX_ARG {
        let mut cut = MAX_ARG;
        while !s.is_char_boundary(cut) {
            cut -= 1;
        }
        s.truncate(cut);
        s.push('…');
    }
    s
}

/// `name(args) ` wrapper used at every entry point.
#[macro_export]
macro_rules! traced {
    ($name:literal, [$($arg:ident),* $(,)?], $f:expr) => {
        $crate::trace::call($name, &[$((stringify!($arg), &$arg as &dyn $crate::trace::TraceArg)),*], $f)
    };
}

// ---------------------------------------------------------------- helpers

fn cstr_array(a: &[c_char]) -> String {
    let bytes: Vec<u8> = a.iter().take_while(|c| **c != 0).map(|c| *c as u8).collect();
    format!("{:?}", String::from_utf8_lossy(&bytes))
}

unsafe fn cstr_ptr(p: *const c_char) -> String {
    if p.is_null() {
        "null".into()
    } else {
        format!("{:?}", CStr::from_ptr(p).to_string_lossy())
    }
}

unsafe fn cstr_list(p: *const *const c_char, n: u32) -> String {
    if p.is_null() {
        return "[]".into();
    }
    let v: Vec<String> = (0..n as usize).map(|i| cstr_ptr(*p.add(i))).collect();
    format!("[{}]", v.join(", "))
}

/// A path as its string, if the runtime knows it (never waits for the lock).
pub fn path_name(p: u64) -> String {
    if p == 0 {
        return "NULL".into();
    }
    match crate::state::try_rt() {
        Some(rt) => match rt.path_str(p) {
            Some(s) => format!("{s:?}"),
            None => format!("Path({p})"),
        },
        None => format!("Path({p})"),
    }
}

unsafe fn chain(mut next: *const c_void) -> String {
    let mut v = Vec::new();
    while !next.is_null() && v.len() < 8 {
        let b = next as *const xr::BaseInStructure;
        v.push(format!("{:?}", (*b).ty));
        next = (*b).next as *const c_void;
    }
    if v.is_empty() {
        String::new()
    } else {
        format!(" next=[{}]", v.join(", "))
    }
}

fn ptr<T>(p: *const T) -> String {
    if p.is_null() {
        "null".into()
    } else {
        format!("{p:p}")
    }
}

// ---------------------------------------------------------------- values

macro_rules! by_debug {
    ($($t:ty),*) => {$(
        impl TraceArg for $t {
            fn before(&self) -> String { format!("{:?}", self) }
        }
    )*};
}
by_debug!(
    u32,
    xr::Instance,
    xr::ReferenceSpaceType,
    xr::Result,
    xr::Session,
    xr::Space,
    xr::StructureType,
    xr::Swapchain,
    xr::SystemId,
    xr::ViewConfigurationType,
    xr::HandTrackerEXT
);

/// An action handle with the name the app gave it.
pub fn action_name(a: xr::Action) -> String {
    let id = a.into_raw();
    match crate::state::try_rt().and_then(|rt| rt.actions.get(&id).map(|x| x.name.clone())) {
        Some(n) => format!("Action({id} {n:?})"),
        None => format!("Action({id})"),
    }
}

pub fn action_set_name(a: xr::ActionSet) -> String {
    let id = a.into_raw();
    match crate::state::try_rt().and_then(|rt| rt.action_sets.get(&id).map(|x| x.name.clone())) {
        Some(n) => format!("ActionSet({id} {n:?})"),
        None => format!("ActionSet({id})"),
    }
}

impl TraceArg for xr::Action {
    fn before(&self) -> String {
        action_name(*self)
    }
}

impl TraceArg for xr::ActionSet {
    fn before(&self) -> String {
        action_set_name(*self)
    }
}

impl TraceArg for xr::Path {
    fn before(&self) -> String {
        path_name(self.into_raw())
    }
}

impl TraceArg for xr::Time {
    fn before(&self) -> String {
        format!("{} ns", self.as_nanos())
    }
}

// ---------------------------------------------------------------- inputs

macro_rules! in_by_debug {
    ($($t:ty),*) => {$(
        impl TraceArg for *const $t {
            fn before(&self) -> String {
                if self.is_null() { "null".into() } else { unsafe { format!("{:?}", **self) } }
            }
        }
    )*};
}
in_by_debug!(
    i64,
    xr::FrameBeginInfo,
    xr::FrameWaitInfo,
    xr::ReferenceSpaceCreateInfo,
    xr::SessionBeginInfo,
    xr::SwapchainCreateInfo,
    xr::SwapchainImageAcquireInfo,
    xr::SwapchainImageReleaseInfo,
    xr::SwapchainImageWaitInfo,
    xr::SystemGetInfo,
    xr::ViewLocateInfo,
    xr::VulkanGraphicsDeviceGetInfoKHR,
    xr::HandTrackerCreateInfoEXT,
    xr::HandJointsLocateInfoEXT
);

impl TraceArg for *const xr::VulkanInstanceCreateInfoKHR {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe {
            let i = &**self;
            format!(
                "{{system={:?}, flags={:?}, vkGetInstanceProcAddr={}, VkInstanceCreateInfo={}}}",
                i.system_id,
                i.create_flags,
                if i.pfn_get_instance_proc_addr.is_some() { "set" } else { "null" },
                ptr(i.vulkan_create_info)
            )
        }
    }
}

impl TraceArg for *const xr::VulkanDeviceCreateInfoKHR {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe {
            let i = &**self;
            format!(
                "{{system={:?}, flags={:?}, physical_device={}, VkDeviceCreateInfo={}}}",
                i.system_id,
                i.create_flags,
                ptr(i.vulkan_physical_device),
                ptr(i.vulkan_create_info)
            )
        }
    }
}

impl TraceArg for *const c_char {
    fn before(&self) -> String {
        unsafe { cstr_ptr(*self) }
    }
}

impl TraceArg for *const xr::InstanceCreateInfo {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe {
            let i = &**self;
            let a = &i.application_info;
            format!(
                "{{app={} v{}, engine={} v{}, api={}, layers={}, extensions={}{}}}",
                cstr_array(&a.application_name),
                a.application_version,
                cstr_array(&a.engine_name),
                a.engine_version,
                a.api_version,
                cstr_list(i.enabled_api_layer_names, i.enabled_api_layer_count),
                cstr_list(i.enabled_extension_names, i.enabled_extension_count),
                chain(i.next)
            )
        }
    }
}

impl TraceArg for *const xr::SessionCreateInfo {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe {
            let i = &**self;
            format!("{{system={:?}, flags={:?}{}}}", i.system_id, i.create_flags, chain(i.next))
        }
    }
}

impl TraceArg for *const xr::ActionSetCreateInfo {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe {
            let i = &**self;
            format!(
                "{{name={}, localized={}, priority={}}}",
                cstr_array(&i.action_set_name),
                cstr_array(&i.localized_action_set_name),
                i.priority
            )
        }
    }
}

impl TraceArg for *const xr::ActionCreateInfo {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe {
            let i = &**self;
            let subs: Vec<String> =
                (0..i.count_subaction_paths as usize).map(|k| path_name((*i.subaction_paths.add(k)).into_raw())).collect();
            format!(
                "{{name={}, type={:?}, subactions=[{}], localized={}}}",
                cstr_array(&i.action_name),
                i.action_type,
                subs.join(", "),
                cstr_array(&i.localized_action_name)
            )
        }
    }
}

impl TraceArg for *const xr::ActionStateGetInfo {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe {
            let i = &**self;
            format!("{{action={}, subaction={}}}", action_name(i.action), path_name(i.subaction_path.into_raw()))
        }
    }
}

impl TraceArg for *const xr::HapticActionInfo {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe {
            let i = &**self;
            format!("{{action={}, subaction={}}}", action_name(i.action), path_name(i.subaction_path.into_raw()))
        }
    }
}

impl TraceArg for *const xr::HapticBaseHeader {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe {
            if (**self).ty == xr::StructureType::HAPTIC_VIBRATION {
                format!("{:?}", *(*self as *const xr::HapticVibration))
            } else {
                format!("{{type={:?}}}", (**self).ty)
            }
        }
    }
}

impl TraceArg for *const xr::ActionSpaceCreateInfo {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe {
            let i = &**self;
            format!(
                "{{action={}, subaction={}, pose={:?}}}",
                action_name(i.action),
                path_name(i.subaction_path.into_raw()),
                i.pose_in_action_space
            )
        }
    }
}

impl TraceArg for *const xr::InteractionProfileSuggestedBinding {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe {
            let i = &**self;
            let b: Vec<String> = (0..i.count_suggested_bindings as usize)
                .map(|k| {
                    let s = &*i.suggested_bindings.add(k);
                    format!("{}={}", action_name(s.action), path_name(s.binding.into_raw()))
                })
                .collect();
            format!("{{profile={}, bindings=[{}]}}", path_name(i.interaction_profile.into_raw()), b.join(", "))
        }
    }
}

impl TraceArg for *const xr::SessionActionSetsAttachInfo {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe {
            let i = &**self;
            let v: Vec<String> = (0..i.count_action_sets as usize).map(|k| action_set_name(*i.action_sets.add(k))).collect();
            format!("{{action_sets=[{}]}}", v.join(", "))
        }
    }
}

impl TraceArg for *const xr::ActionsSyncInfo {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe {
            let i = &**self;
            let v: Vec<String> = (0..i.count_active_action_sets as usize)
                .map(|k| {
                    let a = &*i.active_action_sets.add(k);
                    format!("{}/{}", action_set_name(a.action_set), path_name(a.subaction_path.into_raw()))
                })
                .collect();
            format!("{{active=[{}]}}", v.join(", "))
        }
    }
}

impl TraceArg for *const xr::BoundSourcesForActionEnumerateInfo {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe { format!("{{action={}}}", action_name((**self).action)) }
    }
}

impl TraceArg for *const xr::InputSourceLocalizedNameGetInfo {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe {
            let i = &**self;
            format!("{{source={}, components={:?}}}", path_name(i.source_path.into_raw()), i.which_components)
        }
    }
}

impl TraceArg for *const xr::SpacesLocateInfo {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe {
            let i = &**self;
            let v: Vec<String> = (0..i.space_count as usize).map(|k| format!("{:?}", *i.spaces.add(k))).collect();
            format!("{{base={:?}, time={} ns, spaces=[{}]}}", i.base_space, i.time.as_nanos(), v.join(", "))
        }
    }
}

impl TraceArg for *const xr::FrameEndInfo {
    fn before(&self) -> String {
        if self.is_null() {
            return "null".into();
        }
        unsafe {
            let i = &**self;
            let mut layers = Vec::new();
            for k in 0..i.layer_count as usize {
                let l = *i.layers.add(k);
                if l.is_null() {
                    layers.push("null".to_string());
                    continue;
                }
                if (*l).ty == xr::StructureType::COMPOSITION_LAYER_PROJECTION {
                    let p = &*(l as *const xr::CompositionLayerProjection);
                    let views: Vec<String> = (0..p.view_count as usize)
                        .map(|v| {
                            let view = &*p.views.add(v);
                            let s = &view.sub_image;
                            let di = crate::api::find_in_chain(
                                view.next,
                                xr::StructureType::COMPOSITION_LAYER_DEPTH_INFO_KHR,
                            ) as *const xr::CompositionLayerDepthInfoKHR;
                            let depth = if di.is_null() {
                                String::new()
                            } else {
                                let d = &*di;
                                format!(
                                    ", depth={{swapchain={:?}, rect={:?}, layer={}, depth {}..{}, z {}..{}}}",
                                    d.sub_image.swapchain,
                                    d.sub_image.image_rect,
                                    d.sub_image.image_array_index,
                                    d.min_depth,
                                    d.max_depth,
                                    d.near_z,
                                    d.far_z
                                )
                            };
                            format!(
                                "{{pose={:?}, fov={:?}, swapchain={:?}, rect={:?}, layer={}{depth}}}",
                                view.pose, view.fov, s.swapchain, s.image_rect, s.image_array_index
                            )
                        })
                        .collect();
                    layers.push(format!(
                        "Projection{{space={:?}, flags={:?}, views=[{}]{}}}",
                        p.space,
                        p.layer_flags,
                        views.join(", "),
                        chain(p.next)
                    ));
                } else {
                    layers.push(format!("{:?}{{space={:?}}}", (*l).ty, (*l).space));
                }
            }
            format!(
                "{{display_time={} ns, blend={:?}, layers=[{}]}}",
                i.display_time.as_nanos(),
                i.environment_blend_mode,
                layers.join(", ")
            )
        }
    }
}

// ---------------------------------------------------------------- outputs

macro_rules! out_by_debug {
    ($($t:ty),*) => {$(
        impl TraceArg for *mut $t {
            fn before(&self) -> String { ptr(*self) }
            fn after(&self) -> Option<String> {
                if self.is_null() { None } else { Some(unsafe { format!("{:?}", **self) }) }
            }
        }
    )*};
}
// Arrays filled by the two-call idiom show their first element.
out_by_debug!(
    i64,
    u32,
    i32,
    xr::Action,
    xr::ActionSet,
    xr::ActionStateBoolean,
    xr::ActionStateFloat,
    xr::ActionStatePose,
    xr::ActionStateVector2f,
    xr::EnvironmentBlendMode,
    xr::Extent2Df,
    xr::FrameState,
    xr::GraphicsRequirementsVulkanKHR,
    xr::Instance,
    xr::ReferenceSpaceType,
    xr::Session,
    xr::Space,
    xr::SpaceLocation,
    xr::Swapchain,
    xr::SystemId,
    xr::Time,
    xr::View,
    xr::ViewConfigurationProperties,
    xr::ViewConfigurationType,
    xr::ViewConfigurationView,
    xr::ViewState,
    xr::HandTrackerEXT,
    xr::HandJointLocationsEXT,
    *const c_void
);

#[cfg(windows)]
impl TraceArg for *mut xr::GraphicsRequirementsD3D11KHR {
    fn before(&self) -> String {
        ptr(*self)
    }
    fn after(&self) -> Option<String> {
        if self.is_null() {
            return None;
        }
        unsafe {
            let r = &**self;
            Some(format!(
                "{{adapter_luid={:#x}:{:#x}, min_feature_level={:#x}}}",
                r.adapter_luid.HighPart, r.adapter_luid.LowPart, r.min_feature_level
            ))
        }
    }
}

impl TraceArg for *mut c_char {
    fn before(&self) -> String {
        ptr(*self)
    }
    fn after(&self) -> Option<String> {
        if self.is_null() {
            None
        } else {
            Some(unsafe { cstr_ptr(*self) })
        }
    }
}

impl TraceArg for *mut xr::Path {
    fn before(&self) -> String {
        ptr(*self)
    }
    fn after(&self) -> Option<String> {
        if self.is_null() {
            None
        } else {
            Some(unsafe { format!("{} {}", (**self).into_raw(), path_name((**self).into_raw())) })
        }
    }
}

impl TraceArg for *mut xr::InteractionProfileState {
    fn before(&self) -> String {
        ptr(*self)
    }
    fn after(&self) -> Option<String> {
        if self.is_null() {
            None
        } else {
            Some(unsafe { path_name((**self).interaction_profile.into_raw()) })
        }
    }
}

impl TraceArg for *mut xr::SystemProperties {
    fn before(&self) -> String {
        ptr(*self)
    }
    fn after(&self) -> Option<String> {
        if self.is_null() {
            return None;
        }
        unsafe {
            let p = &**self;
            Some(format!(
                "{{system={:?}, vendor={:#x}, name={}, graphics={:?}, tracking={:?}}}",
                p.system_id,
                p.vendor_id,
                cstr_array(&p.system_name),
                p.graphics_properties,
                p.tracking_properties
            ))
        }
    }
}

impl TraceArg for *mut xr::InstanceProperties {
    fn before(&self) -> String {
        ptr(*self)
    }
    fn after(&self) -> Option<String> {
        if self.is_null() {
            return None;
        }
        unsafe {
            let p = &**self;
            Some(format!("{{runtime={} {}}}", cstr_array(&p.runtime_name), p.runtime_version))
        }
    }
}

impl TraceArg for *mut xr::ExtensionProperties {
    fn before(&self) -> String {
        ptr(*self)
    }
    fn after(&self) -> Option<String> {
        if self.is_null() {
            return None;
        }
        unsafe {
            // Filled only when capacity covered every extension; the count comes back too.
            let n = crate::api::extensions().len();
            let v: Vec<String> = (0..n)
                .map(|k| {
                    let e = &*self.add(k);
                    format!("{} v{}", cstr_array(&e.extension_name), e.extension_version)
                })
                .collect();
            Some(format!("[{}]", v.join(", ")))
        }
    }
}

impl TraceArg for *mut xr::ApiLayerProperties {
    fn before(&self) -> String {
        ptr(*self)
    }
}

impl TraceArg for *mut xr::SwapchainImageBaseHeader {
    fn before(&self) -> String {
        ptr(*self)
    }
}

impl TraceArg for *mut xr::SpaceLocations {
    fn before(&self) -> String {
        ptr(*self)
    }
    fn after(&self) -> Option<String> {
        if self.is_null() {
            return None;
        }
        unsafe {
            let l = &**self;
            let v: Vec<String> = (0..l.location_count as usize).map(|k| format!("{:?}", *l.locations.add(k))).collect();
            Some(format!("[{}]", v.join(", ")))
        }
    }
}

impl TraceArg for *mut xr::EventDataBuffer {
    fn before(&self) -> String {
        ptr(*self)
    }
    fn after(&self) -> Option<String> {
        if self.is_null() {
            return None;
        }
        unsafe {
            let ty = (**self).ty;
            Some(match ty {
                xr::StructureType::EVENT_DATA_SESSION_STATE_CHANGED => {
                    let e = &*(*self as *const xr::EventDataSessionStateChanged);
                    format!("SessionStateChanged{{session={:?}, state={:?}, time={} ns}}", e.session, e.state, e.time.as_nanos())
                }
                xr::StructureType::EVENT_DATA_INTERACTION_PROFILE_CHANGED => {
                    let e = &*(*self as *const xr::EventDataInteractionProfileChanged);
                    format!("InteractionProfileChanged{{session={:?}}}", e.session)
                }
                other => format!("{other:?}"),
            })
        }
    }
}

impl TraceArg for *mut Option<xr::pfn::VoidFunction> {
    fn before(&self) -> String {
        ptr(*self)
    }
    fn after(&self) -> Option<String> {
        if self.is_null() {
            None
        } else {
            Some(unsafe { if (**self).is_some() { "found" } else { "null" } }.to_string())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn per_frame_calls_are_the_frame_loop_and_empty_polls() {
        for n in ["xrWaitFrame", "xrEndFrame", "xrLocateViews", "xrGetActionStateBoolean", "xrSyncActions"] {
            assert!(is_per_frame(n, xr::Result::SUCCESS), "{n}");
        }
        assert!(is_per_frame("xrPollEvent", xr::Result::EVENT_UNAVAILABLE));
        // A poll that delivered an event (a state change) stays visible.
        assert!(!is_per_frame("xrPollEvent", xr::Result::SUCCESS));
        for n in ["xrCreateSession", "xrBeginSession", "xrCreateSwapchain", "xrApplyHapticFeedback", "xrGetSystem"] {
            assert!(!is_per_frame(n, xr::Result::SUCCESS), "{n}");
        }
    }
}
