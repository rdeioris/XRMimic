//! The runtime's headset in another process: `xrmimic-server` on this
//! machine (XRMIMIC_SERVER=local; its pipe: XRMIMIC_SERVER_PIPE).
//!
//! Two connections per app process: one for requests and their replies (one
//! at a time), one for the server's pushes (the user, many times a second;
//! the device when it changes), read by a thread into a mirror. Poses come
//! from the mirror, so the many space and view calls never wait for the
//! server; the frame clock does (`next_wake` asks the server, which may hold
//! frames back). After each frame the shared textures' handle values go to
//! the server, which opens them itself.

use crate::headset::{FrameGate, FrameTarget, Headset, HeadsetControl, HeadsetOptions};
use crate::pipe::{connect, recv, send, Hello, Push, Reply, Request, Role, UserState, PROTOCOL};
use crate::shared::Image;
use crate::xlog;
use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::Duration;
use xrmimic_core::device::DeviceProfile;
use xrmimic_core::haptics::{HapticState, Vibration};
use xrmimic_core::math::Pose;
use xrmimic_core::sim::{ControllerInput, SimState};

/// What the server sent last for a session.
type SessionMirror = UserState;

#[derive(Default)]
struct Mirror {
    device: Option<DeviceProfile>,
    sessions: HashMap<u64, SessionMirror>,
}

impl Mirror {
    /// Keep the newer of what is held and `user`.
    fn update(&mut self, session: u64, user: UserState) {
        if self.sessions.get(&session).is_none_or(|m| m.seq < user.seq) {
            self.sessions.insert(session, user);
        }
    }
}

pub struct Connection {
    requests: Mutex<std::fs::File>,
    mirror: Arc<(Mutex<Mirror>, Condvar)>,
}

static CONNECTION: OnceLock<Option<Arc<Connection>>> = OnceLock::new();

/// Set in xrmimic-server itself: it is the headset, whatever its environment says.
pub static IS_SERVER: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// XRMIMIC_SERVER asks for the server (and this is not the server).
pub fn wanted() -> bool {
    !IS_SERVER.load(Ordering::SeqCst) && std::env::var_os("XRMIMIC_SERVER").is_some()
}

/// The connection to the server, if XRMIMIC_SERVER asks for one (made once
/// per process; None if it is not asked for or cannot be made).
pub fn connection() -> Option<Arc<Connection>> {
    CONNECTION
        .get_or_init(|| {
            if IS_SERVER.load(Ordering::SeqCst) {
                return None;
            }
            let want = std::env::var("XRMIMIC_SERVER").ok()?;
            if want != "local" {
                xlog!("XRMIMIC_SERVER={want:?}: only \"local\" is supported");
                return None;
            }
            match Connection::open() {
                Ok(c) => Some(Arc::new(c)),
                Err(e) => {
                    xlog!("cannot reach xrmimic-server on {}: {e}", crate::pipe::pipe_path());
                    None
                }
            }
        })
        .clone()
}

impl Connection {
    fn open() -> std::io::Result<Connection> {
        let client = (std::process::id() as u64) << 32 | (crate::time::now_ns() as u64 & 0xffff_ffff);
        let mut requests = connect(Duration::from_secs(2))?;
        send(&mut requests, &Hello { protocol: PROTOCOL, pid: std::process::id(), role: Role::Requests, client })?;
        let mut pushes = connect(Duration::from_secs(2))?;
        send(&mut pushes, &Hello { protocol: PROTOCOL, pid: std::process::id(), role: Role::Pushes, client })?;
        let mirror: Arc<(Mutex<Mirror>, Condvar)> = Arc::default();
        let m = mirror.clone();
        std::thread::Builder::new().name("xrmimic-server-pushes".into()).spawn(move || loop {
            let push: Push = match recv(&mut pushes) {
                Ok(p) => p,
                Err(e) => {
                    xlog!("xrmimic-server connection lost: {e}");
                    return;
                }
            };
            let mut g = m.0.lock().unwrap();
            match push {
                Push::User { session, user } => g.update(session, user),
                Push::Device { device } => g.device = Some(device),
            }
            m.1.notify_all();
        })?;
        let c = Connection { requests: Mutex::new(requests), mirror };
        match c.call(&Request::Device)? {
            Reply::Device { device } => c.mirror.0.lock().unwrap().device = Some(device),
            other => return Err(std::io::Error::other(format!("unexpected reply {other:?}"))),
        }
        xlog!("headset: xrmimic-server on {}", crate::pipe::pipe_path());
        Ok(c)
    }

    fn call(&self, req: &Request) -> std::io::Result<Reply> {
        let mut conn = self.requests.lock().unwrap();
        send(&mut *conn, req)?;
        match recv(&mut *conn)? {
            Reply::Error { message } => Err(std::io::Error::other(message)),
            r => Ok(r),
        }
    }

    /// The server's device.
    pub fn device(&self) -> Option<DeviceProfile> {
        self.mirror.0.lock().unwrap().device.clone()
    }

    /// A session's mirror, waiting up to a second for the first push.
    fn session(&self, id: u64) -> Option<SessionMirror> {
        let (lock, cond) = &*self.mirror;
        let g = lock.lock().unwrap();
        let (g, _) = cond.wait_timeout_while(g, Duration::from_secs(1), |m| !m.sessions.contains_key(&id)).unwrap();
        g.sessions.get(&id).cloned()
    }
}

