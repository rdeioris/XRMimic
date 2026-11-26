//! Paths, action sets, actions, bindings and action state.

use crate::api::{fill, read_str};
use crate::state::*;
use crate::{time, xlog};
use openxr_sys as xr;
use std::ffi::{c_char, CStr};
use xr::Handle as _;
use xrmimic_core::bindings::{self, Source, Value, SIMPLE_PROFILE, TOUCH_PROFILE};
use xrmimic_core::device::CONTROLLERS;
use xrmimic_core::haptics::Vibration;
use xrmimic_core::math::Pose;
use xrmimic_core::sim::{ControllerInput, SimState, LEFT, RIGHT};

const HANDS: [&str; 2] = ["/user/hand/left", "/user/hand/right"];

fn valid_path(s: &str) -> bool {
    s.starts_with('/')
        && !s.ends_with('/')
        && !s.contains("//")
        && s.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"/-_.".contains(&b))
}

pub unsafe extern "system" fn string_to_path(
    instance: xr::Instance,
    s: *const c_char,
    out: *mut xr::Path,
) -> xr::Result {
    crate::traced!("xrStringToPath", [instance, s, out], || {
        let mut rt = rt();
        if !rt.instances.contains_key(&instance.into_raw()) {
            return xr::Result::ERROR_HANDLE_INVALID;
        }
        let s = CStr::from_ptr(s).to_string_lossy();
        if !valid_path(&s) {
            return xr::Result::ERROR_PATH_FORMAT_INVALID;
        }
        *out = xr::Path::from_raw(rt.intern(&s));
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn path_to_string(
    instance: xr::Instance,
    path: xr::Path,
    capacity: u32,
    count: *mut u32,
    buf: *mut c_char,
) -> xr::Result {
    crate::traced!("xrPathToString", [instance, path, capacity, count, buf], || {
        let rt = rt();
        if !rt.instances.contains_key(&instance.into_raw()) {
            return xr::Result::ERROR_HANDLE_INVALID;
        }
        let Some(s) = rt.path_str(path.into_raw()) else {
            return xr::Result::ERROR_PATH_INVALID;
        };
        let mut bytes: Vec<c_char> = s.bytes().map(|b| b as c_char).collect();
        bytes.push(0);
        fill(capacity, count, buf, &bytes)
    })
}

pub unsafe extern "system" fn create_action_set(
    instance: xr::Instance,
    info: *const xr::ActionSetCreateInfo,
    out: *mut xr::ActionSet,
) -> xr::Result {
    crate::traced!("xrCreateActionSet", [instance, info, out], || {
        let mut rt = rt();
        if !rt.instances.contains_key(&instance.into_raw()) {
            return xr::Result::ERROR_HANDLE_INVALID;
        }
        let name = read_str(&(*info).action_set_name);
        xlog!("action set '{name}'");
        let h = rt.new_handle();
        rt.action_sets.insert(h, ActionSetData { instance: instance.into_raw(), name, attached: false });
        *out = xr::ActionSet::from_raw(h);
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn destroy_action_set(set: xr::ActionSet) -> xr::Result {
    crate::traced!("xrDestroyActionSet", [set], || {
        let mut rt = rt();
        if rt.action_sets.remove(&set.into_raw()).is_none() {
            return xr::Result::ERROR_HANDLE_INVALID;
        }
        rt.actions.retain(|_, a| a.set != set.into_raw());
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn create_action(
    set: xr::ActionSet,
    info: *const xr::ActionCreateInfo,
    out: *mut xr::Action,
) -> xr::Result {
    crate::traced!("xrCreateAction", [set, info, out], || {
        let mut rt = rt();
        let Some(s) = rt.action_sets.get(&set.into_raw()) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        if s.attached {
            return xr::Result::ERROR_ACTIONSETS_ALREADY_ATTACHED;
        }
        let info = &*info;
        let name = read_str(&info.action_name);
        let subactions: Vec<u64> =
            (0..info.count_subaction_paths as usize).map(|i| (*info.subaction_paths.add(i)).into_raw()).collect();
        let subs: Vec<String> = subactions.iter().filter_map(|p| rt.path_str(*p)).map(String::from).collect();
        xlog!("action '{name}' {:?} subactions {subs:?}", info.action_type);
        let h = rt.new_handle();
        rt.actions.insert(h, ActionData { set: set.into_raw(), name, ty: info.action_type, subactions });
        *out = xr::Action::from_raw(h);
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn destroy_action(action: xr::Action) -> xr::Result {
    crate::traced!("xrDestroyAction", [action], || {
        if rt().actions.remove(&action.into_raw()).is_some() {
            xr::Result::SUCCESS
        } else {
            xr::Result::ERROR_HANDLE_INVALID
        }
    })
}

pub unsafe extern "system" fn suggest_bindings(
    instance: xr::Instance,
    info: *const xr::InteractionProfileSuggestedBinding,
) -> xr::Result {
    crate::traced!("xrSuggestInteractionProfileBindings", [instance, info], || {
        let mut rt = rt();
        let info = &*info;
        let profile = info.interaction_profile.into_raw();
        let pname = rt.path_str(profile).unwrap_or("?").to_string();
        let mut list = Vec::new();
        let mut names = Vec::new();
        for i in 0..info.count_suggested_bindings as usize {
            let b = &*info.suggested_bindings.add(i);
            let path = rt.path_str(b.binding.into_raw()).unwrap_or("?").to_string();
            let aname = rt.actions.get(&b.action.into_raw()).map(|a| a.name.clone()).unwrap_or_default();
            if CONTROLLERS.iter().any(|(p, _, _)| *p == pname) && bindings::resolve(&pname, &path).is_none() {
                xlog!("  unresolved binding {aname} -> {path}");
            }
            names.push(format!("{aname}={path}"));
            list.push((b.action.into_raw(), b.binding.into_raw()));
        }
        xlog!("suggested {pname}: {}", names.join(", "));
        let Some(inst) = rt.instances.get_mut(&instance.into_raw()) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        // Other vendors' profiles are accepted and ignored, as a real runtime would.
        inst.suggested.insert(profile, list);
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn attach_action_sets(
    session: xr::Session,
    info: *const xr::SessionActionSetsAttachInfo,
) -> xr::Result {
    crate::traced!("xrAttachSessionActionSets", [session, info], || {
        let mut rt = rt();
        let h = session.into_raw();
        let info = &*info;
        let sets: Vec<u64> = (0..info.count_action_sets as usize).map(|i| (*info.action_sets.add(i)).into_raw()).collect();
        let Some(s) = rt.sessions.get(&h) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        if !s.attached_sets.is_empty() {
            return xr::Result::ERROR_ACTIONSETS_ALREADY_ATTACHED;
        }
        let instance = s.instance;
        for set in &sets {
            if let Some(a) = rt.action_sets.get_mut(set) {
                a.attached = true;
            }
        }
        let _ = instance;
        xlog!("attached {} action sets", sets.len());
        rt.sessions.get_mut(&h).unwrap().attached_sets = sets;
        refresh_profile(&mut rt, h, true);
        xr::Result::SUCCESS
    })
}

/// The interaction profile a session should report now. Tracked hands: the
/// first hand profile the app suggested bindings for (else none). Controllers:
/// the device's if the app suggested bindings for them, else the closest the
/// app does know: Touch, then the simple controller.
fn choose_profile(rt: &Runtime, instance: u64, hands: bool) -> u64 {
    let suggested = &rt.instances[&instance].suggested;
    let pick = |list: &[&str]| list.iter().map(|p| rt.path_id(p)).find(|id| *id != 0 && suggested.contains_key(id)).unwrap_or(0);
    if hands {
        let names: Vec<&str> = xrmimic_core::device::HAND_PROFILES.iter().map(|(p, _, _)| *p).collect();
        return pick(&names);
    }
    let wanted = crate::devstore::current().controllers;
    let profile = pick(&[wanted.as_str(), TOUCH_PROFILE, SIMPLE_PROFILE]);
    if rt.path_id(&wanted) != profile {
        xlog!("the app suggested no bindings for {wanted}; reporting {}", rt.path_str(profile).unwrap_or("none"));
    }
    profile
}

/// Set the session's interaction profile to what it should report now (at
/// attach, and when the controllers are put down or picked up). On a change
/// the app gets XR_TYPE_EVENT_DATA_INTERACTION_PROFILE_CHANGED and the Input
/// tab the new profile and the app's actions bound in it.
pub fn refresh_profile(rt: &mut Runtime, h: u64, initial: bool) {
    let Some(s) = rt.sessions.get(&h) else { return };
    if s.attached_sets.is_empty() {
        return;
    }
    let hands = s.gfx.shared.hands.load(std::sync::atomic::Ordering::SeqCst);
    if !initial && hands == s.hands {
        return;
    }
    let instance = s.instance;
    let profile = choose_profile(rt, instance, hands);
    let bound: Vec<(String, String)> = rt.instances[&instance]
        .suggested
        .get(&profile)
        .map(|list| {
            list.iter()
                .filter_map(|(action, path)| {
                    let name = rt.actions.get(action)?.name.clone();
                    Some((rt.path_str(*path)?.to_string(), name))
                })
                .collect()
        })
        .unwrap_or_default();
    let name = rt.path_str(profile).unwrap_or("").to_string();
    xlog!(
        "{}; interaction profile {}",
        if hands { "hands in use" } else { "controllers in use" },
        if name.is_empty() { "none" } else { &name }
    );
    let s = rt.sessions.get_mut(&h).unwrap();
    let changed = s.profile != profile;
    s.hands = hands;
    s.profile = profile;
    *s.gfx.shared.profile.lock().unwrap() = name;
    *s.gfx.shared.bound.lock().unwrap() = bound;
    s.gfx.shared.attached.store(true, std::sync::atomic::Ordering::SeqCst);
    if changed || (initial && profile != 0) {
        rt.push_event(instance, Event::ProfileChanged { session: h });
    }
}

pub unsafe extern "system" fn current_interaction_profile(
    session: xr::Session,
    top: xr::Path,
    out: *mut xr::InteractionProfileState,
) -> xr::Result {
    crate::traced!("xrGetCurrentInteractionProfile", [session, top, out], || {
        let rt = rt();
        let Some(s) = rt.sessions.get(&session.into_raw()) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        if s.attached_sets.is_empty() {
            return xr::Result::ERROR_ACTIONSET_NOT_ATTACHED;
        }
        let user = rt.path_str(top.into_raw()).unwrap_or("");
        let p = if HANDS.contains(&user) { s.profile } else { 0 };
        (*out).interaction_profile = xr::Path::from_raw(p);
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn sync_actions(session: xr::Session, info: *const xr::ActionsSyncInfo) -> xr::Result {
    crate::traced!("xrSyncActions", [session, info], || {
        let mut rt = rt();
        refresh_profile(&mut rt, session.into_raw(), false);
        let Some(s) = rt.sessions.get_mut(&session.into_raw()) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        if s.attached_sets.is_empty() {
            return xr::Result::ERROR_ACTIONSET_NOT_ATTACHED;
        }
        let info = &*info;
        s.active_sets =
            (0..info.count_active_action_sets as usize).map(|i| (*info.active_action_sets.add(i)).action_set.into_raw()).collect();
        s.previous = s.snapshot;
        s.snapshot = s.sim.lock().unwrap().controllers;
        s.sync_time = time::now_ns();
        if s.state != xr::SessionState::FOCUSED {
            return xr::Result::SESSION_NOT_FOCUSED;
        }
        xr::Result::SUCCESS
    })
}

fn hand_of(rt: &Runtime, path: u64) -> Option<usize> {
    match rt.path_str(path)? {
        "/user/hand/left" => Some(LEFT),
        "/user/hand/right" => Some(RIGHT),
        _ => None,
    }
}

/// Sources bound to `action` for `hand` (None = either hand) in the current profile.
fn sources(rt: &Runtime, s: &SessionData, action: u64, hand: Option<usize>) -> Vec<(usize, Source)> {
    let Some(profile) = rt.path_str(s.profile) else { return Vec::new() };
    let Some(list) = rt.instances.get(&s.instance).and_then(|i| i.suggested.get(&s.profile)) else {
        return Vec::new();
    };
    list.iter()
        .filter(|(a, _)| *a == action)
        .filter_map(|(_, b)| bindings::resolve(profile, rt.path_str(*b)?))
        .filter(|(h, _)| hand.is_none_or(|w| w == *h))
        .collect()
}

struct Lookup {
    action: u64,
    subaction: u64,
    hand: Option<usize>,
    active: bool,
}

/// Validate an action state query; Err is the result to return.
fn lookup(rt: &Runtime, s: &SessionData, info: &xr::ActionStateGetInfo, ty: &[xr::ActionType]) -> Result<Lookup, xr::Result> {
    let action = info.action.into_raw();
    let Some(a) = rt.actions.get(&action) else {
        return Err(xr::Result::ERROR_HANDLE_INVALID);
    };
    if !ty.contains(&a.ty) {
        return Err(xr::Result::ERROR_ACTION_TYPE_MISMATCH);
    }
    if !s.attached_sets.contains(&a.set) {
        return Err(xr::Result::ERROR_ACTIONSET_NOT_ATTACHED);
    }
    let sub = info.subaction_path.into_raw();
    let hand = if sub == 0 {
        None
    } else {
        if !a.subactions.contains(&sub) {
            return Err(xr::Result::ERROR_PATH_UNSUPPORTED);
        }
        Some(hand_of(rt, sub).ok_or(xr::Result::ERROR_PATH_UNSUPPORTED)?)
    };
    let active = s.active_sets.contains(&a.set) && s.state == xr::SessionState::FOCUSED;
    Ok(Lookup { action, subaction: sub, hand, active })
}

/// Current and previous value, combined over all bound sources.
fn values(rt: &Runtime, s: &SessionData, l: &Lookup, combine: fn(Value, Value) -> Value) -> Option<(Value, Value)> {
    let src = sources(rt, s, l.action, l.hand);
    let read = |c: &[ControllerInput; 2]| {
        src.iter().filter(|(_, s)| !s.is_pose()).map(|(h, s)| bindings::read(&c[*h], *s)).reduce(combine)
    };
    Some((read(&s.snapshot)?, read(&s.previous)?))
}

fn change_time(s: &mut SessionData, l: &Lookup, changed: bool) -> xr::Time {
    let key = (l.action, l.subaction);
    if changed {
        s.change_times.insert(key, s.sync_time);
    }
    xr::Time::from_nanos(s.change_times.get(&key).copied().unwrap_or(0))
}

macro_rules! state_query {
    ($name:literal, $session:ident, $info:ident, $out:ident, $types:expr, |$rt:ident, $s:ident, $l:ident| $body:block) => {
        crate::traced!($name, [$session, $info, $out], || {
            let mut guard_rt = rt();
            let h = $session.into_raw();
            let Some(sess) = guard_rt.sessions.get(&h) else {
                return xr::Result::ERROR_HANDLE_INVALID;
            };
            let l = match lookup(&guard_rt, sess, &*$info, $types) {
                Ok(l) => l,
                Err(e) => return e,
            };
            let $rt: &mut Runtime = &mut guard_rt;
            let $l = l;
            let $s = h;
            $body
        })
    };
}

pub unsafe extern "system" fn action_state_boolean(
    session: xr::Session,
    info: *const xr::ActionStateGetInfo,
    out: *mut xr::ActionStateBoolean,
) -> xr::Result {
    state_query!("xrGetActionStateBoolean", session, info, out, &[xr::ActionType::BOOLEAN_INPUT], |rt, h, l| {
        let o = &mut *out;
        let v = if l.active { values(rt, &rt.sessions[&h], &l, |a, b| Value::Bool(a.as_bool() || b.as_bool())) } else { None };
        let s = rt.sessions.get_mut(&h).unwrap();
        match v {
            Some((cur, prev)) => {
                let changed = cur.as_bool() != prev.as_bool();
                o.current_state = cur.as_bool().into();
                o.changed_since_last_sync = changed.into();
                o.last_change_time = change_time(s, &l, changed);
                o.is_active = xr::TRUE;
            }
            None => {
                o.current_state = xr::FALSE;
                o.changed_since_last_sync = xr::FALSE;
                o.last_change_time = xr::Time::from_nanos(0);
                o.is_active = xr::FALSE;
            }
        }
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn action_state_float(
    session: xr::Session,
    info: *const xr::ActionStateGetInfo,
    out: *mut xr::ActionStateFloat,
) -> xr::Result {
    state_query!("xrGetActionStateFloat", session, info, out, &[xr::ActionType::FLOAT_INPUT], |rt, h, l| {
        let o = &mut *out;
        let v = if l.active {
            values(rt, &rt.sessions[&h], &l, |a, b| {
                if a.as_float().abs() >= b.as_float().abs() { Value::Float(a.as_float()) } else { Value::Float(b.as_float()) }
            })
        } else {
            None
        };
        let s = rt.sessions.get_mut(&h).unwrap();
        match v {
            Some((cur, prev)) => {
                let changed = cur.as_float() != prev.as_float();
                o.current_state = cur.as_float();
                o.changed_since_last_sync = changed.into();
                o.last_change_time = change_time(s, &l, changed);
                o.is_active = xr::TRUE;
            }
            None => {
                o.current_state = 0.0;
                o.changed_since_last_sync = xr::FALSE;
                o.last_change_time = xr::Time::from_nanos(0);
                o.is_active = xr::FALSE;
            }
        }
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn action_state_vector2f(
    session: xr::Session,
    info: *const xr::ActionStateGetInfo,
    out: *mut xr::ActionStateVector2f,
) -> xr::Result {
    state_query!("xrGetActionStateVector2f", session, info, out, &[xr::ActionType::VECTOR2F_INPUT], |rt, h, l| {
        let o = &mut *out;
        let v = if l.active {
            values(rt, &rt.sessions[&h], &l, |a, b| {
                let (x, y) = (a.as_vec2(), b.as_vec2());
                if x[0].hypot(x[1]) >= y[0].hypot(y[1]) { Value::Vec2(x) } else { Value::Vec2(y) }
            })
        } else {
            None
        };
        let s = rt.sessions.get_mut(&h).unwrap();
        match v {
            Some((cur, prev)) => {
                let (c, p) = (cur.as_vec2(), prev.as_vec2());
                let changed = c != p;
                o.current_state = xr::Vector2f { x: c[0], y: c[1] };
                o.changed_since_last_sync = changed.into();
                o.last_change_time = change_time(s, &l, changed);
                o.is_active = xr::TRUE;
            }
            None => {
                o.current_state = xr::Vector2f { x: 0.0, y: 0.0 };
                o.changed_since_last_sync = xr::FALSE;
                o.last_change_time = xr::Time::from_nanos(0);
                o.is_active = xr::FALSE;
            }
        }
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn action_state_pose(
    session: xr::Session,
    info: *const xr::ActionStateGetInfo,
    out: *mut xr::ActionStatePose,
) -> xr::Result {
    state_query!("xrGetActionStatePose", session, info, out, &[xr::ActionType::POSE_INPUT], |rt, h, l| {
        let s = &rt.sessions[&h];
        let bound = sources(rt, s, l.action, l.hand).iter().any(|(_, src)| src.is_pose());
        (*out).is_active = (l.active && bound).into();
        xr::Result::SUCCESS
    })
}

/// Pose (in STAGE space) of a pose action for its subaction hand.
pub fn action_pose(rt: &Runtime, s: &SessionData, sim: &SimState, action: u64, subaction: u64) -> Option<Pose> {
    if !s.active_sets.iter().any(|set| rt.actions.get(&action).is_some_and(|a| a.set == *set)) {
        return None;
    }
    let hand = if subaction == 0 { None } else { Some(hand_of(rt, subaction)?) };
    let (h, src) = sources(rt, s, action, hand).into_iter().find(|(_, src)| src.is_pose())?;
    Some(match src {
        Source::GripPose => sim.grip_pose(h),
        Source::PinchPose => crate::handtrack::pinch_and_poke(sim, h).0,
        Source::PokePose => crate::handtrack::pinch_and_poke(sim, h).1,
        _ => sim.aim_pose(h),
    })
}

pub unsafe extern "system" fn create_action_space(
    session: xr::Session,
    info: *const xr::ActionSpaceCreateInfo,
    out: *mut xr::Space,
) -> xr::Result {
    crate::traced!("xrCreateActionSpace", [session, info, out], || {
        let mut rt = rt();
        let h = session.into_raw();
        if !rt.sessions.contains_key(&h) {
            return xr::Result::ERROR_HANDLE_INVALID;
        }
        let info = &*info;
        let action = info.action.into_raw();
        match rt.actions.get(&action) {
            None => return xr::Result::ERROR_HANDLE_INVALID,
            Some(a) if a.ty != xr::ActionType::POSE_INPUT => return xr::Result::ERROR_ACTION_TYPE_MISMATCH,
            _ => {}
        }
        let id = rt.new_handle();
        rt.spaces.insert(
            id,
            SpaceData {
                session: h,
                kind: SpaceKind::Action { action, subaction: info.subaction_path.into_raw() },
                offset: crate::api::from_xr(&info.pose_in_action_space),
            },
        );
        *out = xr::Space::from_raw(id);
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn enumerate_bound_sources(
    session: xr::Session,
    info: *const xr::BoundSourcesForActionEnumerateInfo,
    capacity: u32,
    count: *mut u32,
    out: *mut xr::Path,
) -> xr::Result {
    crate::traced!("xrEnumerateBoundSourcesForAction", [session, info, capacity, count, out], || {
        let rt = rt();
        let Some(s) = rt.sessions.get(&session.into_raw()) else {
            return xr::Result::ERROR_HANDLE_INVALID;
        };
        let action = (*info).action.into_raw();
        let paths: Vec<xr::Path> = rt
            .instances
            .get(&s.instance)
            .and_then(|i| i.suggested.get(&s.profile))
            .map(|l| l.iter().filter(|(a, _)| *a == action).map(|(_, b)| xr::Path::from_raw(*b)).collect())
            .unwrap_or_default();
        fill(capacity, count, out, &paths)
    })
}

pub unsafe extern "system" fn input_source_name(
    session: xr::Session,
    info: *const xr::InputSourceLocalizedNameGetInfo,
    capacity: u32,
    count: *mut u32,
    buf: *mut c_char,
) -> xr::Result {
    crate::traced!("xrGetInputSourceLocalizedName", [session, info, capacity, count, buf], || {
        let rt = rt();
        if !rt.sessions.contains_key(&session.into_raw()) {
            return xr::Result::ERROR_HANDLE_INVALID;
        }
        let s = rt.path_str((*info).source_path.into_raw()).unwrap_or("").to_string();
        let mut bytes: Vec<c_char> = s.bytes().map(|b| b as c_char).collect();
        bytes.push(0);
        fill(capacity, count, buf, &bytes)
    })
}

/// The hands a haptic action reaches: those its bindings in the current
/// profile send to (`output/haptic`), limited to the subaction path if given.
unsafe fn haptic_hands(rt: &Runtime, session: u64, info: &xr::HapticActionInfo) -> Result<Vec<usize>, xr::Result> {
    let Some(s) = rt.sessions.get(&session) else {
        return Err(xr::Result::ERROR_HANDLE_INVALID);
    };
    let action = info.action.into_raw();
    let Some(a) = rt.actions.get(&action) else {
        return Err(xr::Result::ERROR_HANDLE_INVALID);
    };
    if a.ty != xr::ActionType::VIBRATION_OUTPUT {
        return Err(xr::Result::ERROR_ACTION_TYPE_MISMATCH);
    }
    if !s.attached_sets.contains(&a.set) {
        return Err(xr::Result::ERROR_ACTIONSET_NOT_ATTACHED);
    }
    let sub = info.subaction_path.into_raw();
    let hand = if sub == 0 {
        None
    } else {
        if !a.subactions.contains(&sub) {
            return Err(xr::Result::ERROR_PATH_UNSUPPORTED);
        }
        Some(hand_of(rt, sub).ok_or(xr::Result::ERROR_PATH_UNSUPPORTED)?)
    };
    let mut hands: Vec<usize> =
        sources(rt, s, action, hand).into_iter().filter(|(_, src)| *src == Source::Haptic).map(|(h, _)| h).collect();
    hands.sort();
    hands.dedup();
    Ok(hands)
}

pub unsafe extern "system" fn apply_haptic(
    session: xr::Session,
    info: *const xr::HapticActionInfo,
    haptic: *const xr::HapticBaseHeader,
) -> xr::Result {
    crate::traced!("xrApplyHapticFeedback", [session, info, haptic], || {
        let rt = rt();
        let h = session.into_raw();
        let hands = match haptic_hands(&rt, h, &*info) {
            Ok(hands) => hands,
            Err(e) => return e,
        };
        if (*haptic).ty != xr::StructureType::HAPTIC_VIBRATION {
            return xr::Result::ERROR_VALIDATION_FAILURE;
        }
        let s = &rt.sessions[&h];
        if s.state != xr::SessionState::FOCUSED {
            return xr::Result::SESSION_NOT_FOCUSED;
        }
        let x = &*(haptic as *const xr::HapticVibration);
        let v = Vibration::from_xr(x.duration.as_nanos(), x.frequency, x.amplitude);
        let now = crate::time::now_ns();
        let mut state = s.gfx.shared.haptics.lock().unwrap();
        for &hand in &hands {
            state[hand].apply(v, now);
            xlog!("haptic: {} hand {}", if hand == LEFT { "left" } else { "right" }, v.describe());
        }
        xr::Result::SUCCESS
    })
}

pub unsafe extern "system" fn stop_haptic(session: xr::Session, info: *const xr::HapticActionInfo) -> xr::Result {
    crate::traced!("xrStopHapticFeedback", [session, info], || {
        let rt = rt();
        let h = session.into_raw();
        let hands = match haptic_hands(&rt, h, &*info) {
            Ok(hands) => hands,
            Err(e) => return e,
        };
        let s = &rt.sessions[&h];
        if s.state != xr::SessionState::FOCUSED {
            return xr::Result::SESSION_NOT_FOCUSED;
        }
        let now = crate::time::now_ns();
        let mut state = s.gfx.shared.haptics.lock().unwrap();
        for &hand in &hands {
            if state[hand].playing.is_some() {
                xlog!("haptic: {} hand stopped", if hand == LEFT { "left" } else { "right" });
            }
            state[hand].stop(now);
        }
        xr::Result::SUCCESS
    })
}
