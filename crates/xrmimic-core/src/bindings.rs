//! Interaction-profile binding paths resolved to simulated controller sources.

use crate::sim::{ControllerInput, LEFT, RIGHT};

pub const TOUCH_PROFILE: &str = "/interaction_profiles/oculus/touch_controller";
pub const SIMPLE_PROFILE: &str = "/interaction_profiles/khr/simple_controller";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Source {
    TriggerValue,
    TriggerTouch,
    SqueezeValue,
    Thumbstick,
    ThumbstickX,
    ThumbstickY,
    ThumbstickClick,
    ThumbstickTouch,
    ButtonLowerClick,
    ButtonLowerTouch,
    ButtonUpperClick,
    ButtonUpperTouch,
    Menu,
    ThumbrestTouch,
    /// The trackpad of a controller that also has a stick (Index, WMR).
    Trackpad,
    TrackpadX,
    TrackpadY,
    TrackpadClick,
    TrackpadTouch,
    /// Tracked hands: the hand is tracked (XR_EXT_hand_interaction's `ready_ext`).
    HandReady,
    /// Tracked hands, Input tab only: the hand is out of view.
    HandHidden,
    /// Tracked hands, Input tab only: one finger's curl (thumb 0 .. little 4).
    FingerCurl(u8),
    /// XR_EXT_hand_interaction's pinch point and index fingertip.
    PinchPose,
    PokePose,
    GripPose,
    AimPose,
    Haptic,
}

impl Source {
    pub fn is_pose(self) -> bool {
        matches!(self, Source::GripPose | Source::AimPose | Source::PinchPose | Source::PokePose)
    }
}

/// Value of a source as the four OpenXR action kinds see it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Value {
    Bool(bool),
    Float(f32),
    Vec2([f32; 2]),
}

impl Value {
    pub fn as_float(self) -> f32 {
        match self {
            Value::Bool(b) => b as u8 as f32,
            Value::Float(f) => f,
            Value::Vec2(v) => v[0].hypot(v[1]),
        }
    }
    /// Float sources become booleans with the hysteresis-free threshold OpenXR suggests.
    pub fn as_bool(self) -> bool {
        match self {
            Value::Bool(b) => b,
            other => other.as_float() > 0.5,
        }
    }
    pub fn as_vec2(self) -> [f32; 2] {
        match self {
            Value::Vec2(v) => v,
            Value::Float(f) => [f, 0.0],
            Value::Bool(b) => [b as u8 as f32, 0.0],
        }
    }
}