/// A session's headset on the server.
pub struct RemoteHeadset {
    conn: Arc<Connection>,
    session: u64,
    target: FrameTarget,
    period_ns: i64,
    stepped: bool,
    gate: Arc<FrameGate>,
    ended: bool,
}

impl RemoteHeadset {
    pub fn start(conn: Arc<Connection>, target: FrameTarget, luid: (u32, i32), options: HeadsetOptions) -> std::io::Result<RemoteHeadset> {
        target.depth_flip.store(options.depth_flip, Ordering::Relaxed);
        let req = Request::StartSession {
            app_name: options.app_name,
            hands: options.hands,
            depth_flip: options.depth_flip,
            depth_flip_auto: options.depth_flip_auto,
            luid,
            test_env: std::env::vars().filter(|(k, _)| k.starts_with("XRMIMIC_TEST_")).collect(),
        };
        match conn.call(&req)? {
            Reply::Started { session, period_ns, stepped } => Ok(RemoteHeadset {
                conn,
                session,
                target,
                period_ns,
                stepped,
                gate: Arc::new(FrameGate::default()),
                ended: false,
            }),
            other => Err(std::io::Error::other(format!("unexpected reply {other:?}"))),
        }
    }

    fn mirror(&self) -> Option<SessionMirror> {
        self.conn.session(self.session)
    }

    fn tell(&self, req: Request) {
        if let Err(e) = self.conn.call(&req) {
            xlog!("xrmimic-server: {e}");
        }
    }
}

impl Headset for RemoteHeadset {
    fn user(&self) -> SimState {
        self.mirror().map(|m| m.user).unwrap_or_else(|| SimState::new(&crate::headset::device()))
    }

    fn eye_poses(&self) -> [Pose; 2] {
        let mut user = self.user();
        user.ipd = crate::headset::device().ipd;
        [user.eye_pose(0), user.eye_pose(1)]
    }

    fn hands(&self) -> bool {
        self.mirror().is_some_and(|m| m.hands)
    }

    fn report_profile(&self, profile: &str, bound: Vec<(String, String)>) {
        self.tell(Request::Profile { session: self.session, profile: profile.to_string(), bound });
    }

    fn haptic(&self, hand: usize, vibration: Option<Vibration>, now: i64) -> bool {
        match self.conn.call(&Request::Haptic { session: self.session, hand, vibration, now }) {
            Ok(Reply::Haptic { was_playing }) => was_playing,
            _ => false,
        }
    }

    fn frame_gate(&self) -> Arc<FrameGate> {
        // The server holds frames back (in `next_wake`); nothing waits here.
        self.gate.clone()
    }

    fn next_wake(&mut self, last_wake: i64, now: i64, on_grid: bool) -> i64 {
        match self.conn.call(&Request::WaitFrame { session: self.session, last_wake, now, on_grid }) {
            Ok(Reply::Wake { wake, user }) => {
                let (lock, cond) = &*self.conn.mirror;
                lock.lock().unwrap().update(self.session, user);
                cond.notify_all();
                wake
            }
            _ => (last_wake + self.period_ns).max(now),
        }
    }

    fn period_ns(&self) -> i64 {
        self.period_ns
    }

    fn stepped(&self) -> bool {
        self.stepped
    }

    fn close_requested(&self) -> bool {
        self.mirror().is_some_and(|m| m.close_requested)
    }

    fn request_close(&self) {
        self.tell(Request::RequestClose { session: self.session });
    }

    fn control(&self) -> Arc<dyn HeadsetControl> {
        Arc::new(NoControl(self.user()))
    }

    fn frame_submitted(&self, frame: u64, shown: u64) {
        // The window's depth check may have turned the depth over.
        if let Some(m) = self.mirror() {
            self.target.depth_flip.store(m.depth_flip, Ordering::Relaxed);
        }
        let slot = self.target.slot.lock().unwrap().clone();
        self.tell(Request::Frame { session: self.session, slot, frame, shown });
    }

    fn report_state(&self, state: &str, running: bool) {
        self.tell(Request::State { session: self.session, state: state.to_string(), running });
    }

    fn shutdown(&mut self) -> Option<Box<dyn FnOnce() + Send>> {
        if self.ended {
            return None;
        }
        self.ended = true;
        let (conn, session) = (self.conn.clone(), self.session);
        Some(Box::new(move || {
            let _ = conn.call(&Request::EndSession { session });
        }))
    }
}

/// The control API is the server's when the headset runs there.
struct NoControl(SimState);

impl HeadsetControl for NoControl {
    fn user(&self) -> SimState {
        self.0.clone()
    }
    fn held_inputs(&self) -> [ControllerInput; 2] {
        [ControllerInput::default(); 2]
    }
    fn set_head(&self, _: Option<[f32; 3]>, _: Option<f32>, _: Option<f32>) {}
    fn held_by_control(&self, _: usize) -> ControllerInput {
        ControllerInput::default()
    }
    fn hold_input(&self, _: usize, _: ControllerInput) {}
    fn place_controller(&self, _: usize, _: Option<Pose>) {}
    fn set_hands(&self, _: bool) {}
    fn hands(&self) -> bool {
        false
    }
    fn haptics(&self, _: i64) -> [HapticState; 2] {
        [HapticState::default(); 2]
    }
    fn set_views(&self, _: [Option<bool>; 3]) {}
    fn capture_eyes(&self, _: Duration) -> Result<Image, String> {
        Err("the headset runs in xrmimic-server: use its control API".into())
    }
    fn capture_window(&self, _: Duration) -> Result<Image, String> {
        Err("the headset runs in xrmimic-server: use its control API".into())
    }
}
