//! State the runtime shares with the session's headset thread.

use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use xrmimic_core::math::Pose;

/// How a layer's colours combine with what is under it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Blend {
    /// No XR_COMPOSITION_LAYER_BLEND_TEXTURE_SOURCE_ALPHA_BIT: alpha ignored.
    Opaque,
    /// Source alpha, colours already multiplied by it (OpenXR's default).
    Premultiplied,
    /// Source alpha with XR_COMPOSITION_LAYER_UNPREMULTIPLIED_ALPHA_BIT.
    Straight,
}

/// The shape of a layer drawn over the projection layer.
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub enum Shape {
    /// Width and height in metres.
    Quad { width: f32, height: f32 },
    /// XR_KHR_composition_layer_cylinder: arc of `central_angle` radians at
    /// `radius` metres around the pose, `height` metres tall.
    Cylinder { radius: f32, central_angle: f32, height: f32 },
    /// XR_KHR_composition_layer_equirect2: sphere section `h_angle` radians
    /// wide, from `upper` to `lower` radians of elevation, at `radius`. An
    /// `infinite` sphere is centred on each eye instead of the pose's position.
    Equirect { radius: f32, h_angle: f32, upper: f32, lower: f32, infinite: bool },
    /// XR_KHR_composition_layer_cube: a cube map at infinity, centred on each
    /// eye, turned by the pose's orientation.
    Cube,
}

/// XR_KHR_composition_layer_depth for one eye of the projection layer: `near`
/// is the distance (m) at depth `min`, `far` at `max` (near > far: reversed;
/// either may be infinite).
#[derive(Clone, Copy, Debug, PartialEq)]
#[cfg_attr(not(windows), allow(dead_code))]
pub struct DepthInfo {
    pub min: f32,
    pub max: f32,
    pub near: f32,
    pub far: f32,
}

/// Where a quad or cylinder layer sits (in STAGE space, resolved when it was
/// submitted) and how it is drawn.
#[derive(Clone, Copy, Debug)]
#[cfg_attr(not(windows), allow(dead_code))] // drawn by the Windows headset window only
pub struct QuadMeta {
    pub pose: Pose,
    pub shape: Shape,
    pub left: bool,
    pub right: bool,
    pub blend: Blend,
    /// XR_FB_composition_layer_depth_test: the XrCompareOpFB its distance is
    /// tested with against the projection layer's depth (None: drawn over it).
    pub depth_test: Option<i32>,
}

#[derive(Default)]
pub struct Shared {
    pub slot: Mutex<crate::gfx::Slot>,
    /// The user closed the headset window: the session should stop.
    pub close_requested: AtomicBool,
    /// The session is gone: the headset thread exits.
    pub stop: AtomicBool,
    pub app_name: Mutex<String>,
    /// Input from the headset window for the simulated user's thread.
    pub input: Mutex<crate::simthread::UserInput>,
    /// The interaction profile the app's actions were attached with ("" until then).
    pub profile: Mutex<String>,
    /// The app's suggested bindings for that profile: (binding path, action name).
    pub bound: Mutex<Vec<(String, String)>>,
    /// What the Input tab holds on each hand.
    pub tab_input: Mutex<[xrmimic_core::sim::ControllerInput; 2]>,
    /// The user put the controllers down and uses tracked hands.
    pub hands: AtomicBool,
    /// Haptic feedback the app sent each hand (xrApplyHapticFeedback).
    pub haptics: Mutex<[xrmimic_core::haptics::HapticState; 2]>,
    /// The app has attached its action sets.
    pub attached: AtomicBool,
}
