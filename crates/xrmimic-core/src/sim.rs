//! The simulated user: head and two controllers, driven by desktop input.
//!
//! Everything is expressed in STAGE space: origin on the floor, +Y up, -Z forward.

use crate::device::DeviceProfile;
use crate::math::{Pose, Quat, Vec3};

pub const LEFT: usize = 0;
pub const RIGHT: usize = 1;

/// One controller's input values (Touch layout).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ControllerInput {
    pub thumbstick: [f32; 2],
    pub trigger: f32,
    pub squeeze: f32,
    /// A on the right controller, X on the left.
    pub button_lower: bool,
    /// B on the right controller, Y on the left.
    pub button_upper: bool,
    pub thumbstick_click: bool,
    /// Menu (left controller only on Touch).
    pub menu: bool,
    pub trigger_touch: bool,
    pub thumbstick_touch: bool,
    /// Touch without a press (a press counts as touching too).
    pub button_lower_touch: bool,
    pub button_upper_touch: bool,
    pub thumbrest_touch: bool,
    /// The trackpad of controllers that also have a stick (Index, WMR); a
    /// Vive wand's trackpad is its `thumbstick`.
    pub trackpad: [f32; 2],
    pub trackpad_click: bool,
    pub trackpad_touch: bool,
    /// Tracked hands: each finger's curl (thumb .. little, 0..1). The pinch
    /// is the trigger and the grasp (a fist) the squeeze.
    pub curls: [f32; 5],
    /// Tracked hands: this hand is out of view (not tracked).
    pub hand_hidden: bool,
}

impl ControllerInput {
    /// Combine two sources (keyboard and gamepad): the larger magnitude wins.
    pub fn merge(self, o: ControllerInput) -> ControllerInput {
        let pick = |a: f32, b: f32| if a.abs() >= b.abs() { a } else { b };
        ControllerInput {
            thumbstick: [pick(self.thumbstick[0], o.thumbstick[0]), pick(self.thumbstick[1], o.thumbstick[1])],
            trigger: self.trigger.max(o.trigger),
            squeeze: self.squeeze.max(o.squeeze),
            button_lower: self.button_lower || o.button_lower,
            button_upper: self.button_upper || o.button_upper,
            thumbstick_click: self.thumbstick_click || o.thumbstick_click,
            menu: self.menu || o.menu,
            trigger_touch: self.trigger_touch || o.trigger_touch || self.trigger.max(o.trigger) > 0.0,
            thumbstick_touch: self.thumbstick_touch
                || o.thumbstick_touch
                || self.thumbstick_click
                || o.thumbstick_click,
            button_lower_touch: self.button_lower_touch || o.button_lower_touch,
            button_upper_touch: self.button_upper_touch || o.button_upper_touch,
            thumbrest_touch: self.thumbrest_touch || o.thumbrest_touch,
            trackpad: [pick(self.trackpad[0], o.trackpad[0]), pick(self.trackpad[1], o.trackpad[1])],
            trackpad_click: self.trackpad_click || o.trackpad_click,
            trackpad_touch: self.trackpad_touch || o.trackpad_touch || self.trackpad_click || o.trackpad_click,
            curls: [0, 1, 2, 3, 4].map(|i| self.curls[i].max(o.curls[i])),
            hand_hidden: self.hand_hidden || o.hand_hidden,
        }
    }

    /// The hand's shape when hands are tracked: the curls, closed further by
    /// the grasp (squeeze), and the pinch (trigger).
    pub fn hand_shape(&self) -> crate::hands::HandShape {
        crate::hands::HandShape { curls: self.curls.map(|c| c.max(self.squeeze)), pinch: self.trigger }
    }
}

