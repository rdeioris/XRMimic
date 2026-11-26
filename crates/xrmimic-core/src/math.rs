//! Small f32 pose math in OpenXR conventions: right-handed, +Y up, -Z forward.

use std::ops::{Add, Mul, Neg, Sub};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Vec3 {
    pub x: f32,
    pub y: f32,
    pub z: f32,
}

impl Vec3 {
    pub const ZERO: Vec3 = Vec3 { x: 0.0, y: 0.0, z: 0.0 };
    pub const fn new(x: f32, y: f32, z: f32) -> Self {
        Vec3 { x, y, z }
    }
    pub fn dot(self, o: Vec3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    pub fn cross(self, o: Vec3) -> Vec3 {
        Vec3::new(
            self.y * o.z - self.z * o.y,
            self.z * o.x - self.x * o.z,
            self.x * o.y - self.y * o.x,
        )
    }
    pub fn length(self) -> f32 {
        self.dot(self).sqrt()
    }
    pub fn scale(self, s: f32) -> Vec3 {
        Vec3::new(self.x * s, self.y * s, self.z * s)
    }
}

impl Add for Vec3 {
    type Output = Vec3;
    fn add(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl Sub for Vec3 {
    type Output = Vec3;
    fn sub(self, o: Vec3) -> Vec3 {
        Vec3::new(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
impl Neg for Vec3 {
    type Output = Vec3;
    fn neg(self) -> Vec3 {
        Vec3::new(-self.x, -self.y, -self.z)
    }
}

/// Unit quaternion (x, y, z, w).
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Quat {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Default for Quat {
    fn default() -> Self {
        Quat::IDENTITY
    }
}

impl Quat {
    pub const IDENTITY: Quat = Quat { x: 0.0, y: 0.0, z: 0.0, w: 1.0 };

    pub fn from_axis_angle(axis: Vec3, angle: f32) -> Quat {
        let a = axis.scale(1.0 / axis.length());
        let (s, c) = (angle * 0.5).sin_cos();
        Quat { x: a.x * s, y: a.y * s, z: a.z * s, w: c }
    }

    /// Yaw about +Y, then pitch about the yawed +X, then roll about -Z.
    pub fn from_yaw_pitch_roll(yaw: f32, pitch: f32, roll: f32) -> Quat {
        Quat::from_axis_angle(Vec3::new(0.0, 1.0, 0.0), yaw)
            * Quat::from_axis_angle(Vec3::new(1.0, 0.0, 0.0), pitch)
            * Quat::from_axis_angle(Vec3::new(0.0, 0.0, -1.0), roll)
    }

    pub fn conjugate(self) -> Quat {
        Quat { x: -self.x, y: -self.y, z: -self.z, w: self.w }
    }

    pub fn normalized(self) -> Quat {
        let n = (self.x * self.x + self.y * self.y + self.z * self.z + self.w * self.w).sqrt();
        Quat { x: self.x / n, y: self.y / n, z: self.z / n, w: self.w / n }
    }

    pub fn rotate(self, v: Vec3) -> Vec3 {
        let q = Vec3::new(self.x, self.y, self.z);
        let t = q.cross(v).scale(2.0);
        v + t.scale(self.w) + q.cross(t)
    }
}

impl Mul for Quat {
    type Output = Quat;
    fn mul(self, b: Quat) -> Quat {
        let a = self;
        Quat {
            w: a.w * b.w - a.x * b.x - a.y * b.y - a.z * b.z,
            x: a.w * b.x + a.x * b.w + a.y * b.z - a.z * b.y,
            y: a.w * b.y - a.x * b.z + a.y * b.w + a.z * b.x,
            z: a.w * b.z + a.x * b.y - a.y * b.x + a.z * b.w,
        }
    }
}

/// Rigid transform: maps a point from the pose's local frame into its parent.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Pose {
    pub orientation: Quat,
    pub position: Vec3,
}

impl Pose {
    pub const IDENTITY: Pose = Pose { orientation: Quat::IDENTITY, position: Vec3::ZERO };

    pub fn new(orientation: Quat, position: Vec3) -> Pose {
        Pose { orientation, position }
    }

    pub fn transform_point(&self, p: Vec3) -> Vec3 {
        self.orientation.rotate(p) + self.position
    }

    /// `self * child`: child expressed in self's frame, result in self's parent.
    pub fn compose(&self, child: &Pose) -> Pose {
        Pose {
            orientation: (self.orientation * child.orientation).normalized(),
            position: self.transform_point(child.position),
        }
    }

    pub fn inverse(&self) -> Pose {
        let q = self.orientation.conjugate();
        Pose { orientation: q, position: q.rotate(-self.position) }
    }

    /// Pose of `target` (given in a common base) expressed in `base_space` (also in that base).
    pub fn relative(base_space: &Pose, target: &Pose) -> Pose {
        base_space.inverse().compose(target)
    }
}

/// Field of view as OpenXR angles (radians; left and down are negative).
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct Fov {
    pub left: f32,
    pub right: f32,
    pub up: f32,
    pub down: f32,
}

impl Fov {
    pub fn mirrored(self) -> Fov {
        Fov { left: -self.right, right: -self.left, up: self.up, down: self.down }
    }
}

/// Column-major 4x4 projection like the OpenXR samples' `XrMatrix4x4f_CreateProjectionFov`
/// (graphics API D3D/Vulkan depth range 0..1 when `zero_to_one`, else GL -1..1).
pub fn projection_from_fov(fov: Fov, near: f32, far: f32, zero_to_one: bool) -> [f32; 16] {
    let tl = fov.left.tan();
    let tr = fov.right.tan();
    let tu = fov.up.tan();
    let td = fov.down.tan();
    let w = tr - tl;
    let h = tu - td;
    let offset_z = if zero_to_one { 0.0 } else { near };
    let mut m = [0.0f32; 16];
    m[0] = 2.0 / w;
    m[8] = (tr + tl) / w;
    m[5] = 2.0 / h;
    m[9] = (tu + td) / h;
    m[10] = -(far + offset_z) / (far - near);
    m[14] = -(far * (near + offset_z)) / (far - near);
    m[11] = -1.0;
    m
}

impl Pose {
    /// Column-major 4x4 of this transform (local to parent).
    pub fn to_matrix(&self) -> [f32; 16] {
        let q = self.orientation;
        let (x, y, z, w) = (q.x, q.y, q.z, q.w);
        let p = self.position;
        [
            1.0 - 2.0 * (y * y + z * z),
            2.0 * (x * y + z * w),
            2.0 * (x * z - y * w),
            0.0,
            2.0 * (x * y - z * w),
            1.0 - 2.0 * (x * x + z * z),
            2.0 * (y * z + x * w),
            0.0,
            2.0 * (x * z + y * w),
            2.0 * (y * z - x * w),
            1.0 - 2.0 * (x * x + y * y),
            0.0,
            p.x,
            p.y,
            p.z,
            1.0,
        ]
    }
}

/// `a * b` for column-major 4x4 matrices.
pub fn mat_mul(a: &[f32; 16], b: &[f32; 16]) -> [f32; 16] {
    let mut r = [0.0f32; 16];
    for col in 0..4 {
        for row in 0..4 {
            r[col * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum();
        }
    }
    r
}

/// Column-major scale matrix.
pub fn scale_matrix(x: f32, y: f32, z: f32) -> [f32; 16] {
    let mut m = [0.0f32; 16];
    m[0] = x;
    m[5] = y;
    m[10] = z;
    m[15] = 1.0;
    m
}

/// Clip-space matrix for drawing a quad layer into one eye: the unit square
/// (-0.5..0.5 in x and y, z = 0) scaled to `size` and placed at `quad`, seen
/// from `eye` with field of view `fov` (D3D/Vulkan depth range).
pub fn quad_clip_matrix(eye: &Pose, fov: Fov, quad: &Pose, size: (f32, f32)) -> [f32; 16] {
    let proj = projection_from_fov(fov, 0.05, 1000.0, true);
    let view = eye.inverse().to_matrix();
    let model = mat_mul(&quad.to_matrix(), &scale_matrix(size.0, size.1, 1.0));
    mat_mul(&proj, &mat_mul(&view, &model))
}

/// Height of a cylinder layer: the visible arc's width (radius x angle) over
/// the aspect ratio, as XR_KHR_composition_layer_cylinder defines it.
pub fn cylinder_height(radius: f32, central_angle: f32, aspect_ratio: f32) -> f32 {
    radius * central_angle / aspect_ratio
}

/// A point of a cylinder layer's surface in its own space, for texture
/// coordinates (u, v) with (0, 0) the top left: the arc is centred on -Z and
/// curves around the cylinder's axis (+Y) at `radius`. (The headset window's
/// shader computes the same.)
pub fn cylinder_point(radius: f32, central_angle: f32, height: f32, u: f32, v: f32) -> Vec3 {
    let a = (u - 0.5) * central_angle;
    Vec3::new(radius * a.sin(), (0.5 - v) * height, -radius * a.cos())
}

/// The radius an equirect layer is drawn at when the app asks for an infinite
/// sphere (radius 0 or +inf): far enough for no visible parallax, still well
/// inside the window's 1000 m far plane.
pub const FAR_SPHERE_RADIUS: f32 = 500.0;

/// A point of an XR_KHR_composition_layer_equirect2 sphere section in its own
/// space, for texture coordinates (u, v) with (0, 0) the top left. The section
/// spans `h_angle` radians centred on -Z, from `upper` radians above the
/// horizon (top of the texture) to `lower` (bottom; negative is below).
/// (The headset window's shader computes the same.)
pub fn equirect_point(radius: f32, h_angle: f32, upper: f32, lower: f32, u: f32, v: f32) -> Vec3 {
    let theta = (u - 0.5) * h_angle;
    let phi = upper - v * (upper - lower);
    Vec3::new(radius * phi.cos() * theta.sin(), radius * phi.sin(), -radius * phi.cos() * theta.cos())
}

/// Which face of an XR_KHR_composition_layer_cube cube map a direction (in the
/// layer's own space) hits, and where on it: the standard Vulkan / D3D cube
/// lookup. Faces are +X, -X, +Y, -Y, +Z, -Z (array slices 0..6); (u, v) has
/// (0, 0) at the face image's top left. (The headset window's shader computes
/// the same.)
pub fn cube_lookup(d: Vec3) -> (usize, f32, f32) {
    let (ax, ay, az) = (d.x.abs(), d.y.abs(), d.z.abs());
    let (face, sc, tc, ma) = if ax >= ay && ax >= az {
        if d.x > 0.0 { (0, -d.z, -d.y, ax) } else { (1, d.z, -d.y, ax) }
    } else if ay >= az {
        if d.y > 0.0 { (2, d.x, d.z, ay) } else { (3, d.x, -d.z, ay) }
    } else if d.z > 0.0 {
        (4, d.x, -d.y, az)
    } else {
        (5, -d.x, -d.y, az)
    };
    (face, (sc / ma + 1.0) * 0.5, (tc / ma + 1.0) * 0.5)
}

/// The direction (unit length) that `cube_lookup` maps to (face, u, v).
pub fn cube_direction(face: usize, u: f32, v: f32) -> Vec3 {
    let (s, t) = (2.0 * u - 1.0, 2.0 * v - 1.0);
    let d = match face {
        0 => Vec3::new(1.0, -t, -s),
        1 => Vec3::new(-1.0, -t, s),
        2 => Vec3::new(s, 1.0, t),
        3 => Vec3::new(s, -1.0, -t),
        4 => Vec3::new(s, -t, 1.0),
        _ => Vec3::new(-s, -t, -1.0),
    };
    let l = d.length();
    Vec3::new(d.x / l, d.y / l, d.z / l)
}

/// The distance in metres of a depth value from an XR_KHR_composition_layer_depth
/// buffer. `near` is the distance at `min` and `far` the distance at `max`; a
/// near greater than far is reversed Z, and either may be infinite. (The
/// headset window's depth view computes the same.)
pub fn depth_distance(depth: f32, min: f32, max: f32, near: f32, far: f32) -> f32 {
    let z = ((depth - min) / (max - min)).clamp(0.0, 1.0);
    if near.is_infinite() {
        far / z.max(1e-6)
    } else if far.is_infinite() {
        near / (1.0 - z).max(1e-6)
    } else {
        near * far / (far - z * (far - near))
    }
}

/// The depth value a perspective projection writes for `distance`: the
/// inverse of `depth_distance`.
pub fn depth_value(distance: f32, min: f32, max: f32, near: f32, far: f32) -> f32 {
    let z = if near.is_infinite() {
        far / distance
    } else if far.is_infinite() {
        1.0 - near / distance
    } else {
        far * (distance - near) / (distance * (far - near))
    };
    min + z * (max - min)
}

/// The grey the headset window's depth view shows for a distance: 1 at the
/// eye, 1/2 at 2 m, 1/5 at 8 m.
pub fn depth_shade(distance: f32) -> f32 {
    1.0 / (1.0 + 0.5 * distance.max(0.0))
}

/// The camera of the headset window's centre view: halfway between the two
/// eyes, looking where the left eye looks (the eyes share an orientation),
/// and wide enough to cover both eyes' fields of view (each eye sees further
/// to its own side).
pub fn centre_eye(left: (&Pose, Fov), right: (&Pose, Fov)) -> (Pose, Fov) {
    let (a, b) = (left.0.position, right.0.position);
    let position = Vec3::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5, (a.z + b.z) * 0.5);
    let fov = Fov {
        left: left.1.left.min(right.1.left),
        right: left.1.right.max(right.1.right),
        up: left.1.up.max(right.1.up),
        down: left.1.down.min(right.1.down),
    };
    (Pose::new(left.0.orientation, position), fov)
}

/// Multiply a column-major 4x4 by a point (w = 1) and return clip coordinates.
pub fn mul_point(m: &[f32; 16], p: Vec3) -> [f32; 4] {
    let mut r = [0.0; 4];
    for (row, out) in r.iter_mut().enumerate() {
        *out = m[row] * p.x + m[4 + row] * p.y + m[8 + row] * p.z + m[12 + row];
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f32::consts::{FRAC_PI_2, FRAC_PI_4};

    fn close(a: Vec3, b: Vec3) -> bool {
        (a - b).length() < 1e-5
    }

    #[test]
    fn yaw_turns_forward_to_the_left() {
        // +90 degrees of yaw about +Y turns -Z (forward) into -X (left).
        let q = Quat::from_yaw_pitch_roll(FRAC_PI_2, 0.0, 0.0);
        assert!(close(q.rotate(Vec3::new(0.0, 0.0, -1.0)), Vec3::new(-1.0, 0.0, 0.0)));
    }

    #[test]
    fn pitch_up_looks_up() {
        let q = Quat::from_yaw_pitch_roll(0.0, FRAC_PI_2, 0.0);
        assert!(close(q.rotate(Vec3::new(0.0, 0.0, -1.0)), Vec3::new(0.0, 1.0, 0.0)));
    }

    #[test]
    fn pitch_is_applied_in_the_yawed_frame() {
        let q = Quat::from_yaw_pitch_roll(FRAC_PI_2, 0.3, 0.0);
        // The local +X axis stays horizontal whatever the pitch.
        let right = q.rotate(Vec3::new(1.0, 0.0, 0.0));
        assert!(right.y.abs() < 1e-6);
    }

    #[test]
    fn compose_and_inverse_round_trip() {
        let a = Pose::new(Quat::from_yaw_pitch_roll(0.4, -0.2, 0.1), Vec3::new(1.0, 2.0, 3.0));
        let b = Pose::new(Quat::from_yaw_pitch_roll(-1.1, 0.5, 0.0), Vec3::new(-0.5, 0.1, 0.7));
        let ab = a.compose(&b);
        let back = a.inverse().compose(&ab);
        assert!(close(back.position, b.position));
        let p = Vec3::new(0.3, -0.2, 0.9);
        assert!(close(back.transform_point(p), b.transform_point(p)));
        assert!(close(a.compose(&a.inverse()).position, Vec3::ZERO));
    }

    #[test]
    fn relative_pose_of_a_space_to_itself_is_identity() {
        let a = Pose::new(Quat::from_yaw_pitch_roll(0.7, 0.1, 0.0), Vec3::new(4.0, 1.6, -2.0));
        let r = Pose::relative(&a, &a);
        assert!(close(r.position, Vec3::ZERO));
        assert!((r.orientation.w.abs() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn projection_maps_fov_edges_to_clip_edges() {
        let fov = Fov { left: -0.9, right: 0.7, up: 0.8, down: -0.85 };
        let m = projection_from_fov(fov, 0.1, 100.0, true);
        let z = -2.0;
        let edge = |x: f32, y: f32| {
            let c = mul_point(&m, Vec3::new(x, y, z));
            (c[0] / c[3], c[1] / c[3], c[2] / c[3])
        };
        let (l, _, _) = edge(fov.left.tan() * 2.0, 0.0);
        let (r, _, _) = edge(fov.right.tan() * 2.0, 0.0);
        let (_, u, _) = edge(0.0, fov.up.tan() * 2.0);
        let (_, d, _) = edge(0.0, fov.down.tan() * 2.0);
        assert!((l + 1.0).abs() < 1e-5 && (r - 1.0).abs() < 1e-5);
        assert!((u - 1.0).abs() < 1e-5 && (d + 1.0).abs() < 1e-5);
    }

    #[test]
    fn projection_depth_ranges() {
        let fov = Fov { left: -0.8, right: 0.8, up: 0.8, down: -0.8 };
        for (zero_to_one, lo) in [(true, 0.0), (false, -1.0)] {
            let m = projection_from_fov(fov, 0.1, 50.0, zero_to_one);
            let near = mul_point(&m, Vec3::new(0.0, 0.0, -0.1));
            let far = mul_point(&m, Vec3::new(0.0, 0.0, -50.0));
            assert!((near[2] / near[3] - lo).abs() < 1e-4, "near {zero_to_one}");
            assert!((far[2] / far[3] - 1.0).abs() < 1e-4, "far {zero_to_one}");
        }
    }

    #[test]
    fn pose_matrix_matches_transform_point() {
        let p = Pose::new(Quat::from_yaw_pitch_roll(0.7, -0.3, 0.2), Vec3::new(1.0, -2.0, 0.5));
        let m = p.to_matrix();
        for v in [Vec3::new(0.0, 0.0, 0.0), Vec3::new(0.3, -1.0, 2.0)] {
            let c = mul_point(&m, v);
            assert!(close(Vec3::new(c[0], c[1], c[2]), p.transform_point(v)));
            assert!((c[3] - 1.0).abs() < 1e-6);
        }
        let i = mat_mul(&m, &p.inverse().to_matrix());
        for (k, v) in i.iter().enumerate() {
            let want = if k % 5 == 0 { 1.0 } else { 0.0 };
            assert!((v - want).abs() < 1e-5, "{k}: {v}");
        }
    }

    #[test]
    fn quad_ahead_lands_in_the_middle_and_keeps_its_size() {
        // Symmetric 90 degree view, quad 2 m ahead, 2 m wide: its edges are 1 m
        // off-axis at 2 m, tan = 0.5, so half way to the view's edge.
        let fov = Fov { left: -FRAC_PI_4, right: FRAC_PI_4, up: FRAC_PI_4, down: -FRAC_PI_4 };
        let eye = Pose::new(Quat::IDENTITY, Vec3::new(0.0, 1.6, 0.0));
        let quad = Pose::new(Quat::IDENTITY, Vec3::new(0.0, 1.6, -2.0));
        let m = quad_clip_matrix(&eye, fov, &quad, (2.0, 1.0));
        let ndc = |x: f32, y: f32| {
            let c = mul_point(&m, Vec3::new(x, y, 0.0));
            (c[0] / c[3], c[1] / c[3], c[2] / c[3])
        };
        let (cx, cy, cz) = ndc(0.0, 0.0);
        assert!(cx.abs() < 1e-5 && cy.abs() < 1e-5 && (0.0..1.0).contains(&cz));
        let (rx, _, _) = ndc(0.5, 0.0);
        assert!((rx - 0.5).abs() < 1e-5, "right edge {rx}");
        let (_, ty, _) = ndc(0.0, 0.5);
        assert!((ty - 0.25).abs() < 1e-5, "top edge {ty}");
        // A head turned left sees the quad to its right.
        let turned = Pose::new(Quat::from_yaw_pitch_roll(0.3, 0.0, 0.0), eye.position);
        let m2 = quad_clip_matrix(&turned, fov, &quad, (2.0, 1.0));
        let c = mul_point(&m2, Vec3::ZERO);
        assert!(c[0] / c[3] > 0.1);
    }

    #[test]
    fn cylinder_arc_is_centred_ahead_and_curves_around_the_axis() {
        let (r, a) = (2.0, 1.0);
        let h = cylinder_height(r, a, 4.0);
        assert!((h - 0.5).abs() < 1e-6);
        // Centre of the texture: straight ahead at the radius.
        assert!(close(cylinder_point(r, a, h, 0.5, 0.5), Vec3::new(0.0, 0.0, -2.0)));
        // Left and right edges: half the central angle to each side, same distance.
        let left = cylinder_point(r, a, h, 0.0, 0.5);
        let right = cylinder_point(r, a, h, 1.0, 0.5);
        assert!(left.x < 0.0 && right.x > 0.0);
        assert!((left.length() - r).abs() < 1e-5 && (right.length() - r).abs() < 1e-5);
        assert!(((-left.x).atan2(-left.z) - 0.5).abs() < 1e-5);
        // Top of the texture is up.
        assert!((cylinder_point(r, a, h, 0.5, 0.0).y - 0.25).abs() < 1e-6);
    }

    #[test]
    fn equirect_section_spans_its_angles_on_the_sphere() {
        let (r, h, up, lo) = (3.0, 1.2, 0.45, -0.2);
        // Every point is on the sphere.
        for &(u, v) in &[(0.0, 0.0), (0.3, 0.7), (1.0, 1.0)] {
            assert!((equirect_point(r, h, up, lo, u, v).length() - r).abs() < 1e-5);
        }
        // Centre column, top and bottom rows: at the upper and lower elevations, ahead.
        let top = equirect_point(r, h, up, lo, 0.5, 0.0);
        let bottom = equirect_point(r, h, up, lo, 0.5, 1.0);
        assert!(top.x.abs() < 1e-6 && top.z < 0.0 && ((top.y / r).asin() - up).abs() < 1e-5);
        assert!(((bottom.y / r).asin() - lo).abs() < 1e-5);
        // Left edge is half the horizontal angle to the left.
        let left = equirect_point(r, h, up, lo, 0.0, 0.5);
        assert!(left.x < 0.0 && ((-left.x).atan2(-left.z) - h / 2.0).abs() < 1e-5);
        // A full 360-degree section from pole to pole closes on itself behind.
        let tau = std::f32::consts::TAU;
        let a = equirect_point(1.0, tau, FRAC_PI_2, -FRAC_PI_2, 0.0, 0.5);
        let b = equirect_point(1.0, tau, FRAC_PI_2, -FRAC_PI_2, 1.0, 0.5);
        assert!(close(a, b) && close(a, Vec3::new(0.0, 0.0, 1.0)));
    }

    #[test]
    fn cube_lookup_follows_the_standard_faces() {
        // Axis directions hit the centres of their faces, in +X -X +Y -Y +Z -Z order.
        let axes = [
            Vec3::new(1.0, 0.0, 0.0),
            Vec3::new(-1.0, 0.0, 0.0),
            Vec3::new(0.0, 1.0, 0.0),
            Vec3::new(0.0, -1.0, 0.0),
            Vec3::new(0.0, 0.0, 1.0),
            Vec3::new(0.0, 0.0, -1.0),
        ];
        for (i, a) in axes.iter().enumerate() {
            let (f, u, v) = cube_lookup(*a);
            assert_eq!(f, i);
            assert!((u - 0.5).abs() < 1e-6 && (v - 0.5).abs() < 1e-6);
        }
        // On the side faces the top of the image is up (+Y).
        let (f, _, v) = cube_lookup(Vec3::new(0.0, 0.5, -1.0));
        assert_eq!(f, 5);
        assert!(v < 0.5);
        // Seen from inside, looking along -Z, +X (right) is the image's left
        // half: the standard convention (content is authored for it).
        let (_, u, _) = cube_lookup(Vec3::new(0.5, 0.0, -1.0));
        assert!(u < 0.5);
        // The inverse round-trips.
        for face in 0..6 {
            for &(u, v) in &[(0.2, 0.3), (0.8, 0.6), (0.5, 0.9)] {
                let (f2, u2, v2) = cube_lookup(cube_direction(face, u, v));
                assert_eq!(f2, face);
                assert!((u2 - u).abs() < 1e-5 && (v2 - v).abs() < 1e-5, "{face} {u} {v}");
            }
        }
    }

    #[test]
    fn depth_round_trips_in_every_convention() {
        let inf = f32::INFINITY;
        // (min, max, near, far): standard, reversed, reversed infinite, standard
        // infinite, and a sub-range of the buffer.
        for &(min, max, near, far) in
            &[(0.0, 1.0, 0.1, 100.0), (0.0, 1.0, 100.0, 0.1), (0.0, 1.0, inf, 0.1), (0.0, 1.0, 0.1, inf), (0.2, 0.8, 0.05, 50.0)]
        {
            for &d in &[0.3f32, 1.0, 4.0, 20.0] {
                let v = depth_value(d, min, max, near, far);
                assert!(v >= min - 1e-6 && v <= max + 1e-6, "{v} outside [{min}, {max}]");
                let back = depth_distance(v, min, max, near, far);
                assert!((back - d).abs() / d < 1e-3, "{d} -> {v} -> {back} for {near}..{far}");
            }
        }
        // The ends of the range are the planes.
        assert!((depth_distance(0.0, 0.0, 1.0, 0.1, 100.0) - 0.1).abs() < 1e-5);
        assert!((depth_distance(1.0, 0.0, 1.0, 0.1, 100.0) - 100.0).abs() < 1e-2);
        // Reversed: 1 is the near plane.
        assert!((depth_distance(1.0, 0.0, 1.0, inf, 0.1) - 0.1).abs() < 1e-5);
        assert!((depth_shade(2.0) - 0.5).abs() < 1e-6);
    }

    #[test]
    fn centre_eye_sits_between_the_eyes_and_covers_both() {
        let q = Quat::from_yaw_pitch_roll(0.3, 0.0, 0.0);
        let l = Pose::new(q, Vec3::new(-0.0315, 1.6, 0.0));
        let r = Pose::new(q, Vec3::new(0.0315, 1.6, 0.0));
        let fl = Fov { left: -0.94, right: 0.70, up: 0.84, down: -0.87 };
        let (c, f) = centre_eye((&l, fl), (&r, fl.mirrored()));
        assert!(close(c.position, Vec3::new(0.0, 1.6, 0.0)));
        assert_eq!(c.orientation, q);
        // Symmetric: each side is as wide as the eye on that side sees.
        assert_eq!((f.left, f.right, f.up, f.down), (-0.94, 0.94, 0.84, -0.87));
    }

    #[test]
    fn mirrored_fov_swaps_sides() {
        let f = Fov { left: -0.94, right: 0.70, up: 0.84, down: -0.87 };
        let m = f.mirrored();
        assert_eq!((m.left, m.right), (-0.70, 0.94));
    }
}
