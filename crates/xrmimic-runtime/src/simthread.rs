//! The simulated user's own thread. It steps the head and controllers every
//! few milliseconds from the moment the session starts, so their motion never
//! depends on the headset window (its start-up, its vsync'd presents, or a
//! snapshot). The window, where there is one, only feeds it input.

use crate::shared::Shared;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use xrmimic_core::sim::{ControllerInput, InputFrame, SimState, LEFT, RIGHT};

/// Input waiting for the next step. Held state (movement axes, the keyboard
/// controller in `frame.controllers[0]`) is replaced by each update; drags and
/// wheel turns accumulate until a step takes them.
#[derive(Default)]
pub struct UserInput {
    pub frame: InputFrame,
    /// Switch which controller the keyboard drives.
    pub toggle: bool,
}

impl UserInput {
    /// Fold a new window input frame in: held state replaced, motion added.
    /// (The Windows headset window; Linux has no window to feed it.)
    #[cfg_attr(not(windows), allow(dead_code))]
    pub fn update(&mut self, f: &InputFrame, toggle: bool) {
        let acc = &mut self.frame;
        let old = *acc;
        *acc = *f;
        acc.mouse_dx += old.mouse_dx;
        acc.mouse_dy += old.mouse_dy;
        acc.pan_dx += old.pan_dx;
        acc.pan_dy += old.pan_dy;
        acc.zoom += old.zoom;
        acc.hand_dx += old.hand_dx;
        acc.hand_dy += old.hand_dy;
        acc.hand_push += old.hand_push;
        acc.hand_turn_dx += old.hand_turn_dx;
        acc.hand_turn_dy += old.hand_turn_dy;
        acc.hand_reset |= old.hand_reset;
        self.toggle |= toggle;
    }

    fn take(&mut self) -> (InputFrame, bool) {
        let f = self.frame;
        let a = &mut self.frame;
        a.mouse_dx = 0.0;
        a.mouse_dy = 0.0;
        a.pan_dx = 0.0;
        a.pan_dy = 0.0;
        a.zoom = 0.0;
        a.hand_dx = 0.0;
        a.hand_dy = 0.0;
        a.hand_push = 0.0;
        a.hand_turn_dx = 0.0;
        a.hand_turn_dy = 0.0;
        a.hand_reset = false;
        (f, std::mem::take(&mut self.toggle))
    }
}

fn env_f32(name: &str) -> f32 {
    std::env::var(name).ok().and_then(|s| s.parse().ok()).unwrap_or(0.0)
}

/// Steps the simulated user with everything that drives it: the window's
/// keyboard and mouse, the gamepad, the Input tab, the control API, and the
/// test hooks (XRMIMIC_TEST_TURN / _MOVE / _CONTROLLERS).
pub struct Driver {
    test_turn: f32,
    test_move: f32,
    test_controllers: bool,
}

impl Driver {
    /// Reads the test hooks; XRMIMIC_TEST_HAND_PUT puts the right controller
    /// down at x,y,z (STAGE) at once, as Ctrl + drags would.
    pub fn new(sim: &Mutex<SimState>) -> Driver {
        if let Some(p) = std::env::var("XRMIMIC_TEST_HAND_PUT").ok().and_then(|v| {
            let n: Vec<f32> = v.split(',').filter_map(|x| x.trim().parse().ok()).collect();
            (n.len() == 3).then(|| xrmimic_core::math::Vec3::new(n[0], n[1], n[2]))
        }) {
            sim.lock().unwrap().placed[RIGHT] = Some(xrmimic_core::math::Pose::new(xrmimic_core::math::Quat::IDENTITY, p));
        }
        Driver {
            test_turn: env_f32("XRMIMIC_TEST_TURN"),
            test_move: env_f32("XRMIMIC_TEST_MOVE"),
            test_controllers: std::env::var_os("XRMIMIC_TEST_CONTROLLERS").is_some(),
        }
    }

