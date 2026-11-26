//! `xrmimic-server`: the simulated headset in a process of its own, for apps
//! whose runtime is started with XRMIMIC_SERVER=local (crate::remote).
//!
//! Each app session gets a [`LocalHeadset`] here (the simulated user and its
//! thread, the window, the frame clock), so the window, the Device and Input
//! tabs and the control API (XRMIMIC_CONTROL, set for the server) work as
//! they do in-process. The app's runtime keeps its swapchains and copies each
//! frame into shared textures as before; it sends their handle values, and
//! the server takes the handles out of the app's process (`DuplicateHandle`)
//! and gives them to the window. Frames are never copied.

use crate::control::{ControlledSession, SessionInfo};
use crate::headset::{FrameGate, FrameTarget, Headset, HeadsetControl, HeadsetOptions, LocalHeadset};
use crate::pipe::server::{accept, close, ClientProcess};
use crate::pipe::{recv, send, Hello, Push, Reply, Request, Role, UserState, PROTOCOL};
use crate::xlog;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// A session the server runs a headset for.
struct ServerSession {
    id: u64,
    headset: Mutex<LocalHeadset>,
    target: FrameTarget,
    gate: Arc<FrameGate>,
    control: Arc<dyn HeadsetControl>,
    clock: (bool, i64),
    info: Mutex<SessionInfo>,
    ended: AtomicBool,
    /// Handles taken from the app: (ring generation, its value) -> ours, kept
    /// while the session lives. The generation is part of the key because the
    /// app closes a ring's handles when it remakes the ring, and Windows hands
    /// the same values out again.
    handles: Mutex<HashMap<(u64, usize), usize>>,
}

impl ControlledSession for ServerSession {
    fn info(&self) -> Option<SessionInfo> {
        if self.ended.load(Ordering::SeqCst) {
            return None;
        }
        let i = self.info.lock().unwrap();
        Some(SessionInfo {
            handle: i.handle,
            state: i.state.clone(),
            running: i.running,
            frame: i.frame,
            shown: i.shown,
            app: i.app.clone(),
            profile: i.profile.clone(),
            bound: i.bound.clone(),
        })
    }

    fn control(&self) -> Option<Arc<dyn HeadsetControl>> {
        (!self.ended.load(Ordering::SeqCst)).then(|| self.control.clone())
    }

    fn gate(&self) -> Option<Arc<FrameGate>> {
        (!self.ended.load(Ordering::SeqCst)).then(|| self.gate.clone())
    }

    fn clock(&self) -> (bool, i64) {
        self.clock
    }
}

/// An app process: its two connections share these sessions.
struct Client {
    process: ClientProcess,
    sessions: Mutex<HashMap<u64, Arc<ServerSession>>>,
    gone: AtomicBool,
}

static NEXT_SESSION: AtomicU64 = AtomicU64::new(1);
static NEXT_SEQ: AtomicU64 = AtomicU64::new(1);

/// The session's user and headset state now (numbered while the headset is held).
fn user_state(s: &ServerSession) -> UserState {
    let h = s.headset.lock().unwrap();
    UserState {
        seq: NEXT_SEQ.fetch_add(1, Ordering::SeqCst),
        user: h.user(),
        hands: h.hands(),
        close_requested: h.close_requested(),
        depth_flip: s.target.depth_flip.load(Ordering::Relaxed),
    }
}

/// Serve until the process is stopped. Returns a process exit code.
pub fn run() -> i32 {
    crate::remote::IS_SERVER.store(true, Ordering::SeqCst);
    let path = crate::pipe::pipe_path();
    xlog!("xrmimic-server on {path}");
    crate::control::start_once();
    let clients: Arc<Mutex<HashMap<u64, Arc<Client>>>> = Arc::new(Mutex::new(HashMap::new()));
    loop {
        let (conn, pid) = match accept(&path) {
            Ok(c) => c,
            Err(e) => {
                xlog!("server: cannot open {path}: {e}");
                return 1;
            }
        };
        let clients = clients.clone();
        let _ = std::thread::Builder::new().name("xrmimic-server-client".into()).spawn(move || {
            if let Err(e) = serve(conn, pid, &clients) {
                xlog!("server: client {pid}: {e}");
            }
        });
    }
}

