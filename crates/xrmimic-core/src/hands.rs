//! A procedural hand for XR_EXT_hand_tracking: the 26 joints of
//! `XrHandJointEXT` (palm, wrist, thumb 4, four fingers 5 each), each with a
//! pose and a radius, posed from five finger curls and a thumb-index pinch.
//!
//! Joint frames follow the OpenXR convention: a joint's -Z points along the
//! bone towards the fingertip, +Y out of the back of the hand (or finger
//! nail), +X completes a right-handed frame. The hand's own space has its
//! origin at the wrist, fingers along -Z and the back of the hand up (+Y),
//! the right hand's thumb on -X (mirrored for the left).

use crate::math::{Pose, Quat, Vec3};
use crate::sim::LEFT;

pub const JOINT_COUNT: usize = 26;
pub const PALM: usize = 0;
pub const WRIST: usize = 1;
pub const THUMB_TIP: usize = 5;
pub const INDEX_TIP: usize = 10;
pub const MIDDLE_METACARPAL: usize = 11;
pub const MIDDLE_PROXIMAL: usize = 12;

/// Joint names in `XrHandJointEXT` order (for logs and tests).
pub const JOINT_NAMES: [&str; JOINT_COUNT] = [
    "palm", "wrist",
    "thumb metacarpal", "thumb proximal", "thumb distal", "thumb tip",
    "index metacarpal", "index proximal", "index intermediate", "index distal", "index tip",
    "middle metacarpal", "middle proximal", "middle intermediate", "middle distal", "middle tip",
    "ring metacarpal", "ring proximal", "ring intermediate", "ring distal", "ring tip",
    "little metacarpal", "little proximal", "little intermediate", "little distal", "little tip",
];

/// How the hand is held: per-finger curls (thumb, index, middle, ring,
/// little; 0 open .. 1 closed) and the thumb-index pinch (0 .. 1, 1 =
/// touching). A fist is all curls at 1.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HandShape {
    pub curls: [f32; 5],
    pub pinch: f32,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HandJoint {
    pub pose: Pose,
    pub radius: f32,
}

fn norm(v: Vec3) -> Vec3 {
    let l = v.length();
    if l > 1e-9 {
        v.scale(1.0 / l)
    } else {
        v
    }
}

fn lerp(a: Vec3, b: Vec3, t: f32) -> Vec3 {
    a + (b - a).scale(t)
}

/// The rotation whose columns are the unit axes x, y, z.
pub fn quat_from_basis(x: Vec3, y: Vec3, z: Vec3) -> Quat {
    let (m00, m11, m22) = (x.x, y.y, z.z);
    let tr = m00 + m11 + m22;
    let q = if tr > 0.0 {
        let s = (tr + 1.0).sqrt() * 2.0;
        Quat { w: 0.25 * s, x: (y.z - z.y) / s, y: (z.x - x.z) / s, z: (x.y - y.x) / s }
    } else if m00 > m11 && m00 > m22 {
        let s = (1.0 + m00 - m11 - m22).sqrt() * 2.0;
        Quat { w: (y.z - z.y) / s, x: 0.25 * s, y: (y.x + x.y) / s, z: (z.x + x.z) / s }
    } else if m11 > m22 {
        let s = (1.0 + m11 - m00 - m22).sqrt() * 2.0;
        Quat { w: (z.x - x.z) / s, x: (y.x + x.y) / s, y: 0.25 * s, z: (z.y + y.z) / s }
    } else {
        let s = (1.0 + m22 - m00 - m11).sqrt() * 2.0;
        Quat { w: (x.y - y.x) / s, x: (z.x + x.z) / s, y: (z.y + y.z) / s, z: 0.25 * s }
    };
    q.normalized()
}

/// The shortest rotation taking direction `a` to direction `b`.
fn rotation_between(a: Vec3, b: Vec3) -> Quat {
    let (a, b) = (norm(a), norm(b));
    let c = a.cross(b);
    let d = a.dot(b).clamp(-1.0, 1.0);
    if c.length() < 1e-7 {
        return Quat::IDENTITY;
    }
    Quat::from_axis_angle(norm(c), d.acos())
}

/// A joint while posing: where it is, the direction of its bone towards the
/// fingertip, and the back-of-hand direction.
#[derive(Clone, Copy)]
struct J {
    p: Vec3,
    d: Vec3,
    u: Vec3,
}

