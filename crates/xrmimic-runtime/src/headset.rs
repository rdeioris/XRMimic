//! The boundary between the OpenXR side of the runtime (instances, sessions,
//! spaces, actions, swapchains: api.rs, input.rs, handtrack.rs) and the
//! "headset" it simulates: the device, the simulated user, the frame clock,
//! the window. The OpenXR side asks only through this module.
//!
//! The headset runs in the app's process ([`LocalHeadset`]: the simulated
//! user's thread, the window or the headless thread, the state they share);
//! another implementation could run it in another process or on another
//! machine behind the same interface.
//!
//! Frames are not part of [`Headset`]: the session's graphics (`gfx`) own the
//! swapchains on the app's GPU and copy each frame into textures the window
//! reads ([`FrameTarget`]), which is the in-process frame transport.
//!
//! The clock: by default frames follow real time at the device's refresh
//! rate. With XRMIMIC_CLOCK=step (the frame-stepped clock) xrWaitFrame does
//! not wait, display times advance exactly one period per frame and the
//! simulated user moves exactly one period per frame, so a run is the same
//! every time and as fast as the app renders. The control API can hold
//! frames back and hand them out one by one ([`FrameGate`]).

use crate::shared::{Image, Shared};
use crate::xlog;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;
use xrmimic_core::device::DeviceProfile;
use xrmimic_core::haptics::Vibration;
use xrmimic_core::math::Pose;
use xrmimic_core::sim::{ControllerInput, SimState};
use xrmimic_core::timing::FrameClock;

/// The device the runtime pretends to be (the Device tab's settings; the
/// server's, when the headset runs in xrmimic-server).
pub fn device() -> DeviceProfile {
    #[cfg(windows)]
    if let Some(d) = crate::remote::connection().and_then(|c| c.device()) {
        return d;
    }
    crate::devstore::current()
}

/// Where the session's graphics put each frame for the headset to show: made
/// before the graphics, handed to them, then to [`LocalHeadset::start`].
/// Opaque to the OpenXR side.
pub type FrameTarget = Arc<Shared>;

/// How a session's headset starts.
pub struct HeadsetOptions {
    /// Shown in the window's title.
    pub app_name: String,
    /// Start with tracked hands instead of controllers (XRMIMIC_HANDS=1).
    pub hands: bool,
    /// Read the app's depth upside down at first, and whether the window may
    /// turn it over from what it sees (crate::depthwatch).
    pub depth_flip: bool,
    pub depth_flip_auto: bool,
}

/// What the OpenXR side needs from the headset, for one session.
pub trait Headset: Send {
    /// The simulated user as of now: head, controllers (poses and inputs),
    /// hands, a controller put down. A copy: the user keeps moving.
    fn user(&self) -> SimState;
    /// Both eyes' poses (STAGE space) as of now, with the device's current IPD.
    fn eye_poses(&self) -> [Pose; 2];
    /// The user put the controllers down and uses tracked hands.
    fn hands(&self) -> bool;
    /// The interaction profile the session reports and the app's bindings in
    /// it ((binding path, action name)), for the window's Input tab.
    fn report_profile(&self, profile: &str, bound: Vec<(String, String)>);
    /// A vibration on a hand, or None to stop it. Returns whether one was playing.
    fn haptic(&self, hand: usize, vibration: Option<Vibration>, now: i64) -> bool;
    /// Frames may be held back (the control API); xrWaitFrame passes this
    /// gate, outside the runtime lock, before [`Headset::next_wake`].
    fn frame_gate(&self) -> Arc<FrameGate>;
    /// When the next frame should start (ns), after the last one at
    /// `last_wake`; `on_grid` keeps to the display's fixed grid. On the
    /// frame-stepped clock this also moves the simulated user one period on.
    fn next_wake(&mut self, last_wake: i64, now: i64, on_grid: bool) -> i64;
    /// The display's frame period (ns).
    fn period_ns(&self) -> i64;
    /// The frame-stepped clock is on (XRMIMIC_CLOCK=step).
    fn stepped(&self) -> bool;
    /// The user closed the headset: the session should stop.
    fn close_requested(&self) -> bool;
    /// Behave as if the user closed the headset (tests).
    fn request_close(&self);
    /// What the control API may do with this headset (crate::control).
    fn control(&self) -> Arc<dyn HeadsetControl>;
    /// The session's graphics copied a frame into the frame target (the
    /// app's `frame`-th, `shown` of them with a projection layer).
    fn frame_submitted(&self, _frame: u64, _shown: u64) {}
    /// The session's state changed (for the control API).
    fn report_state(&self, _state: &str, _running: bool) {}
    /// Stop the headset; returns the rest of the work (joining its threads)
    /// to do outside the runtime lock. Later calls return None.
    fn shutdown(&mut self) -> Option<Box<dyn FnOnce() + Send>>;
}