/// Resolve `/user/hand/{left,right}/{input,output}/...` for `profile`, one of
/// [`crate::device::CONTROLLERS`]. Every profile's inputs map onto the
/// simulated controller: trigger, squeeze/grip, thumbstick (trackpads drive
/// it too), the two face buttons, menu/system, and the grip and aim poses.
pub fn resolve(profile: &str, path: &str) -> Option<(usize, Source)> {
    use Source::*;
    let (hand, rest) = if let Some(r) = path.strip_prefix("/user/hand/left/") {
        (LEFT, r)
    } else if let Some(r) = path.strip_prefix("/user/hand/right/") {
        (RIGHT, r)
    } else {
        return None;
    };
    let family = family(profile)?;
    if family == Family::Hand {
        return resolve_hand(profile, hand, rest);
    }
    let mut parts = rest.splitn(3, '/');
    let kind = parts.next()?;
    let name = parts.next()?;
    let comp = parts.next();
    if kind == "output" {
        return (name == "haptic" && comp.is_none()).then_some((hand, Haptic));
    }
    if kind != "input" {
        return None;
    }
    if family == Family::Simple {
        return Some((
            hand,
            match (name, comp) {
                ("select", None | Some("click")) => TriggerValue,
                ("menu", None | Some("click")) => Menu,
                ("grip", None | Some("pose")) => GripPose,
                ("aim", None | Some("pose")) => AimPose,
                _ => return None,
            },
        ));
    }
    // Face buttons: Touch-style controllers have X/Y on the left and A/B on the
    // right; the Index has A/B on both; Vive and WMR controllers have none.
    let (lower, upper) = match family {
        Family::TouchLike => {
            if hand == LEFT {
                ("x", "y")
            } else {
                ("a", "b")
            }
        }
        Family::Index => ("a", "b"),
        _ => ("", ""),
    };
    let has_stick = family != Family::Vive;
    let has_pad = matches!(family, Family::Index | Family::Vive | Family::Wmr);
    // A Vive wand's trackpad is its only 2D input: it drives the stick values
    // (so the keyboard and gamepad sticks reach it); with a stick beside it
    // the trackpad has values of its own.
    let pad = |vive: Source, own: Source| if has_stick { own } else { vive };
    let src = match (name, comp) {
        ("trigger", None | Some("value") | Some("click")) => TriggerValue,
        ("trigger", Some("touch") | Some("proximity")) => TriggerTouch,
        ("squeeze", None | Some("value") | Some("force") | Some("click")) => SqueezeValue,
        ("thumbstick", None) if has_stick => Thumbstick,
        ("thumbstick", Some("x")) if has_stick => ThumbstickX,
        ("thumbstick", Some("y")) if has_stick => ThumbstickY,
        ("thumbstick", Some("click")) if has_stick => ThumbstickClick,
        ("thumbstick", Some("touch")) if has_stick => ThumbstickTouch,
        ("trackpad", None) if has_pad => pad(Thumbstick, Trackpad),
        ("trackpad", Some("x")) if has_pad => pad(ThumbstickX, TrackpadX),
        ("trackpad", Some("y")) if has_pad => pad(ThumbstickY, TrackpadY),
        ("trackpad", Some("click") | Some("force")) if has_pad => pad(ThumbstickClick, TrackpadClick),
        ("trackpad", Some("touch")) if has_pad => pad(ThumbstickTouch, TrackpadTouch),
        ("thumbrest", Some("touch") | Some("proximity")) if family == Family::TouchLike => ThumbrestTouch,
        // Touch has a menu button on the left only; the HP controller on both.
        ("menu", None | Some("click"))
            if family != Family::TouchLike || hand == LEFT || profile == "/interaction_profiles/hp/mixed_reality_controller" =>
        {
            Menu
        }
        ("system", None | Some("click") | Some("touch")) if family != Family::TouchLike => Menu,
        ("grip" | "grip_surface", None | Some("pose")) => GripPose,
        ("aim", None | Some("pose")) => AimPose,
        (n, None | Some("click")) if !lower.is_empty() && n == lower => ButtonLowerClick,
        (n, Some("touch")) if !lower.is_empty() && n == lower => ButtonLowerTouch,
        (n, None | Some("click")) if !upper.is_empty() && n == upper => ButtonUpperClick,
        (n, Some("touch")) if !upper.is_empty() && n == upper => ButtonUpperTouch,
        _ => return None,
    };
    Some((hand, src))
}