/// Bend a bone direction towards the palm (away from `u`) by `a` radians.
fn bend(d: Vec3, u: Vec3, a: f32) -> (Vec3, Vec3) {
    let (c, s) = (a.cos(), a.sin());
    (norm(d.scale(c) - u.scale(s)), norm(u.scale(c) + d.scale(s)))
}

/// One finger (index .. little): base of the metacarpal, knuckle, bone
/// lengths (proximal, intermediate, distal), curl.
fn finger(base: Vec3, knuckle: Vec3, lens: [f32; 3], curl: f32) -> [J; 5] {
    let up = Vec3::new(0.0, 1.0, 0.0);
    let d0 = norm(knuckle - base);
    let u0 = norm(up - d0.scale(up.dot(d0)));
    let meta = J { p: base, d: d0, u: u0 };
    let (d1, u1) = bend(d0, u0, curl * 75f32.to_radians());
    let prox = J { p: knuckle, d: d1, u: u1 };
    let (d2, u2) = bend(d1, u1, curl * 95f32.to_radians());
    let inter = J { p: knuckle + d1.scale(lens[0]), d: d2, u: u2 };
    let (d3, u3) = bend(d2, u2, curl * 65f32.to_radians());
    let dist = J { p: inter.p + d2.scale(lens[1]), d: d3, u: u3 };
    let tip = J { p: dist.p + d3.scale(lens[2]), d: d3, u: u3 };
    [meta, prox, inter, dist, tip]
}

/// The right hand's joints in its own space (wrist at the origin).
fn right_hand(shape: &HandShape) -> [J; JOINT_COUNT] {
    let c = shape.curls.map(|v| v.clamp(0.0, 1.0));
    let pinch = shape.pinch.clamp(0.0, 1.0);
    // A pinch bends the index a little towards the thumb.
    let index_curl = c[1].max(0.45 * pinch);
    let index = finger(Vec3::new(-0.010, 0.0, -0.004), Vec3::new(-0.028, 0.0, -0.068), [0.040, 0.024, 0.022], index_curl);
    let middle = finger(Vec3::new(0.0, 0.0, -0.004), Vec3::new(-0.006, 0.0, -0.066), [0.044, 0.028, 0.024], c[2]);
    let ring = finger(Vec3::new(0.010, 0.0, -0.004), Vec3::new(0.014, 0.0, -0.062), [0.041, 0.027, 0.023], c[3]);
    let little = finger(Vec3::new(0.018, 0.0, -0.004), Vec3::new(0.032, 0.0, -0.056), [0.032, 0.020, 0.021], c[4]);

    // Thumb: from the base of the palm, forward and out, its nail facing
    // sideways-up; it folds across the palm as it curls.
    let td = norm(Vec3::new(-0.62, -0.28, -0.73));
    let tu = norm(norm(Vec3::new(-0.55, 0.8, 0.0)) - td.scale(norm(Vec3::new(-0.55, 0.8, 0.0)).dot(td)));
    let (d0, u0) = bend(td, tu, c[0] * 20f32.to_radians());
    let tmeta = J { p: Vec3::new(-0.020, -0.012, -0.012), d: d0, u: u0 };
    let (d1, u1) = bend(d0, u0, c[0] * 35f32.to_radians());
    let tprox = J { p: tmeta.p + d0.scale(0.045), d: d1, u: u1 };
    let (d2, u2) = bend(d1, u1, c[0] * 55f32.to_radians());
    let tdist = J { p: tprox.p + d1.scale(0.032), d: d2, u: u2 };
    let ttip = J { p: tdist.p + d2.scale(0.028), d: d2, u: u2 };
    let mut thumb = [tmeta, tprox, tdist, ttip];

    // Pinch: bring the thumb tip to the index tip (just beside it, towards
    // the thumb) by rotating the thumb's bones (cyclic coordinate descent).
    if pinch > 0.0 {
        let target_full = index[4].p + Vec3::new(-0.006, -0.004, 0.0);
        let target = lerp(thumb[3].p, target_full, pinch);
        for _ in 0..12 {
            for j in (0..3).rev() {
                let pivot = thumb[j].p;
                let r = rotation_between(thumb[3].p - pivot, target - pivot);
                for k in j..4 {
                    thumb[k].p = pivot + r.rotate(thumb[k].p - pivot);
                    thumb[k].u = norm(r.rotate(thumb[k].u));
                }
            }
        }
        // Bone directions follow the moved joints.
        for k in 0..3 {
            let d = norm(thumb[k + 1].p - thumb[k].p);
            let u = thumb[k].u;
            thumb[k].d = d;
            thumb[k].u = norm(u - d.scale(u.dot(d)));
        }
        thumb[3].d = thumb[2].d;
        thumb[3].u = thumb[2].u;
    }

    let wrist = J { p: Vec3::new(0.0, 0.0, 0.0), d: middle[0].d, u: middle[0].u };
    // Palm: the middle of the middle finger's metacarpal, oriented like it.
    let palm = J { p: lerp(middle[0].p, middle[1].p, 0.5), d: middle[0].d, u: middle[0].u };
    let mut out = [wrist; JOINT_COUNT];
    out[PALM] = palm;
    out[WRIST] = wrist;
    out[2..6].copy_from_slice(&thumb);
    out[6..11].copy_from_slice(&index);
    out[11..16].copy_from_slice(&middle);
    out[16..21].copy_from_slice(&ring);
    out[21..26].copy_from_slice(&little);
    out
}