/// Frames held back or handed out: paused, xrWaitFrame waits until a frame
/// is allowed (`step`), so a test decides exactly when each frame happens.
#[derive(Default)]
pub struct FrameGate {
    /// (paused, frames still allowed while paused, open for good: shut down).
    state: Mutex<(bool, u64, bool)>,
    changed: Condvar,
}

impl FrameGate {
    /// Wait until a frame may start; while paused that takes one allowed frame.
    pub fn pass(&self) {
        let mut s = self.state.lock().unwrap();
        while s.0 && s.1 == 0 && !s.2 {
            s = self.changed.wait(s).unwrap();
        }
        if s.0 && s.1 > 0 {
            s.1 -= 1;
        }
        self.changed.notify_all();
    }

    pub fn set_paused(&self, paused: bool) {
        let mut s = self.state.lock().unwrap();
        s.0 = paused;
        if !paused {
            s.1 = 0;
        }
        self.changed.notify_all();
    }

    /// Allow `frames` more frames while paused.
    pub fn step(&self, frames: u64) {
        let mut s = self.state.lock().unwrap();
        s.1 += frames;
        self.changed.notify_all();
    }

    /// Wait until the allowed frames have started, up to `timeout`; true if they did.
    pub fn wait_drained(&self, timeout: Duration) -> bool {
        let s = self.state.lock().unwrap();
        let (s, r) = self.changed.wait_timeout_while(s, timeout, |s| s.1 > 0 && !s.2).unwrap();
        drop(s);
        !r.timed_out()
    }

    /// Never wait again (the session is ending).
    pub fn open(&self) {
        self.state.lock().unwrap().2 = true;
        self.changed.notify_all();
    }

    /// (paused, frames still allowed).
    pub fn status(&self) -> (bool, u64) {
        let s = self.state.lock().unwrap();
        (s.0, s.1)
    }
}

/// What the control API can read and do on a headset.
pub trait HeadsetControl: Send + Sync {
    /// The simulated user as of now.
    fn user(&self) -> SimState;
    /// What the Input tab and the control API hold on each hand.
    fn held_inputs(&self) -> [ControllerInput; 2];
    /// Move and turn the head (STAGE: position in metres, yaw and pitch in radians).
    fn set_head(&self, position: Option<[f32; 3]>, yaw: Option<f32>, pitch: Option<f32>);
    /// What the control API holds on a hand.
    fn held_by_control(&self, hand: usize) -> ControllerInput;
    /// Replace what the control API holds on a hand.
    fn hold_input(&self, hand: usize, input: ControllerInput);
    /// Put a controller down at an aim pose (STAGE), or give it back to the head (None).
    fn place_controller(&self, hand: usize, aim: Option<Pose>);
    /// Put the controllers down (tracked hands) or pick them up.
    fn set_hands(&self, on: bool);
    fn hands(&self) -> bool;
    /// Each hand's haptics: (playing amplitude now, the last vibration, how many).
    fn haptics(&self, now: i64) -> [xrmimic_core::haptics::HapticState; 2];
    /// Switch the window's views: Depth, Centre, Reproject (None: leave).
    fn set_views(&self, views: [Option<bool>; 3]);
    /// The next frame's eyes side by side, as the app drew them.
    fn capture_eyes(&self, timeout: Duration) -> Result<Image, String>;
    /// The headset window as shown (Windows).
    fn capture_window(&self, timeout: Duration) -> Result<Image, String>;
}

/// The headset in the app's process: the simulated user and its thread, the
/// window (a headless thread on Linux), sharing [`Shared`] with the session's
/// graphics, and the frame clock.
pub struct LocalHeadset {
    shared: Arc<Shared>,
    sim: Arc<Mutex<SimState>>,
    driver: Arc<crate::simthread::Driver>,
    window: Option<crate::ui::Window>,
    user_thread: Option<std::thread::JoinHandle<()>>,
    clock: FrameClock,
    stepped: bool,
    control: Arc<LocalControl>,
}