/// The hand interaction profiles' paths (after `/user/hand/<side>/`).
fn resolve_hand(profile: &str, hand: usize, rest: &str) -> Option<(usize, Source)> {
    use Source::*;
    let ext = profile == "/interaction_profiles/ext/hand_interaction_ext";
    let src = match rest {
        "input/aim/pose" | "input/aim" => AimPose,
        "input/grip/pose" | "input/grip" => GripPose,
        "input/pinch_ext/pose" if ext => PinchPose,
        "input/poke_ext/pose" if ext => PokePose,
        "input/pinch_ext/value" | "input/aim_activate_ext/value" if ext => TriggerValue,
        "input/grasp_ext/value" if ext => SqueezeValue,
        "input/pinch_ext/ready_ext" | "input/aim_activate_ext/ready_ext" | "input/grasp_ext/ready_ext" if ext => HandReady,
        "input/select/value" | "input/select" if !ext => TriggerValue,
        "input/squeeze/value" | "input/squeeze" if !ext => SqueezeValue,
        _ => return None,
    };
    Some((hand, src))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Family {
    Hand,
    TouchLike,
    Index,
    Vive,
    Wmr,
    Simple,
}

fn family(profile: &str) -> Option<Family> {
    Some(match profile {
        TOUCH_PROFILE
        | "/interaction_profiles/meta/touch_controller_plus"
        | "/interaction_profiles/facebook/touch_controller_pro"
        | "/interaction_profiles/hp/mixed_reality_controller" => Family::TouchLike,
        "/interaction_profiles/valve/index_controller" => Family::Index,
        "/interaction_profiles/htc/vive_controller" => Family::Vive,
        "/interaction_profiles/microsoft/motion_controller" => Family::Wmr,
        SIMPLE_PROFILE => Family::Simple,
        "/interaction_profiles/ext/hand_interaction_ext" | "/interaction_profiles/microsoft/hand_interaction" => Family::Hand,
        _ => return None,
    })
}

pub fn read(c: &ControllerInput, s: Source) -> Value {
    use Source::*;
    match s {
        TriggerValue => Value::Float(c.trigger),
        TriggerTouch => Value::Bool(c.trigger_touch),
        SqueezeValue => Value::Float(c.squeeze),
        Thumbstick => Value::Vec2(c.thumbstick),
        ThumbstickX => Value::Float(c.thumbstick[0]),
        ThumbstickY => Value::Float(c.thumbstick[1]),
        ThumbstickClick => Value::Bool(c.thumbstick_click),
        ThumbstickTouch => Value::Bool(c.thumbstick_touch),
        ButtonLowerClick => Value::Bool(c.button_lower),
        ButtonLowerTouch => Value::Bool(c.button_lower || c.button_lower_touch),
        ButtonUpperClick => Value::Bool(c.button_upper),
        ButtonUpperTouch => Value::Bool(c.button_upper || c.button_upper_touch),
        Menu => Value::Bool(c.menu),
        ThumbrestTouch => Value::Bool(c.thumbrest_touch),
        Trackpad => Value::Vec2(c.trackpad),
        TrackpadX => Value::Float(c.trackpad[0]),
        TrackpadY => Value::Float(c.trackpad[1]),
        TrackpadClick => Value::Bool(c.trackpad_click),
        TrackpadTouch => Value::Bool(c.trackpad_touch || c.trackpad_click),
        HandReady => Value::Bool(!c.hand_hidden),
        HandHidden => Value::Bool(c.hand_hidden),
        FingerCurl(i) => Value::Float(c.curls[(i as usize).min(4)]),
        GripPose | AimPose | PinchPose | PokePose | Haptic => Value::Bool(false),
    }
}

/// Set a source's value on a controller (the Input tab): the inverse of
/// [`read`] for the inputs a tab row drives (sticks and trackpads take a Vec2
/// on their whole source).
pub fn write(c: &mut ControllerInput, s: Source, v: Value) {
    use Source::*;
    match s {
        TriggerValue => c.trigger = v.as_float().clamp(0.0, 1.0),
        TriggerTouch => c.trigger_touch = v.as_bool(),
        SqueezeValue => c.squeeze = v.as_float().clamp(0.0, 1.0),
        Thumbstick => c.thumbstick = v.as_vec2().map(|a| a.clamp(-1.0, 1.0)),
        ThumbstickX => c.thumbstick[0] = v.as_float().clamp(-1.0, 1.0),
        ThumbstickY => c.thumbstick[1] = v.as_float().clamp(-1.0, 1.0),
        ThumbstickClick => c.thumbstick_click = v.as_bool(),
        ThumbstickTouch => c.thumbstick_touch = v.as_bool(),
        ButtonLowerClick => c.button_lower = v.as_bool(),
        ButtonLowerTouch => c.button_lower_touch = v.as_bool(),
        ButtonUpperClick => c.button_upper = v.as_bool(),
        ButtonUpperTouch => c.button_upper_touch = v.as_bool(),
        Menu => c.menu = v.as_bool(),
        ThumbrestTouch => c.thumbrest_touch = v.as_bool(),
        Trackpad => c.trackpad = v.as_vec2().map(|a| a.clamp(-1.0, 1.0)),
        TrackpadX => c.trackpad[0] = v.as_float().clamp(-1.0, 1.0),
        TrackpadY => c.trackpad[1] = v.as_float().clamp(-1.0, 1.0),
        TrackpadClick => c.trackpad_click = v.as_bool(),
        TrackpadTouch => c.trackpad_touch = v.as_bool(),
        HandHidden => c.hand_hidden = v.as_bool(),
        FingerCurl(i) => c.curls[(i as usize).min(4)] = v.as_float().clamp(0.0, 1.0),
        HandReady | GripPose | AimPose | PinchPose | PokePose | Haptic => {}
    }
}

/// How an Input tab row is driven.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// Pressed or not (clicks, touches).
    Button,
    /// 0..1 (trigger, squeeze).
    Axis,
    /// -1..1 on x and y (sticks, trackpads).
    Stick,
}