/// Joint radii (m): thicker at the palm and knuckles, thinner at the tips.
fn radius(joint: usize) -> f32 {
    match joint {
        PALM => 0.025,
        WRIST => 0.022,
        2..=5 => [0.013, 0.011, 0.010, 0.009][joint - 2],
        _ => {
            let k = (joint - 6) % 5;
            let little = joint >= 21;
            [0.012, 0.010, 0.009, 0.008, 0.007][k] * if little { 0.85 } else { 1.0 }
        }
    }
}

/// The 26 joints of `hand` (LEFT or RIGHT) in the hand's own space.
pub fn hand_joints(hand: usize, shape: &HandShape) -> [HandJoint; JOINT_COUNT] {
    let js = right_hand(shape);
    let mirror = |v: Vec3| if hand == LEFT { Vec3::new(-v.x, v.y, v.z) } else { v };
    let mut out = [HandJoint { pose: Pose::IDENTITY, radius: 0.0 }; JOINT_COUNT];
    for (i, j) in js.iter().enumerate() {
        let (p, d, u) = (mirror(j.p), mirror(j.d), mirror(j.u));
        let z = -d;
        let y = norm(u - z.scale(u.dot(z)));
        let x = y.cross(z);
        out[i] = HandJoint { pose: Pose::new(quat_from_basis(x, y, z), p), radius: radius(i) };
    }
    out
}

/// Where the wrist sits for a hand reaching like a controller would: the
/// palm centre at the controller's aim position, fingers along the aim, the
/// palm turned in (rolled 60 degrees towards the body's middle).
pub fn wrist_pose(hand: usize, aim: &Pose) -> Pose {
    let roll = if hand == LEFT { 60f32 } else { -60f32 }.to_radians();
    let o = (aim.orientation * Quat::from_axis_angle(Vec3::new(0.0, 0.0, 1.0), roll)).normalized();
    Pose::new(o, aim.position + o.rotate(Vec3::new(0.0, 0.0, 0.035)))
}