impl LocalHeadset {
    /// The frame target for the session's graphics.
    pub fn frame_target() -> FrameTarget {
        Arc::new(Shared::default())
    }

    /// Start the headset: the simulated user at the device's eye height, its
    /// thread, the window on the graphics' adapter, and the frame clock at
    /// the device's refresh rate (XRMIMIC_REFRESH overrides it;
    /// XRMIMIC_CLOCK=step steps it per frame).
    /// `luid`: the graphics adapter the app renders on (low, high part).
    pub fn start(target: FrameTarget, luid: (u32, i32), options: HeadsetOptions, now: i64) -> LocalHeadset {
        let shared = target;
        shared.hands.store(options.hands, Ordering::SeqCst);
        shared.depth_flip.store(options.depth_flip, Ordering::Relaxed);
        shared.depth_flip_auto.store(options.depth_flip_auto, Ordering::Relaxed);
        *shared.app_name.lock().unwrap() = options.app_name;
        let stepped = std::env::var("XRMIMIC_CLOCK").is_ok_and(|v| v == "step");
        shared.stepped.store(stepped, Ordering::Relaxed);
        if stepped {
            xlog!("frame-stepped clock: frames do not wait, the user moves one period per frame");
        }
        let device = device();
        let sim = Arc::new(Mutex::new(SimState::new(&device)));
        let driver = Arc::new(crate::simthread::Driver::new(&sim));
        let user_thread = crate::simthread::spawn(sim.clone(), shared.clone(), driver.clone());
        let window = crate::ui::Window::spawn(luid, sim.clone(), shared.clone());
        let hz = std::env::var("XRMIMIC_REFRESH").ok().and_then(|s| s.parse().ok()).unwrap_or(device.default_refresh);
        let control = Arc::new(LocalControl { sim: sim.clone(), shared: shared.clone() });
        LocalHeadset {
            shared,
            sim,
            driver,
            window: Some(window),
            user_thread,
            clock: FrameClock::new(hz, now),
            stepped,
            control,
        }
    }
}

impl Headset for LocalHeadset {
    fn user(&self) -> SimState {
        self.sim.lock().unwrap().clone()
    }

    fn eye_poses(&self) -> [Pose; 2] {
        // IPD follows the Device tab at once; the window draws with it too.
        let ipd = device().ipd;
        let mut sim = self.sim.lock().unwrap();
        sim.ipd = ipd;
        [sim.eye_pose(0), sim.eye_pose(1)]
    }

    fn hands(&self) -> bool {
        self.shared.hands.load(Ordering::SeqCst)
    }

    fn report_profile(&self, profile: &str, bound: Vec<(String, String)>) {
        *self.shared.profile.lock().unwrap() = profile.to_string();
        *self.shared.bound.lock().unwrap() = bound;
        self.shared.attached.store(true, Ordering::SeqCst);
    }

    fn haptic(&self, hand: usize, vibration: Option<Vibration>, now: i64) -> bool {
        let mut state = self.shared.haptics.lock().unwrap();
        let was_playing = state[hand].playing.is_some();
        match vibration {
            Some(v) => state[hand].apply(v, now),
            None => state[hand].stop(now),
        }
        was_playing
    }

    fn frame_gate(&self) -> Arc<FrameGate> {
        self.shared.gate.clone()
    }

    fn next_wake(&mut self, last_wake: i64, now: i64, on_grid: bool) -> i64 {
        if self.stepped {
            // One period of the user's time per frame, whatever the wall clock says.
            self.driver.step(&self.sim, &self.shared, self.clock.period_ns as f32 / 1e9);
            return last_wake + self.clock.period_ns;
        }
        if on_grid {
            self.clock.wake_after(last_wake, now)
        } else {
            self.clock.wake_free(last_wake, now)
        }
    }

    fn period_ns(&self) -> i64 {
        self.clock.period_ns
    }

    fn stepped(&self) -> bool {
        self.stepped
    }

    fn close_requested(&self) -> bool {
        self.shared.close_requested.load(Ordering::SeqCst)
    }

    fn request_close(&self) {
        self.shared.close_requested.store(true, Ordering::SeqCst);
    }

    fn control(&self) -> Arc<dyn HeadsetControl> {
        self.control.clone()
    }