fn serve(mut conn: std::fs::File, pid: u32, clients: &Mutex<HashMap<u64, Arc<Client>>>) -> std::io::Result<()> {
    let hello: Hello = recv(&mut conn)?;
    if hello.protocol != PROTOCOL {
        return Err(std::io::Error::other(format!("protocol {} (this server speaks {PROTOCOL})", hello.protocol)));
    }
    let client = {
        let mut c = clients.lock().unwrap();
        match c.get(&hello.client) {
            Some(c) => c.clone(),
            None => {
                let new = Arc::new(Client {
                    process: ClientProcess::open(pid)?,
                    sessions: Mutex::new(HashMap::new()),
                    gone: AtomicBool::new(false),
                });
                c.insert(hello.client, new.clone());
                new
            }
        }
    };
    match hello.role {
        Role::Requests => {
            xlog!("server: app process {pid} connected");
            let r = requests(&mut conn, &client);
            // The app is gone (or broke the protocol): end its sessions.
            client.gone.store(true, Ordering::SeqCst);
            let sessions: Vec<_> = client.sessions.lock().unwrap().drain().map(|(_, s)| s).collect();
            for s in sessions {
                end_session(&s);
            }
            clients.lock().unwrap().remove(&hello.client);
            xlog!("server: app process {pid} disconnected");
            r
        }
        Role::Pushes => pushes(&mut conn, &client),
    }
}

fn requests(conn: &mut std::fs::File, client: &Client) -> std::io::Result<()> {
    loop {
        let req: Request = match recv(conn) {
            Ok(r) => r,
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof || e.raw_os_error() == Some(109) => return Ok(()),
            Err(e) => return Err(e),
        };
        let reply = handle(req, client);
        send(conn, &reply)?;
    }
}

fn session(client: &Client, id: u64) -> Option<Arc<ServerSession>> {
    client.sessions.lock().unwrap().get(&id).cloned()
}

fn handle(req: Request, client: &Client) -> Reply {
    match req {
        Request::Device => Reply::Device { device: crate::headset::device() },
        Request::StartSession { app_name, hands, depth_flip, depth_flip_auto, luid, test_env } => {
            // The window reads its test hooks from this process's environment.
            for (k, v) in test_env {
                xlog!("server: test hook {k}={v}");
                std::env::set_var(k, v);
            }
            let id = NEXT_SESSION.fetch_add(1, Ordering::SeqCst);
            let target = LocalHeadset::frame_target();
            let options = HeadsetOptions { app_name: app_name.clone(), hands, depth_flip, depth_flip_auto };
            let headset = LocalHeadset::start(target.clone(), luid, options, crate::time::now_ns());
            let clock = (headset.stepped(), headset.period_ns());
            let s = Arc::new(ServerSession {
                id,
                gate: headset.frame_gate(),
                control: headset.control(),
                headset: Mutex::new(headset),
                target,
                clock,
                info: Mutex::new(SessionInfo {
                    handle: id,
                    state: "IDLE".into(),
                    running: false,
                    frame: 0,
                    shown: 0,
                    app: app_name.clone(),
                    profile: None,
                    bound: Vec::new(),
                }),
                ended: AtomicBool::new(false),
                handles: Mutex::new(HashMap::new()),
            });
            client.sessions.lock().unwrap().insert(id, s.clone());
            crate::control::register(id, s);
            xlog!("server: session {id} for {app_name:?}");
            Reply::Started { session: id, period_ns: clock.1, stepped: clock.0 }
        }
        Request::WaitFrame { session: id, last_wake, now, on_grid } => {
            let Some(s) = session(client, id) else { return no_session(id) };
            s.gate.pass();
            let wake = s.headset.lock().unwrap().next_wake(last_wake, now, on_grid);
            Reply::Wake { wake, user: user_state(&s) }
        }
        Request::Frame { session: id, mut slot, frame, shown } => {
            let Some(s) = session(client, id) else { return no_session(id) };
            if let Err(e) = take_handles(&s, client, &mut slot) {
                return Reply::Error { message: format!("frame handles: {e}") };
            }
            *s.target.slot.lock().unwrap() = slot;
            let mut i = s.info.lock().unwrap();
            i.frame = frame;
            i.shown = shown;
            Reply::Ok
        }
        Request::State { session: id, state, running } => {
            let Some(s) = session(client, id) else { return no_session(id) };
            let mut i = s.info.lock().unwrap();
            i.state = state;
            i.running = running;
            Reply::Ok
        }
        Request::Profile { session: id, profile, bound } => {
            let Some(s) = session(client, id) else { return no_session(id) };
            s.headset.lock().unwrap().report_profile(&profile, bound.clone());
            let mut i = s.info.lock().unwrap();
            i.profile = (!profile.is_empty()).then_some(profile);
            i.bound = bound;
            Reply::Ok
        }
        Request::Haptic { session: id, hand, vibration, now } => {
            let Some(s) = session(client, id) else { return no_session(id) };
            let was_playing = s.headset.lock().unwrap().haptic(hand, vibration, now);
            Reply::Haptic { was_playing }
        }
        Request::RequestClose { session: id } => {
            let Some(s) = session(client, id) else { return no_session(id) };
            s.headset.lock().unwrap().request_close();
            Reply::Ok
        }
        Request::EndSession { session: id } => {
            let s = client.sessions.lock().unwrap().remove(&id);
            if let Some(s) = s {
                end_session(&s);
            }
            Reply::Ok
        }
    }
}