/// Desktop input accumulated since the previous step.
#[derive(Clone, Copy, Debug, Default)]
pub struct InputFrame {
    /// Look drag in pixels (left button): right turns right, down looks down.
    pub mouse_dx: f32,
    pub mouse_dy: f32,
    /// Pan drag in pixels (right button): the view follows the cursor, so the
    /// head moves the other way, in its own view plane.
    pub pan_dx: f32,
    pub pan_dy: f32,
    /// Zoom in wheel notches: positive moves the head forward along its gaze.
    pub zoom: f32,
    /// Head movement axes in [-1, 1]: strafe right, up, forward.
    pub move_right: f32,
    pub move_up: f32,
    pub move_forward: f32,
    pub sprint: bool,
    /// Head turning from a gamepad or keys, radians per second (positive turns left).
    pub turn_rate: f32,
    pub controllers: [ControllerInput; 2],
    /// Move the active controller (Ctrl + left drag, Ctrl + wheel): pixels
    /// right and down in the view plane, wheel notches along the gaze. A
    /// moved controller stays where it was put when the head moves.
    pub hand_dx: f32,
    pub hand_dy: f32,
    pub hand_push: f32,
    /// Turn the active controller (Ctrl + right drag), pixels: right turns
    /// it right, down tilts it down.
    pub hand_turn_dx: f32,
    pub hand_turn_dy: f32,
    /// Give the active controller back to the head (R): it follows again.
    pub hand_reset: bool,
}

#[derive(Clone, Debug)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct SimState {
    pub head_position: Vec3,
    pub yaw: f32,
    pub pitch: f32,
    pub ipd: f32,
    /// Standing eye height; LOCAL space starts here above the floor.
    pub eye_height_m: f32,
    pub active_controller: usize,
    pub controllers: [ControllerInput; 2],
    /// Hand offsets in the head's yaw frame (metres).
    pub hand_offsets: [Vec3; 2],
    /// A controller the user moved by itself: its aim pose in STAGE space,
    /// kept while the head moves. None: it follows the head.
    pub placed: [Option<Pose>; 2],
    /// Metres per pixel of a controller drag, per wheel notch, and radians
    /// per pixel of a controller turn.
    pub hand_move_speed: f32,
    pub hand_push_step: f32,
    pub hand_turn_speed: f32,
    pub mouse_sensitivity: f32,
    pub walk_speed: f32,
    /// Metres per pixel of pan drag.
    pub pan_speed: f32,
    /// Metres per wheel notch.
    pub zoom_step: f32,
}

impl SimState {
    pub fn new(device: &DeviceProfile) -> SimState {
        SimState {
            head_position: Vec3::new(0.0, device.eye_height_m, 0.0),
            eye_height_m: device.eye_height_m,
            yaw: 0.0,
            pitch: 0.0,
            ipd: device.ipd,
            active_controller: RIGHT,
            controllers: [ControllerInput::default(); 2],
            hand_offsets: [Vec3::new(-0.18, -0.35, -0.35), Vec3::new(0.18, -0.35, -0.35)],
            placed: [None; 2],
            hand_move_speed: 0.002,
            hand_push_step: 0.05,
            hand_turn_speed: 0.005,
            mouse_sensitivity: 0.0025,
            walk_speed: 1.4,
            pan_speed: 0.004,
            zoom_step: 0.25,
        }
    }

    pub fn toggle_active_controller(&mut self) {
        self.active_controller = 1 - self.active_controller;
    }

    pub fn step(&mut self, input: &InputFrame, dt: f32) {
        self.yaw -= input.mouse_dx * self.mouse_sensitivity;
        self.yaw += input.turn_rate * dt;
        self.pitch -= input.mouse_dy * self.mouse_sensitivity;
        let limit = 89f32.to_radians();
        self.pitch = self.pitch.clamp(-limit, limit);
        self.yaw = wrap_angle(self.yaw);

        let speed = self.walk_speed * if input.sprint { 3.0 } else { 1.0 } * dt;
        let yaw_q = self.yaw_orientation();
        let forward = yaw_q.rotate(Vec3::new(0.0, 0.0, -1.0));
        let right = yaw_q.rotate(Vec3::new(1.0, 0.0, 0.0));
        let mut delta = forward.scale(input.move_forward) + right.scale(input.move_right);
        let flat = delta.length();
        if flat > 1.0 {
            delta = delta.scale(1.0 / flat);
        }
        delta = delta + Vec3::new(0.0, input.move_up, 0.0);
        self.head_position = self.head_position + delta.scale(speed);

        // Pan in the view plane (horizontal right, head-relative up) and zoom
        // along the gaze, both independent of the frame time.
        let head = self.head_pose().orientation;
        let view_up = head.rotate(Vec3::new(0.0, 1.0, 0.0));
        let gaze = head.rotate(Vec3::new(0.0, 0.0, -1.0));
        self.head_position = self.head_position
            + right.scale(-input.pan_dx * self.pan_speed)
            + view_up.scale(input.pan_dy * self.pan_speed)
            + gaze.scale(input.zoom * self.zoom_step);
        self.head_position.y = self.head_position.y.max(0.1);
        self.controllers = input.controllers;
        self.move_hand(input);
    }