    fn shutdown(&mut self) -> Option<Box<dyn FnOnce() + Send>> {
        let window = self.window.take()?;
        let user = self.user_thread.take();
        // A paused app must not stay stuck in xrWaitFrame.
        self.shared.gate.open();
        self.shared.stop.store(true, Ordering::SeqCst);
        xlog!("headset stopping");
        Some(Box::new(move || {
            window.stop();
            if let Some(t) = user {
                let _ = t.join();
            }
        }))
    }
}

/// The control API's view of a [`LocalHeadset`].
struct LocalControl {
    sim: Arc<Mutex<SimState>>,
    shared: Arc<Shared>,
}

/// Wait for a picture someone else makes, up to `timeout`.
fn request_picture(
    queue: &Mutex<Vec<std::sync::mpsc::Sender<Result<Image, String>>>>,
    timeout: Duration,
    what: &str,
) -> Result<Image, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    queue.lock().unwrap().push(tx);
    rx.recv_timeout(timeout).map_err(|_| format!("no {what} within {} ms", timeout.as_millis()))?
}

impl HeadsetControl for LocalControl {
    fn user(&self) -> SimState {
        self.sim.lock().unwrap().clone()
    }

    fn held_inputs(&self) -> [ControllerInput; 2] {
        let tab = *self.shared.tab_input.lock().unwrap();
        let control = *self.shared.control_input.lock().unwrap();
        [tab[0].merge(control[0]), tab[1].merge(control[1])]
    }

    fn set_head(&self, position: Option<[f32; 3]>, yaw: Option<f32>, pitch: Option<f32>) {
        let mut s = self.sim.lock().unwrap();
        if let Some([x, y, z]) = position {
            s.head_position = xrmimic_core::math::Vec3::new(x, y, z);
        }
        if let Some(y) = yaw {
            s.yaw = y;
        }
        if let Some(p) = pitch {
            let limit = 89f32.to_radians();
            s.pitch = p.clamp(-limit, limit);
        }
    }

    fn held_by_control(&self, hand: usize) -> ControllerInput {
        self.shared.control_input.lock().unwrap()[hand]
    }

    fn hold_input(&self, hand: usize, input: ControllerInput) {
        self.shared.control_input.lock().unwrap()[hand] = input;
        // Values show at once even when the user is stepped per frame.
        if !self.shared.stepped.load(Ordering::Relaxed) {
            return;
        }
        let mut s = self.sim.lock().unwrap();
        let tab = self.shared.tab_input.lock().unwrap()[hand];
        s.controllers[hand] = tab.merge(input);
    }

    fn place_controller(&self, hand: usize, aim: Option<Pose>) {
        self.sim.lock().unwrap().placed[hand] = aim;
    }

    fn set_hands(&self, on: bool) {
        self.shared.hands.store(on, Ordering::SeqCst);
    }

    fn hands(&self) -> bool {
        self.shared.hands.load(Ordering::SeqCst)
    }

    fn haptics(&self, _now: i64) -> [xrmimic_core::haptics::HapticState; 2] {
        *self.shared.haptics.lock().unwrap()
    }

    fn set_views(&self, views: [Option<bool>; 3]) {
        let mut v = self.shared.view_request.lock().unwrap();
        for (slot, want) in v.iter_mut().zip(views) {
            if want.is_some() {
                *slot = want;
            }
        }
    }

    fn capture_eyes(&self, timeout: Duration) -> Result<Image, String> {
        request_picture(&self.shared.eye_requests, timeout, "frame")
    }

    fn capture_window(&self, timeout: Duration) -> Result<Image, String> {
        if cfg!(not(windows)) {
            return Err("no headset window on this platform".into());
        }
        request_picture(&self.shared.window_requests, timeout, "window picture")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paused_gate_lets_through_only_the_frames_allowed() {
        let gate = Arc::new(FrameGate::default());
        gate.pass(); // not paused: no wait
        gate.set_paused(true);
        gate.step(2);
        let g = gate.clone();
        let t = std::thread::spawn(move || {
            g.pass();
            g.pass();
            g.pass(); // the third waits
        });
        assert!(gate.wait_drained(Duration::from_secs(5)), "two frames went through");
        std::thread::sleep(Duration::from_millis(50));
        assert!(!t.is_finished(), "the third frame waits");
        gate.step(1);
        t.join().unwrap();
        assert_eq!(gate.status(), (true, 0));
        // Resuming or shutting down lets everything through.
        let g = gate.clone();
        let t = std::thread::spawn(move || g.pass());
        gate.open();
        t.join().unwrap();
    }
}