fn no_session(id: u64) -> Reply {
    Reply::Error { message: format!("no session {id}") }
}

fn end_session(s: &ServerSession) {
    s.ended.store(true, Ordering::SeqCst);
    crate::control::unregister(s.id);
    let stop = s.headset.lock().unwrap().shutdown();
    if let Some(stop) = stop {
        stop();
    }
    for (_, ours) in s.handles.lock().unwrap().drain() {
        close(ours);
    }
    xlog!("server: session {} ended", s.id);
}

/// Replace the app's handle values in a frame's slot with handles of this
/// process (taken once per handle and kept while the session lives).
fn take_handles(s: &ServerSession, client: &Client, slot: &mut crate::gfx::Slot) -> std::io::Result<()> {
    let mut map = s.handles.lock().unwrap();
    let mut take = |generation: u64, h: &mut usize| -> std::io::Result<()> {
        if *h == 0 {
            return Ok(());
        }
        let ours = match map.get(&(generation, *h)) {
            Some(o) => *o,
            None => {
                let o = client.process.take(*h)?;
                map.insert((generation, *h), o);
                o
            }
        };
        *h = ours;
        Ok(())
    };
    for h in slot.handles.iter_mut() {
        take(slot.generation, h)?;
    }
    take(slot.generation, &mut slot.fence)?;
    for q in slot.quads.iter_mut() {
        for h in q.handles.iter_mut() {
            take(q.generation, h)?;
        }
    }
    for d in slot.depth.iter_mut() {
        for h in d.handles.iter_mut() {
            take(d.generation, h)?;
        }
    }
    Ok(())
}

/// The user and the headset's state for each of the app's sessions, every
/// few milliseconds, and the device when it changes.
fn pushes(conn: &mut std::fs::File, client: &Client) -> std::io::Result<()> {
    let mut device = crate::headset::device();
    while !client.gone.load(Ordering::SeqCst) {
        let now = crate::headset::device();
        if now != device {
            device = now;
            send(conn, &Push::Device { device: device.clone() })?;
        }
        let sessions: Vec<_> = client.sessions.lock().unwrap().values().cloned().collect();
        for s in sessions {
            send(conn, &Push::User { session: s.id, user: user_state(&s) })?;
        }
        std::thread::sleep(Duration::from_millis(4));
    }
    Ok(())
}
