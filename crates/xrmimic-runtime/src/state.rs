//! The runtime's objects, behind one global lock.

use crate::gfx::{SessionGfx, SwapchainData};
use crate::headset::Window;
use openxr_sys as xr;
use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, LazyLock, Mutex, MutexGuard};
use xrmimic_core::math::Pose;
use xrmimic_core::sim::{ControllerInput, SimState};
use xrmimic_core::timing::FrameClock;

pub enum Event {
    SessionState { session: u64, state: xr::SessionState, time: i64 },
    ProfileChanged { session: u64 },
    /// The play area changed: STAGE's bounds are new (its origin stays).
    StageChanged { session: u64, time: i64 },
}

pub struct InstanceData {
    pub events: VecDeque<Event>,
    pub app_name: String,
    /// XrApplicationInfo::engineName ("Chromium" for Chrome and its kin).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub engine: String,
    pub extensions: Vec<String>,
    /// Suggested bindings per interaction profile path: (action, binding path).
    pub suggested: HashMap<u64, Vec<(u64, u64)>>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SpaceKind {
    Reference(xr::ReferenceSpaceType),
    Action { action: u64, subaction: u64 },
}

pub struct SpaceData {
    pub session: u64,
    pub kind: SpaceKind,
    pub offset: Pose,
}

#[allow(dead_code)]
pub struct ActionSetData {
    pub instance: u64,
    pub name: String,
    pub attached: bool,
}

pub struct ActionData {
    pub set: u64,
    pub name: String,
    pub ty: xr::ActionType,
    pub subactions: Vec<u64>,
}

pub struct SessionData {
    pub instance: u64,
    pub state: xr::SessionState,
    pub running: bool,
    pub exit_requested: bool,
    pub gfx: SessionGfx,
    pub sim: Arc<Mutex<SimState>>,
    pub window: Option<Window>,
    pub clock: FrameClock,
    pub last_wake: i64,
    pub frame_begun: bool,
    pub frames_waited: u64,
    pub stats: FrameStats,
    pub frames_ended: u64,
    /// Frames that carried a projection layer (the app's first ones may not).
    pub frames_shown: u64,
    pub attached_sets: Vec<u64>,
    /// Current interaction profile path (0 = none).
    pub profile: u64,
    /// Whether `profile` was chosen for tracked hands.
    pub hands: bool,
    pub snapshot: [ControllerInput; 2],
    pub previous: [ControllerInput; 2],
    pub sync_time: i64,
    pub active_sets: Vec<u64>,
    pub change_times: HashMap<(u64, u64), i64>,
    /// The play area the app was last told about (STAGE's bounds).
    pub stage_bounds: [f32; 2],
}

/// Where frame time goes, summed over a window of frames (ns).
#[derive(Default)]
pub struct FrameStats {
    pub wait_called: i64,
    pub wait_returned: i64,
    pub begun: i64,
    pub ended: i64,
    pub blocked: i64,
    pub to_begin: i64,
    pub to_end: i64,
    pub to_next_wait: i64,
    pub skipped: i64,
    pub n: i64,
}

pub struct Runtime {
    next_handle: u64,
    pub instances: HashMap<u64, InstanceData>,
    pub sessions: HashMap<u64, SessionData>,
    pub spaces: HashMap<u64, SpaceData>,
    pub swapchains: HashMap<u64, SwapchainData>,
    pub action_sets: HashMap<u64, ActionSetData>,
    pub actions: HashMap<u64, ActionData>,
    paths: Vec<String>,
    path_ids: HashMap<String, u64>,
}

static RUNTIME: LazyLock<Mutex<Runtime>> = LazyLock::new(|| {
    Mutex::new(Runtime {
        next_handle: 1,
        instances: HashMap::new(),
        sessions: HashMap::new(),
        spaces: HashMap::new(),
        swapchains: HashMap::new(),
        action_sets: HashMap::new(),
        actions: HashMap::new(),
        paths: Vec::new(),
        path_ids: HashMap::new(),
    })
});

/// The runtime if nobody holds it right now (for the call trace, which must never block).
pub fn try_rt() -> Option<MutexGuard<'static, Runtime>> {
    RUNTIME.try_lock().ok()
}

pub fn rt() -> MutexGuard<'static, Runtime> {
    RUNTIME.lock().unwrap_or_else(|e| e.into_inner())
}

impl Runtime {
    pub fn new_handle(&mut self) -> u64 {
        let h = self.next_handle;
        self.next_handle += 1;
        h
    }

    pub fn intern(&mut self, s: &str) -> u64 {
        if let Some(&id) = self.path_ids.get(s) {
            return id;
        }
        self.paths.push(s.to_string());
        let id = self.paths.len() as u64;
        self.path_ids.insert(s.to_string(), id);
        id
    }

    pub fn path_str(&self, id: u64) -> Option<&str> {
        if id == 0 {
            return None;
        }
        self.paths.get(id as usize - 1).map(|s| s.as_str())
    }

    pub fn path_id(&self, s: &str) -> u64 {
        self.path_ids.get(s).copied().unwrap_or(0)
    }

    pub fn push_event(&mut self, instance: u64, e: Event) {
        if let Some(i) = self.instances.get_mut(&instance) {
            i.events.push_back(e);
        }
    }

    /// Move a session to `state` and queue the event.
    pub fn set_state(&mut self, session: u64, state: xr::SessionState) {
        let Some(s) = self.sessions.get_mut(&session) else { return };
        if s.state == state {
            return;
        }
        s.state = state;
        let instance = s.instance;
        crate::xlog!("session {session} -> {state:?}");
        let time = crate::time::now_ns();
        self.push_event(instance, Event::SessionState { session, state, time });
    }

    /// Everything a destroyed instance owned goes with it.
    pub fn drop_instance(&mut self, instance: u64) {
        let sessions: Vec<u64> =
            self.sessions.iter().filter(|(_, s)| s.instance == instance).map(|(k, _)| *k).collect();
        for s in sessions {
            self.drop_session(s);
        }
        let sets: Vec<u64> =
            self.action_sets.iter().filter(|(_, a)| a.instance == instance).map(|(k, _)| *k).collect();
        for set in sets {
            self.action_sets.remove(&set);
            self.actions.retain(|_, a| a.set != set);
        }
        self.instances.remove(&instance);
    }

    pub fn drop_session(&mut self, session: u64) {
        // Events the app never polled must not reach its next session: Chrome
        // treats a stale EXITING as the end of whatever session it has now.
        for inst in self.instances.values_mut() {
            inst.events.retain(|e| match e {
                Event::SessionState { session: s, .. }
                | Event::ProfileChanged { session: s }
                | Event::StageChanged { session: s, .. } => *s != session,
            });
        }
        self.spaces.retain(|_, s| s.session != session);
        self.swapchains.retain(|_, s| s.session != session);
        if let Some(mut s) = self.sessions.remove(&session) {
            if let Some(w) = s.window.take() {
                w.stop();
            }
        }
    }
}