    /// The active controller moved or turned by itself, or given back to the head.
    fn move_hand(&mut self, input: &InputFrame) {
        let h = self.active_controller;
        if input.hand_reset {
            self.placed[h] = None;
        }
        let moved = [input.hand_dx, input.hand_dy, input.hand_push, input.hand_turn_dx, input.hand_turn_dy]
            .iter()
            .any(|v| *v != 0.0);
        if !moved {
            return;
        }
        let mut p = self.aim_pose(h);
        let head = self.head_pose().orientation;
        let right = head.rotate(Vec3::new(1.0, 0.0, 0.0));
        let up = head.rotate(Vec3::new(0.0, 1.0, 0.0));
        let gaze = head.rotate(Vec3::new(0.0, 0.0, -1.0));
        p.position = p.position
            + right.scale(input.hand_dx * self.hand_move_speed)
            + up.scale(-input.hand_dy * self.hand_move_speed)
            + gaze.scale(input.hand_push * self.hand_push_step);
        p.position.y = p.position.y.max(0.0);
        // Turn about the room's vertical, tilt about the controller's own right.
        let yaw = Quat::from_axis_angle(Vec3::new(0.0, 1.0, 0.0), -input.hand_turn_dx * self.hand_turn_speed);
        let tilt = Quat::from_axis_angle(Vec3::new(1.0, 0.0, 0.0), -input.hand_turn_dy * self.hand_turn_speed);
        p.orientation = (yaw * p.orientation * tilt).normalized();
        self.placed[h] = Some(p);
    }

    pub fn yaw_orientation(&self) -> Quat {
        Quat::from_yaw_pitch_roll(self.yaw, 0.0, 0.0)
    }

    /// The VIEW space pose: between the eyes.
    pub fn head_pose(&self) -> Pose {
        Pose::new(Quat::from_yaw_pitch_roll(self.yaw, self.pitch, 0.0), self.head_position)
    }

    /// Eye pose in STAGE space; eye 0 is left.
    pub fn eye_pose(&self, eye: usize) -> Pose {
        let half = self.ipd * 0.5;
        let x = if eye == 0 { -half } else { half };
        self.head_pose().compose(&Pose::new(Quat::IDENTITY, Vec3::new(x, 0.0, 0.0)))
    }

    /// Controller aim pose: the hand sits at a fixed offset under the head and
    /// points at the spot 2.5 m along the gaze, so looking aims the ray; or,
    /// once the user moved it by itself, where they put it.
    pub fn aim_pose(&self, hand: usize) -> Pose {
        if let Some(p) = self.placed[hand] {
            return p;
        }
        let position = self.head_position + self.yaw_orientation().rotate(self.hand_offsets[hand]);
        let head = self.head_pose();
        let target = head.transform_point(Vec3::new(0.0, 0.0, -2.5));
        Pose::new(look_rotation(target - position), position)
    }

    /// Grip pose: the aim pose tilted the way a Touch handle sits in the palm.
    pub fn grip_pose(&self, hand: usize) -> Pose {
        let aim = self.aim_pose(hand);
        let tilt = Quat::from_axis_angle(Vec3::new(1.0, 0.0, 0.0), 40f32.to_radians());
        Pose::new((aim.orientation * tilt).normalized(), aim.position)
    }

    /// LOCAL space: where the head started, at eye height, facing -Z.
    pub fn local_space_pose(&self, eye_height: f32) -> Pose {
        Pose::new(Quat::IDENTITY, Vec3::new(0.0, eye_height, 0.0))
    }
}

/// Orientation whose -Z axis points along `dir`, with +Y kept as close to up as possible.
pub fn look_rotation(dir: Vec3) -> Quat {
    let len = dir.length();
    if len < 1e-6 {
        return Quat::IDENTITY;
    }
    let d = dir.scale(1.0 / len);
    let yaw = (-d.x).atan2(-d.z);
    let pitch = d.y.clamp(-1.0, 1.0).asin();
    Quat::from_yaw_pitch_roll(yaw, pitch, 0.0)
}