/// One input of a profile on one hand, as the Input tab shows it: the paths
/// that read it (e.g. "trigger/value, trigger/click"), the simulated source
/// they share, and how it is driven.
#[derive(Clone, Debug, PartialEq)]
pub struct InputRow {
    pub paths: Vec<&'static str>,
    pub source: Source,
    pub kind: Kind,
}

/// The input paths `profile` has under `/user/hand/<hand>/input/` (the
/// OpenXR specification's lists; the extensions' extra Touch Plus / Pro
/// inputs are left out), in the order the Input tab lists them.
fn profile_paths(profile: &str, hand: usize) -> Vec<&'static str> {
    let left = hand == LEFT;
    let touch = |extra: &[&'static str]| {
        let mut v = vec!["trigger/value", "trigger/touch"];
        v.extend_from_slice(extra);
        v.extend(["squeeze/value", "thumbstick", "thumbstick/click", "thumbstick/touch"]);
        if left {
            v.extend(["x/click", "x/touch", "y/click", "y/touch", "menu/click"]);
        } else {
            v.extend(["a/click", "a/touch", "b/click", "b/touch"]);
        }
        v.push("thumbrest/touch");
        v
    };
    match profile {
        TOUCH_PROFILE => touch(&[]),
        "/interaction_profiles/meta/touch_controller_plus" | "/interaction_profiles/facebook/touch_controller_pro" => {
            touch(&["trigger/proximity"])
        }
        "/interaction_profiles/hp/mixed_reality_controller" => {
            let mut v = vec!["trigger/value", "squeeze/value", "thumbstick", "thumbstick/click"];
            v.extend(if left { ["x/click", "y/click"] } else { ["a/click", "b/click"] });
            v.push("menu/click");
            v
        }
        "/interaction_profiles/valve/index_controller" => vec![
            "trigger/value",
            "trigger/click",
            "trigger/touch",
            "squeeze/value",
            "squeeze/force",
            "thumbstick",
            "thumbstick/click",
            "thumbstick/touch",
            "trackpad",
            "trackpad/force",
            "trackpad/touch",
            "a/click",
            "a/touch",
            "b/click",
            "b/touch",
            "system/click",
            "system/touch",
        ],
        "/interaction_profiles/htc/vive_controller" => vec![
            "trigger/value",
            "trigger/click",
            "squeeze/click",
            "trackpad",
            "trackpad/click",
            "trackpad/touch",
            "menu/click",
            "system/click",
        ],
        "/interaction_profiles/microsoft/motion_controller" => vec![
            "trigger/value",
            "squeeze/click",
            "thumbstick",
            "thumbstick/click",
            "trackpad",
            "trackpad/click",
            "trackpad/touch",
            "menu/click",
        ],
        SIMPLE_PROFILE => vec!["select/click", "menu/click"],
        "/interaction_profiles/ext/hand_interaction_ext" => vec!["pinch_ext/value", "aim_activate_ext/value", "grasp_ext/value"],
        "/interaction_profiles/microsoft/hand_interaction" => vec!["select/value", "squeeze/value"],
        _ => Vec::new(),
    }
}

