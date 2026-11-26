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

use crate::shared::Shared;
use crate::xlog;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use xrmimic_core::device::DeviceProfile;
use xrmimic_core::haptics::Vibration;
use xrmimic_core::math::Pose;
use xrmimic_core::sim::SimState;
use xrmimic_core::timing::FrameClock;

/// The device the runtime pretends to be (the Device tab's settings).
pub fn device() -> DeviceProfile {
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
    /// When the next frame should start (ns), after the last one at
    /// `last_wake`; `on_grid` keeps to the display's fixed grid.
    fn next_wake(&mut self, last_wake: i64, now: i64, on_grid: bool) -> i64;
    /// The display's frame period (ns).
    fn period_ns(&self) -> i64;
    /// The user closed the headset: the session should stop.
    fn close_requested(&self) -> bool;
    /// Behave as if the user closed the headset (tests).
    fn request_close(&self);
    /// Stop the headset; returns the rest of the work (joining its threads)
    /// to do outside the runtime lock. Later calls return None.
    fn shutdown(&mut self) -> Option<Box<dyn FnOnce() + Send>>;
}

/// The headset in the app's process: the simulated user's thread and the
/// window (a headless thread on Linux), sharing [`Shared`] with the session's
/// graphics.
pub struct LocalHeadset {
    shared: Arc<Shared>,
    sim: Arc<Mutex<SimState>>,
    window: Option<crate::ui::Window>,
    clock: FrameClock,
}

impl LocalHeadset {
    /// The frame target for the session's graphics.
    pub fn frame_target() -> FrameTarget {
        Arc::new(Shared::default())
    }

    /// Start the headset: the simulated user at the device's eye height, its
    /// thread, the window on the graphics' adapter, and the frame clock at
    /// the device's refresh rate (XRMIMIC_REFRESH overrides it).
    pub fn start(target: FrameTarget, gfx: &crate::gfx::SessionGfx, options: HeadsetOptions, now: i64) -> LocalHeadset {
        let shared = target;
        shared.hands.store(options.hands, Ordering::SeqCst);
        shared.depth_flip.store(options.depth_flip, Ordering::Relaxed);
        shared.depth_flip_auto.store(options.depth_flip_auto, Ordering::Relaxed);
        *shared.app_name.lock().unwrap() = options.app_name;
        let device = device();
        let sim = Arc::new(Mutex::new(SimState::new(&device)));
        let window = crate::ui::Window::spawn(gfx, sim.clone(), shared.clone());
        let hz = std::env::var("XRMIMIC_REFRESH").ok().and_then(|s| s.parse().ok()).unwrap_or(device.default_refresh);
        LocalHeadset { shared, sim, window: Some(window), clock: FrameClock::new(hz, now) }
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

    fn next_wake(&mut self, last_wake: i64, now: i64, on_grid: bool) -> i64 {
        if on_grid {
            self.clock.wake_after(last_wake, now)
        } else {
            self.clock.wake_free(last_wake, now)
        }
    }

    fn period_ns(&self) -> i64 {
        self.clock.period_ns
    }

    fn close_requested(&self) -> bool {
        self.shared.close_requested.load(Ordering::SeqCst)
    }

    fn request_close(&self) {
        self.shared.close_requested.store(true, Ordering::SeqCst);
    }

    fn shutdown(&mut self) -> Option<Box<dyn FnOnce() + Send>> {
        let window = self.window.take()?;
        xlog!("headset stopping");
        Some(Box::new(move || window.stop()))
    }
}