fn wrap_angle(a: f32) -> f32 {
    use std::f32::consts::PI;
    let mut a = (a + PI).rem_euclid(2.0 * PI) - PI;
    if a <= -PI {
        a += 2.0 * PI;
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-4
    }

    fn sim() -> SimState {
        SimState::new(&DeviceProfile::quest2())
    }

    #[test]
    fn starts_standing_at_eye_height() {
        let s = sim();
        assert!(close(s.head_pose().position, Vec3::new(0.0, 1.6, 0.0)));
    }

    #[test]
    fn eyes_are_ipd_apart_along_head_right() {
        let mut s = sim();
        s.yaw = 1.0;
        s.pitch = 0.4;
        let (l, r) = (s.eye_pose(0).position, s.eye_pose(1).position);
        assert!(((r - l).length() - 0.063).abs() < 1e-5);
        let right = s.head_pose().orientation.rotate(Vec3::new(1.0, 0.0, 0.0));
        assert!(((r - l).scale(1.0 / 0.063).dot(right) - 1.0).abs() < 1e-5);
    }

    #[test]
    fn forward_moves_along_yaw_and_ignores_pitch() {
        let mut s = sim();
        s.yaw = std::f32::consts::FRAC_PI_2; // facing -X
        s.pitch = 0.8;
        s.step(&InputFrame { move_forward: 1.0, ..Default::default() }, 1.0);
        assert!(close(s.head_position, Vec3::new(-1.4, 1.6, 0.0)));
    }

    #[test]
    fn diagonal_is_not_faster() {
        let mut s = sim();
        s.step(&InputFrame { move_forward: 1.0, move_right: 1.0, ..Default::default() }, 1.0);
        let moved = s.head_position - Vec3::new(0.0, 1.6, 0.0);
        assert!((moved.length() - 1.4).abs() < 1e-4);
    }

    #[test]
    fn pan_moves_against_the_drag_in_the_view_plane() {
        let mut s = sim();
        // Dragging right and down: the view follows the cursor, so the head
        // moves left and up.
        s.step(&InputFrame { pan_dx: 100.0, pan_dy: 50.0, ..Default::default() }, 0.0);
        let d = s.head_position - Vec3::new(0.0, 1.6, 0.0);
        assert!(close(d, Vec3::new(-0.4, 0.2, 0.0)), "{d:?}");
        assert_eq!((s.yaw, s.pitch), (0.0, 0.0), "panning does not turn");
    }

    #[test]
    fn zoom_moves_along_the_gaze() {
        let mut s = sim();
        s.pitch = -std::f32::consts::FRAC_PI_4; // looking 45 degrees down
        s.step(&InputFrame { zoom: 2.0, ..Default::default() }, 0.0);
        let d = s.head_position - Vec3::new(0.0, 1.6, 0.0);
        let h = 0.5 / 2f32.sqrt();
        assert!(close(d, Vec3::new(0.0, -h, -h)), "{d:?}");
        s.step(&InputFrame { zoom: -2.0, ..Default::default() }, 0.0);
        assert!(close(s.head_position, Vec3::new(0.0, 1.6, 0.0)));
    }

    #[test]
    fn mouse_right_turns_right_and_pitch_clamps() {
        let mut s = sim();
        s.step(&InputFrame { mouse_dx: 100.0, mouse_dy: -100_000.0, ..Default::default() }, 0.01);
        assert!(s.yaw < 0.0, "turning right is negative yaw about +Y");
        assert!((s.pitch - 89f32.to_radians()).abs() < 1e-5);
    }

    #[test]
    fn aim_points_at_gaze_target() {
        let mut s = sim();
        s.yaw = 0.3;
        s.pitch = -0.2;
        for hand in [LEFT, RIGHT] {
            let aim = s.aim_pose(hand);
            let target = s.head_pose().transform_point(Vec3::new(0.0, 0.0, -2.5));
            let dir = aim.orientation.rotate(Vec3::new(0.0, 0.0, -1.0));
            let want = (target - aim.position).scale(1.0 / (target - aim.position).length());
            assert!(close(dir, want));
        }
        assert!(s.aim_pose(LEFT).position.x < s.aim_pose(RIGHT).position.x);
    }

    #[test]
    fn look_rotation_keeps_up_vertical() {
        let q = look_rotation(Vec3::new(1.0, 0.5, -0.3));
        let right = q.rotate(Vec3::new(1.0, 0.0, 0.0));
        assert!(right.y.abs() < 1e-5);
    }

    #[test]
    fn merge_prefers_larger_axis() {
        let a = ControllerInput { thumbstick: [0.2, -1.0], trigger: 0.0, ..Default::default() };
        let b = ControllerInput { thumbstick: [-0.8, 0.1], trigger: 1.0, ..Default::default() };
        let m = a.merge(b);
        assert_eq!(m.thumbstick, [-0.8, -1.0]);
        assert!(m.trigger_touch);
    }

    #[test]
    fn wrap_angle_stays_in_range() {
        for a in [-10.0f32, -3.2, 0.0, 3.2, 10.0] {
            let w = wrap_angle(a);
            assert!(w > -std::f32::consts::PI - 1e-6 && w <= std::f32::consts::PI + 1e-6);
            assert!(((w - a) / (2.0 * std::f32::consts::PI)).fract().abs() < 1e-4
                || ((w - a) / (2.0 * std::f32::consts::PI)).fract().abs() > 1.0 - 1e-4);
        }
    }

    #[test]
    fn a_moved_controller_stays_put_when_the_head_walks_and_comes_back_on_reset() {
        let mut s = sim();
        let before = s.aim_pose(RIGHT);
        // Drag 100 px right and 50 px up: 0.2 m right, 0.1 m up in the view plane.
        s.step(&InputFrame { hand_dx: 100.0, hand_dy: -50.0, ..Default::default() }, 0.0);
        let moved = s.aim_pose(RIGHT);
        assert!(close(moved.position - before.position, Vec3::new(0.2, 0.1, 0.0)), "{:?}", moved.position);
        assert!(s.placed[LEFT].is_none(), "only the active controller");
        // Walking and turning leave it where it is; the other one follows.
        let left_before = s.aim_pose(LEFT).position;
        s.step(&InputFrame { move_forward: 1.0, mouse_dx: 200.0, ..Default::default() }, 1.0);
        assert!(close(s.aim_pose(RIGHT).position, moved.position));
        assert!((s.aim_pose(LEFT).position - left_before).length() > 1.0);
        // R: it follows the head again.
        s.step(&InputFrame { hand_reset: true, ..Default::default() }, 0.0);
        assert!(s.placed[RIGHT].is_none());
        assert!(close(s.aim_pose(RIGHT).position, s.head_position + s.yaw_orientation().rotate(s.hand_offsets[RIGHT])));
    }

    #[test]
    fn a_controller_turns_and_pushes_out_along_the_gaze() {
        let mut s = sim();
        s.toggle_active_controller(); // left
        let p0 = s.aim_pose(LEFT).position;
        s.step(&InputFrame { hand_push: 4.0, ..Default::default() }, 0.0);
        assert!(close(s.aim_pose(LEFT).position - p0, Vec3::new(0.0, 0.0, -0.2)));
        // Turning right by 90 degrees about the vertical: its ray points along the old ray turned right.
        let ray0 = s.aim_pose(LEFT).orientation.rotate(Vec3::new(0.0, 0.0, -1.0));
        let px = std::f32::consts::FRAC_PI_2 / s.hand_turn_speed;
        s.step(&InputFrame { hand_turn_dx: px, ..Default::default() }, 0.0);
        let ray = s.aim_pose(LEFT).orientation.rotate(Vec3::new(0.0, 0.0, -1.0));
        let want = Quat::from_axis_angle(Vec3::new(0.0, 1.0, 0.0), -std::f32::consts::FRAC_PI_2).rotate(ray0);
        assert!(close(ray, want), "{ray:?} vs {want:?}");
        assert!(close(s.aim_pose(LEFT).position - p0, Vec3::new(0.0, 0.0, -0.2)), "turning does not move it");
        // Tilting down lowers the ray.
        s.step(&InputFrame { hand_turn_dy: 100.0, ..Default::default() }, 0.0);
        assert!(s.aim_pose(LEFT).orientation.rotate(Vec3::new(0.0, 0.0, -1.0)).y < ray.y - 0.3);
    }
}