/// The point between the thumb tip and the index tip: XR_EXT_hand_interaction's
/// pinch pose position, in the hand's space.
pub fn pinch_point(joints: &[HandJoint; JOINT_COUNT]) -> Vec3 {
    lerp(joints[THUMB_TIP].pose.position, joints[INDEX_TIP].pose.position, 0.5)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sim::RIGHT;

    fn dist(a: &HandJoint, b: &HandJoint) -> f32 {
        (a.pose.position - b.pose.position).length()
    }

    #[test]
    fn open_hand_points_along_minus_z_with_the_back_up() {
        let j = hand_joints(RIGHT, &HandShape::default());
        // Fingertips are ahead of their knuckles, about a hand's length out.
        for f in 0..4 {
            let base = 6 + f * 5;
            assert!(j[base + 4].pose.position.z < j[base + 1].pose.position.z - 0.05, "{}", JOINT_NAMES[base + 4]);
        }
        assert!((j[INDEX_TIP].pose.position.length() - 0.15).abs() < 0.03);
        // Joint frames: -Z along the bone to the next joint, +Y up (back of the hand).
        let prox = &j[MIDDLE_PROXIMAL];
        let along = prox.pose.orientation.rotate(Vec3::new(0.0, 0.0, -1.0));
        let to_next = norm(j[MIDDLE_PROXIMAL + 1].pose.position - prox.pose.position);
        assert!(along.dot(to_next) > 0.999);
        assert!(prox.pose.orientation.rotate(Vec3::new(0.0, 1.0, 0.0)).y > 0.99);
        // The right thumb is on -X; every orientation is a unit quaternion.
        assert!(j[THUMB_TIP].pose.position.x < -0.04);
        for (i, k) in j.iter().enumerate() {
            let q = k.pose.orientation;
            assert!(((q.x * q.x + q.y * q.y + q.z * q.z + q.w * q.w) - 1.0).abs() < 1e-4, "{}", JOINT_NAMES[i]);
            assert!(k.radius > 0.004 && k.radius < 0.03);
        }
    }

    #[test]
    fn left_hand_mirrors_the_right() {
        let s = HandShape { curls: [0.2, 0.3, 0.1, 0.5, 0.9], pinch: 0.4 };
        let (l, r) = (hand_joints(LEFT, &s), hand_joints(RIGHT, &s));
        for i in 0..JOINT_COUNT {
            let (a, b) = (l[i].pose.position, r[i].pose.position);
            assert!((a.x + b.x).abs() < 1e-6 && (a.y - b.y).abs() < 1e-6 && (a.z - b.z).abs() < 1e-6);
            // Still right-handed frames: +Y up the back, -Z along the bone.
            let up = l[i].pose.orientation.rotate(Vec3::new(0.0, 1.0, 0.0));
            let up_r = r[i].pose.orientation.rotate(Vec3::new(0.0, 1.0, 0.0));
            assert!((up.x + up_r.x).abs() < 1e-4 && (up.y - up_r.y).abs() < 1e-4, "{}", JOINT_NAMES[i]);
        }
        assert!(l[THUMB_TIP].pose.position.x > 0.04, "the left thumb is on +X");
    }

    #[test]
    fn a_fist_closes_and_a_pinch_touches() {
        let open = hand_joints(RIGHT, &HandShape::default());
        let fist = hand_joints(RIGHT, &HandShape { curls: [1.0; 5], pinch: 0.0 });
        // Curled fingertips come back towards the palm.
        for f in 0..4 {
            let tip = 6 + f * 5 + 4;
            assert!(dist(&fist[tip], &fist[PALM]) < dist(&open[tip], &open[PALM]) * 0.6, "{}", JOINT_NAMES[tip]);
        }
        let apart = dist(&open[THUMB_TIP], &open[INDEX_TIP]);
        let half = hand_joints(RIGHT, &HandShape { curls: [0.0; 5], pinch: 0.5 });
        let pinched = hand_joints(RIGHT, &HandShape { curls: [0.0; 5], pinch: 1.0 });
        let (h, p) = (dist(&half[THUMB_TIP], &half[INDEX_TIP]), dist(&pinched[THUMB_TIP], &pinched[INDEX_TIP]));
        assert!(apart > 0.06, "open: {apart}");
        assert!(h < apart && p < h, "{apart} {h} {p}");
        assert!(p < 0.012, "pinched tips {p} m apart");
        // The thumb's bones keep their lengths through the pinch.
        for k in 2..5 {
            let (a, b) = (dist(&open[k], &open[k + 1]), dist(&pinched[k], &pinched[k + 1]));
            assert!((a - b).abs() < 1e-4, "{}: {a} vs {b}", JOINT_NAMES[k]);
        }
        // The pinch point sits between the two tips.
        let pp = pinch_point(&pinched);
        assert!((pp - pinched[INDEX_TIP].pose.position).length() < 0.01);
    }

    #[test]
    fn the_wrist_sits_behind_the_aim_with_the_palm_turned_in() {
        let aim = Pose::new(Quat::IDENTITY, Vec3::new(0.2, 1.2, -0.4));
        let w = wrist_pose(RIGHT, &aim);
        assert!(w.position.z > aim.position.z, "the wrist is nearer the body");
        // The right palm (-Y of the hand) turns towards -X (the body's middle).
        let palm = w.orientation.rotate(Vec3::new(0.0, -1.0, 0.0));
        assert!(palm.x < -0.5, "{palm:?}");
        let palm_l = wrist_pose(LEFT, &aim).orientation.rotate(Vec3::new(0.0, -1.0, 0.0));
        assert!(palm_l.x > 0.5);
    }
}