/// The Input tab's rows for a tracked hand: the hand profile's inputs (or,
/// when the app suggested none, a plain pinch and grasp, which still shape the
/// hand), each finger's curl, and whether the hand is out of view.
pub fn hand_inputs(profile: &str, hand: usize) -> Vec<InputRow> {
    let mut rows = inputs(profile, hand);
    if rows.is_empty() {
        rows.push(InputRow { paths: vec!["pinch"], source: Source::TriggerValue, kind: Kind::Axis });
        rows.push(InputRow { paths: vec!["grasp"], source: Source::SqueezeValue, kind: Kind::Axis });
    }
    for (i, name) in ["thumb curl", "index curl", "middle curl", "ring curl", "little curl"].iter().enumerate() {
        rows.push(InputRow { paths: vec![name], source: Source::FingerCurl(i as u8), kind: Kind::Axis });
    }
    rows.push(InputRow { paths: vec!["out of view"], source: Source::HandHidden, kind: Kind::Button });
    rows
}

/// The inputs `profile` has on `hand` (`LEFT` or `RIGHT`), one row per
/// simulated source, in a stable order. Poses and haptics are not inputs a
/// tab can drive and are left out.
pub fn inputs(profile: &str, hand: usize) -> Vec<InputRow> {
    let side = if hand == LEFT { "left" } else { "right" };
    let mut rows: Vec<InputRow> = Vec::new();
    for c in profile_paths(profile, hand) {
        let Some((h, source)) = resolve(profile, &format!("/user/hand/{side}/input/{c}")) else { continue };
        debug_assert_eq!(h, hand);
        let kind = match source {
            Source::TriggerValue | Source::SqueezeValue => Kind::Axis,
            Source::Thumbstick | Source::Trackpad => Kind::Stick,
            _ => Kind::Button,
        };
        // The simple profile's select is a click: a button, not a trigger axis.
        let kind = if c == "select/click" { Kind::Button } else { kind };
        match rows.iter_mut().find(|r| r.source == source) {
            Some(r) => r.paths.push(c),
            None => rows.push(InputRow { paths: vec![c], source, kind }),
        }
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn touch_paths_resolve_per_hand() {
        let r = |p| resolve(TOUCH_PROFILE, p);
        assert_eq!(r("/user/hand/right/input/a/click"), Some((RIGHT, Source::ButtonLowerClick)));
        assert_eq!(r("/user/hand/left/input/x/click"), Some((LEFT, Source::ButtonLowerClick)));
        assert_eq!(r("/user/hand/left/input/a/click"), None, "A is on the right controller");
        assert_eq!(r("/user/hand/left/input/y/touch"), Some((LEFT, Source::ButtonUpperTouch)));
        assert_eq!(r("/user/hand/right/input/trigger"), Some((RIGHT, Source::TriggerValue)));
        assert_eq!(r("/user/hand/right/input/thumbstick"), Some((RIGHT, Source::Thumbstick)));
        assert_eq!(r("/user/hand/left/input/thumbstick/y"), Some((LEFT, Source::ThumbstickY)));
        assert_eq!(r("/user/hand/left/input/menu/click"), Some((LEFT, Source::Menu)));
        assert_eq!(r("/user/hand/right/input/menu/click"), None);
        assert_eq!(r("/user/hand/right/input/aim/pose"), Some((RIGHT, Source::AimPose)));
        assert_eq!(r("/user/hand/left/output/haptic"), Some((LEFT, Source::Haptic)));
        assert_eq!(r("/user/head/input/system/click"), None);
    }

    #[test]
    fn other_profiles_resolve() {
        let index = "/interaction_profiles/valve/index_controller";
        assert_eq!(resolve(index, "/user/hand/left/input/a/click"), Some((LEFT, Source::ButtonLowerClick)));
        assert_eq!(resolve(index, "/user/hand/right/input/trackpad"), Some((RIGHT, Source::Trackpad)));
        assert_eq!(resolve(index, "/user/hand/right/input/thumbstick"), Some((RIGHT, Source::Thumbstick)));
        assert_eq!(resolve(index, "/user/hand/right/input/squeeze/force"), Some((RIGHT, Source::SqueezeValue)));
        let vive = "/interaction_profiles/htc/vive_controller";
        assert_eq!(resolve(vive, "/user/hand/left/input/trackpad/click"), Some((LEFT, Source::ThumbstickClick)));
        assert_eq!(resolve(vive, "/user/hand/left/input/thumbstick"), None, "a Vive wand has no stick");
        assert_eq!(resolve(vive, "/user/hand/right/input/menu/click"), Some((RIGHT, Source::Menu)));
        let plus = "/interaction_profiles/meta/touch_controller_plus";
        assert_eq!(resolve(plus, "/user/hand/right/input/b/touch"), Some((RIGHT, Source::ButtonUpperTouch)));
        assert_eq!(resolve(plus, "/user/hand/left/input/trigger/proximity"), Some((LEFT, Source::TriggerTouch)));
        let hp = "/interaction_profiles/hp/mixed_reality_controller";
        assert_eq!(resolve(hp, "/user/hand/left/input/x/click"), Some((LEFT, Source::ButtonLowerClick)));
        assert_eq!(resolve(hp, "/user/hand/right/input/menu/click"), Some((RIGHT, Source::Menu)));
        assert_eq!(resolve("/interaction_profiles/unknown/thing", "/user/hand/left/input/a/click"), None);
        // Every profile the device can report resolves its poses.
        for (p, _, _) in crate::device::CONTROLLERS {
            assert_eq!(resolve(p, "/user/hand/left/input/aim/pose"), Some((LEFT, Source::AimPose)), "{p}");
        }
    }

    #[test]
    fn simple_profile_select_is_the_trigger() {
        assert_eq!(
            resolve(SIMPLE_PROFILE, "/user/hand/left/input/select/click"),
            Some((LEFT, Source::TriggerValue))
        );
    }

    #[test]
    fn input_rows_follow_each_profile() {
        let names = |p: &str, h| inputs(p, h).iter().map(|r| r.paths.join(",")).collect::<Vec<_>>();
        let has = |rows: &[String], path: &str| rows.iter().any(|r| r.split(',').any(|q| q == path));
        // Touch: X/Y on the left with the menu, A/B on the right without it.
        let left = names(TOUCH_PROFILE, LEFT);
        let right = names(TOUCH_PROFILE, RIGHT);
        assert!(has(&left, "x/click") && has(&left, "menu/click"));
        assert!(has(&right, "a/click") && !has(&right, "menu/click"));
        assert!(has(&right, "trigger/value") && has(&right, "thumbrest/touch"));
        let index_rows = names("/interaction_profiles/valve/index_controller", RIGHT);
        assert!(index_rows.contains(&"trigger/value,trigger/click".to_string()), "one row per value: {index_rows:?}");
        // Paths reading the same value share a row.
        let index = "/interaction_profiles/valve/index_controller";
        let rows = inputs(index, RIGHT);
        let squeeze = rows.iter().find(|r| r.source == Source::SqueezeValue).unwrap();
        assert_eq!(squeeze.paths, vec!["squeeze/value", "squeeze/force"]);
        assert_eq!(squeeze.kind, Kind::Axis);
        // The Index has both a stick and a trackpad, each its own row.
        assert!(rows.iter().any(|r| r.source == Source::Thumbstick && r.kind == Kind::Stick));
        assert!(rows.iter().any(|r| r.source == Source::Trackpad && r.kind == Kind::Stick));
        // A Vive wand: a trackpad (its stick values), no stick, no face buttons.
        let vive = names("/interaction_profiles/htc/vive_controller", LEFT);
        assert!(has(&vive, "trackpad") && !vive.iter().any(|n| n.starts_with("thumbstick")));
        assert!(!vive.iter().any(|n| n.starts_with('a') || n.starts_with('x')));
        // The simple controller: select and menu only.
        assert_eq!(names(SIMPLE_PROFILE, RIGHT), vec!["select/click", "menu/click"]);
    }

    #[test]
    fn every_row_writes_what_it_reads() {
        for (p, _, _) in crate::device::CONTROLLERS {
            for hand in [LEFT, RIGHT] {
                for row in inputs(p, hand) {
                    let mut c = ControllerInput::default();
                    let v = match row.kind {
                        Kind::Button => Value::Bool(true),
                        Kind::Axis => Value::Float(0.6),
                        Kind::Stick => Value::Vec2([0.5, -0.25]),
                    };
                    write(&mut c, row.source, v);
                    let back = read(&c, row.source);
                    let same = match v {
                        Value::Bool(b) => back.as_bool() == b,
                        Value::Float(f) => back.as_float() == f,
                        Value::Vec2(xy) => back.as_vec2() == xy,
                    };
                    assert!(same, "{p} {hand} {:?}: wrote {v:?}, read {back:?}", row.paths);
                    // And nothing else in that row's profile moved, except what a press implies.
                    for other in inputs(p, hand).iter().filter(|o| o.source != row.source) {
                        let implied = matches!(
                            (row.source, other.source),
                            (Source::ButtonLowerClick, Source::ButtonLowerTouch)
                                | (Source::ButtonUpperClick, Source::ButtonUpperTouch)
                                | (Source::TrackpadClick, Source::TrackpadTouch)
                        );
                        let rest = read(&c, other.source);
                        let zero = matches!(rest, Value::Bool(false)) || rest.as_float() == 0.0;
                        assert!(zero || implied, "{p}: setting {:?} moved {:?}", row.paths, other.paths);
                    }
                }
            }
        }
    }

    #[test]
    fn hand_profiles_resolve_and_list_their_rows() {
        let ext = "/interaction_profiles/ext/hand_interaction_ext";
        let msft = "/interaction_profiles/microsoft/hand_interaction";
        assert_eq!(resolve(ext, "/user/hand/left/input/pinch_ext/value"), Some((LEFT, Source::TriggerValue)));
        assert_eq!(resolve(ext, "/user/hand/right/input/grasp_ext/value"), Some((RIGHT, Source::SqueezeValue)));
        assert_eq!(resolve(ext, "/user/hand/right/input/pinch_ext/pose"), Some((RIGHT, Source::PinchPose)));
        assert_eq!(resolve(ext, "/user/hand/right/input/poke_ext/pose"), Some((RIGHT, Source::PokePose)));
        assert_eq!(resolve(ext, "/user/hand/left/input/aim_activate_ext/ready_ext"), Some((LEFT, Source::HandReady)));
        assert_eq!(resolve(ext, "/user/hand/left/input/select/value"), None, "MSFT's path, not EXT's");
        assert_eq!(resolve(msft, "/user/hand/left/input/select/value"), Some((LEFT, Source::TriggerValue)));
        assert_eq!(resolve(msft, "/user/hand/right/input/aim/pose"), Some((RIGHT, Source::AimPose)));
        assert_eq!(resolve(msft, "/user/hand/right/input/pinch_ext/pose"), None);
        // The tab: the profile's pinch and grasp, five curls, out of view.
        let rows = hand_inputs(ext, LEFT);
        assert_eq!(rows[0].paths, vec!["pinch_ext/value", "aim_activate_ext/value"]);
        assert_eq!(rows.len(), 2 + 5 + 1);
        assert_eq!(hand_inputs("", RIGHT)[0].paths, vec!["pinch"], "no hand profile: a plain pinch");
        let mut c = ControllerInput::default();
        write(&mut c, Source::FingerCurl(3), Value::Float(0.4));
        write(&mut c, Source::HandHidden, Value::Bool(true));
        assert_eq!(c.curls[3], 0.4);
        assert!(!read(&c, Source::HandReady).as_bool());
        // The shape: the grasp closes every finger at least that far, the pinch is the trigger.
        let c = ControllerInput { squeeze: 0.5, trigger: 0.8, curls: [0.0, 0.9, 0.0, 0.0, 0.0], ..Default::default() };
        let s = c.hand_shape();
        assert_eq!((s.curls, s.pinch), ([0.5, 0.9, 0.5, 0.5, 0.5], 0.8));
    }

    #[test]
    fn values_convert_between_kinds() {
        let c = ControllerInput { trigger: 0.7, thumbstick: [0.0, -1.0], ..Default::default() };
        assert!(read(&c, Source::TriggerValue).as_bool());
        assert_eq!(read(&c, Source::ThumbstickY).as_float(), -1.0);
        assert_eq!(read(&c, Source::Thumbstick).as_vec2(), [0.0, -1.0]);
        assert_eq!(read(&c, Source::ButtonLowerClick).as_float(), 0.0);
    }
}