    /// One step of `dt` seconds with the input gathered since the last.
    pub fn step(&self, sim: &Mutex<SimState>, shared: &Shared, dt: f32) {
        let (mut f, toggle) = shared.input.lock().unwrap().take();
        let mut s = sim.lock().unwrap();
        if toggle {
            s.toggle_active_controller();
        }
        // The keyboard drives the active controller.
        let kb = f.controllers[0];
        f.controllers = [ControllerInput::default(); 2];
        f.controllers[s.active_controller] = kb;
        #[cfg(windows)]
        {
            let pad = crate::window::gamepad();
            for h in [LEFT, RIGHT] {
                f.controllers[h] = f.controllers[h].merge(pad.controllers[h]);
            }
            f.turn_rate += pad.turn_rate;
            f.move_forward += pad.move_forward;
            f.move_right += pad.move_right;
        }
        // Held inputs: the Input tab's, and the control API's.
        let tab = *shared.tab_input.lock().unwrap();
        let control = *shared.control_input.lock().unwrap();
        for h in [LEFT, RIGHT] {
            f.controllers[h] = f.controllers[h].merge(tab[h]).merge(control[h]);
        }
        f.turn_rate += self.test_turn;
        f.move_forward += self.test_move;
        if self.test_controllers {
            // Right: trigger fully pulled and A held. Left: stick pushed down-right, grip squeezed.
            let t = [
                ControllerInput { thumbstick: [0.5, -1.0], squeeze: 1.0, ..Default::default() },
                ControllerInput { trigger: 1.0, button_lower: true, ..Default::default() },
            ];
            for h in [LEFT, RIGHT] {
                f.controllers[h] = f.controllers[h].merge(t[h]);
            }
        }
        s.step(&f, dt);
    }
}

/// The simulated user's thread: steps the user every few milliseconds in
/// real time, unless the frame-stepped clock steps it per frame instead
/// (`Shared::stepped`, crate::headset); feeds the gamepad's motors.
pub fn spawn(sim: Arc<Mutex<SimState>>, shared: Arc<Shared>, driver: Arc<Driver>) -> Option<JoinHandle<()>> {
    std::thread::Builder::new()
        .name("xrmimic-user".into())
        .spawn(move || {
            let mut last = Instant::now();
            // The gamepad's motors follow the hands' haptics: left motor, left hand.
            #[cfg(windows)]
            let mut rumble = [0u16; 2];
            while !shared.stop.load(Ordering::SeqCst) {
                std::thread::sleep(Duration::from_millis(4));
                let now = Instant::now();
                let dt = (now - last).as_secs_f32().min(0.25);
                last = now;
                if !shared.stepped.load(Ordering::Relaxed) {
                    driver.step(&sim, &shared, dt);
                }
                #[cfg(windows)]
                {
                    let now = crate::time::now_ns();
                    let h = *shared.haptics.lock().unwrap();
                    let want = [LEFT, RIGHT].map(|i| (h[i].amplitude(now) * 65535.0) as u16);
                    if want != rumble {
                        rumble = want;
                        crate::window::rumble(want);
                    }
                }
            }
            #[cfg(windows)]
            if rumble != [0, 0] {
                crate::window::rumble([0, 0]);
            }
        })
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drags_accumulate_and_held_state_is_replaced() {
        let mut u = UserInput::default();
        u.update(&InputFrame { mouse_dx: 3.0, zoom: 1.0, move_forward: 1.0, ..Default::default() }, false);
        u.update(&InputFrame { mouse_dx: 2.0, move_forward: 0.0, ..Default::default() }, true);
        let (f, toggle) = u.take();
        assert_eq!((f.mouse_dx, f.zoom, f.move_forward), (5.0, 1.0, 0.0));
        assert!(toggle);
        let (f, toggle) = u.take();
        assert_eq!((f.mouse_dx, f.zoom), (0.0, 0.0), "taken once");
        assert!(!toggle);
    }
}
