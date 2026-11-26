//! The simulated headset: what the runtime tells apps about it. Presets copy
//! the published or commonly reported figures of real headsets (approximate:
//! real runtimes vary with firmware, IPD and settings), and every field can be
//! overridden and saved as `key=value` lines.

use crate::math::Fov;

/// Interaction profiles the runtime can report, with the OpenXR extension an
/// app must enable to use each (None: core).
pub const CONTROLLERS: &[(&str, &str, Option<&str>)] = &[
    ("/interaction_profiles/oculus/touch_controller", "Oculus Touch", None),
    ("/interaction_profiles/meta/touch_controller_plus", "Meta Touch Plus (Quest 3)", Some("XR_META_touch_controller_plus")),
    ("/interaction_profiles/facebook/touch_controller_pro", "Meta Touch Pro", Some("XR_FB_touch_controller_pro")),
    ("/interaction_profiles/valve/index_controller", "Valve Index", None),
    ("/interaction_profiles/htc/vive_controller", "HTC Vive", None),
    ("/interaction_profiles/hp/mixed_reality_controller", "HP Reverb G2", Some("XR_EXT_hp_mixed_reality_controller")),
    ("/interaction_profiles/microsoft/motion_controller", "Windows Mixed Reality", None),
    ("/interaction_profiles/khr/simple_controller", "Simple controller", None),
];

/// Extensions a device may advertise, beyond the ones the runtime always needs
/// (graphics binding, clock, sandbox): (name, spec version, what xrmimic does with it).
pub const OPTIONAL_EXTENSIONS: &[(&str, u32, &str)] = &[
    ("XR_EXT_local_floor", 1, "LOCAL_FLOOR space"),
    ("XR_MSFT_unbounded_reference_space", 1, "UNBOUNDED_MSFT space"),
    ("XR_META_touch_controller_plus", 1, "Quest 3 controller profile"),
    ("XR_FB_touch_controller_pro", 1, "Quest Pro controller profile"),
    ("XR_EXT_hp_mixed_reality_controller", 1, "HP Reverb G2 controller profile"),
    ("XR_KHR_composition_layer_cylinder", 1, "cylinder layers"),
    ("XR_KHR_composition_layer_equirect2", 1, "equirect layers"),
    ("XR_KHR_composition_layer_cube", 8, "cube layers"),
    ("XR_KHR_composition_layer_depth", 6, "depth with projection layers (Depth view)"),
    ("XR_FB_composition_layer_depth_test", 1, "layers hidden behind the projection layer's depth"),
    ("XR_EXT_hand_tracking", 4, "hand joints (Hands in the Input tab)"),
    ("XR_EXT_hand_interaction", 1, "hand interaction profile: pinch, grasp, poke"),
    ("XR_MSFT_hand_interaction", 1, "Microsoft hand interaction profile: select, squeeze"),
];

/// Interaction profiles for tracked hands (the Input tab's Hands), with the
/// extension each needs: when hands are in use the runtime reports the first
/// of these the app suggested bindings for.
pub const HAND_PROFILES: &[(&str, &str, &str)] = &[
    ("/interaction_profiles/ext/hand_interaction_ext", "Hands (XR_EXT_hand_interaction)", "XR_EXT_hand_interaction"),
    ("/interaction_profiles/microsoft/hand_interaction", "Hands (XR_MSFT_hand_interaction)", "XR_MSFT_hand_interaction"),
];

#[derive(Clone, Debug, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DeviceProfile {
    /// Preset id this was built from ("custom" once edited by hand is still fine).
    pub preset: String,
    /// XrSystemProperties::systemName.
    pub name: String,
    pub vendor_id: u32,
    /// Recommended per-eye render size.
    pub eye_width: u32,
    pub eye_height: u32,
    /// Maximum per-eye size the runtime accepts for swapchains.
    pub max_eye_width: u32,
    pub max_eye_height: u32,
    /// Left eye field of view; the right eye is the mirror image.
    pub left_fov: Fov,
    /// Interpupillary distance in metres.
    pub ipd: f32,
    /// Standing eye height above the floor in metres (STAGE origin is on the floor).
    pub eye_height_m: f32,
    pub refresh_rates: Vec<f32>,
    pub default_refresh: f32,
    /// Interaction profile path reported for both hands.
    pub controllers: String,
    /// Optional extensions advertised (names from [`OPTIONAL_EXTENSIONS`]).
    pub extensions: Vec<String>,
    /// The play area (bounded floor): width along X and depth along Z in
    /// metres, centred on the STAGE origin; [0, 0] = no boundary set up.
    pub play_area: [f32; 2],
}

fn fov_deg(left: f32, right: f32, up: f32, down: f32) -> Fov {
    Fov { left: left.to_radians(), right: right.to_radians(), up: up.to_radians(), down: down.to_radians() }
}

impl DeviceProfile {
    #[allow(clippy::too_many_arguments)]
    fn make(
        preset: &str,
        name: &str,
        vendor_id: u32,
        eye: (u32, u32),
        fov: Fov,
        rates: &[f32],
        default_refresh: f32,
        controllers: &str,
    ) -> DeviceProfile {
        DeviceProfile {
            preset: preset.into(),
            name: name.into(),
            vendor_id,
            eye_width: eye.0,
            eye_height: eye.1,
            max_eye_width: 4096,
            max_eye_height: 4096,
            left_fov: fov,
            ipd: 0.063,
            eye_height_m: 1.6,
            refresh_rates: rates.to_vec(),
            default_refresh,
            controllers: controllers.into(),
            extensions: default_extensions(controllers),
            play_area: [2.5, 2.5],
        }
    }

    /// Values a Quest 2 reports over Link: 1832x1920 per eye, ~63 mm IPD.
    pub fn quest2() -> DeviceProfile {
        Self::make(
            "quest2",
            "Oculus Quest2",
            0x2833,
            (1832, 1920),
            fov_deg(-52.0, 42.0, 48.0, -50.0),
            &[72.0, 90.0, 120.0],
            90.0,
            CONTROLLERS[0].0,
        )
    }

    pub fn presets() -> Vec<DeviceProfile> {
        vec![
            Self::quest2(),
            Self::make(
                "quest3",
                "Meta Quest 3",
                0x2833,
                (2064, 2208),
                fov_deg(-52.0, 43.0, 45.0, -52.0),
                &[72.0, 90.0, 120.0],
                90.0,
                CONTROLLERS[1].0,
            ),
            Self::make(
                "questpro",
                "Meta Quest Pro",
                0x2833,
                (1800, 1920),
                fov_deg(-50.0, 42.0, 47.0, -50.0),
                &[72.0, 90.0],
                90.0,
                CONTROLLERS[2].0,
            ),
            Self::make(
                "index",
                "Valve Index",
                0x28DE,
                (2016, 2240),
                fov_deg(-55.0, 48.0, 55.0, -55.0),
                &[80.0, 90.0, 120.0, 144.0],
                90.0,
                CONTROLLERS[3].0,
            )
            .room_scale(),
            Self::make(
                "vivepro2",
                "HTC Vive Pro 2",
                0x0BB4,
                (2448, 2448),
                fov_deg(-58.0, 51.0, 56.0, -56.0),
                &[90.0, 120.0],
                90.0,
                CONTROLLERS[4].0,
            )
            .room_scale(),
            Self::make(
                "reverbg2",
                "HP Reverb G2",
                0x03F0,
                (2160, 2160),
                fov_deg(-49.0, 44.0, 46.0, -46.0),
                &[90.0],
                90.0,
                CONTROLLERS[5].0,
            ),
        ]
    }

    /// Room-scale presets (base stations) get a larger play area.
    fn room_scale(mut self) -> DeviceProfile {
        self.play_area = [3.5, 3.0];
        self
    }

    pub fn preset_by_id(id: &str) -> Option<DeviceProfile> {
        Self::presets().into_iter().find(|p| p.preset == id)
    }

    pub fn fov(&self, eye: usize) -> Fov {
        if eye == 0 {
            self.left_fov
        } else {
            self.left_fov.mirrored()
        }
    }

    /// The settings as `key=value` lines (angles in degrees, IPD in millimetres).
    pub fn to_ini(&self) -> String {
        let f = self.left_fov;
        let rates: Vec<String> = self.refresh_rates.iter().map(|r| format!("{r}")).collect();
        format!(
            "preset={}\nname={}\nvendor_id={:#06x}\neye_width={}\neye_height={}\n\
             fov_left={}\nfov_right={}\nfov_up={}\nfov_down={}\nipd_mm={}\neye_height_m={}\n\
             refresh_rates={}\ndefault_refresh={}\ncontrollers={}\nextensions={}\nplay_area_m={}\n",
            self.preset,
            self.name,
            self.vendor_id,
            self.eye_width,
            self.eye_height,
            round(f.left.to_degrees()),
            round(f.right.to_degrees()),
            round(f.up.to_degrees()),
            round(f.down.to_degrees()),
            round(self.ipd * 1000.0),
            self.eye_height_m,
            rates.join(","),
            self.default_refresh,
            self.controllers,
            self.extensions.join(","),
            if self.play_area == [0.0, 0.0] {
                "0".to_string()
            } else {
                format!("{}x{}", self.play_area[0], self.play_area[1])
            }
        )
    }

    /// Apply `key=value` lines on top of `self`. `preset=` first loads that
    /// preset, so later lines override it. Unknown or invalid lines are
    /// reported, not fatal.
    pub fn apply_ini(&mut self, text: &str) -> Vec<String> {
        let mut problems = Vec::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let Some((k, v)) = line.split_once('=') else {
                problems.push(format!("line {}: no '='", n + 1));
                continue;
            };
            if let Err(e) = self.set(k.trim(), v.trim()) {
                problems.push(format!("line {}: {e}", n + 1));
            }
        }
        problems
    }

    /// Set one field from text (the same keys and units as `to_ini`).
    pub fn set(&mut self, key: &str, value: &str) -> Result<(), String> {
        let num = |v: &str| v.parse::<f32>().map_err(|_| format!("{key}: not a number: {v:?}"));
        let int = |v: &str| {
            let v = v.trim();
            let r = if let Some(h) = v.strip_prefix("0x").or_else(|| v.strip_prefix("0X")) {
                u32::from_str_radix(h, 16)
            } else {
                v.parse::<u32>()
            };
            r.map_err(|_| format!("{key}: not a whole number: {v:?}"))
        };
        let angle = |v: &str, sign: f32| -> Result<f32, String> {
            let d = num(v)?;
            if !(0.0..89.0).contains(&(d * sign)) {
                return Err(format!("{key}: {d} is out of range (0 to 89 degrees, sign as in OpenXR)"));
            }
            Ok(d.to_radians())
        };
        match key {
            "preset" => {
                *self = Self::preset_by_id(value).ok_or_else(|| format!("unknown preset {value:?}"))?;
            }
            "name" => {
                if value.is_empty() || value.len() > 255 {
                    return Err("name: 1 to 255 characters".into());
                }
                self.name = value.into();
            }
            "vendor_id" => self.vendor_id = int(value)?,
            "eye_width" | "eye_height" => {
                let v = int(value)?;
                if !(64..=self.max_eye_width).contains(&v) {
                    return Err(format!("{key}: {v} is out of range (64 to {})", self.max_eye_width));
                }
                if key == "eye_width" {
                    self.eye_width = v;
                } else {
                    self.eye_height = v;
                }
            }
            "fov_left" => self.left_fov.left = angle(value, -1.0)?,
            "fov_right" => self.left_fov.right = angle(value, 1.0)?,
            "fov_up" => self.left_fov.up = angle(value, 1.0)?,
            "fov_down" => self.left_fov.down = angle(value, -1.0)?,
            "ipd_mm" => {
                let v = num(value)?;
                if !(40.0..=90.0).contains(&v) {
                    return Err(format!("ipd_mm: {v} is out of range (40 to 90)"));
                }
                self.ipd = v / 1000.0;
            }
            "eye_height_m" => {
                let v = num(value)?;
                if !(0.3..=2.5).contains(&v) {
                    return Err(format!("eye_height_m: {v} is out of range (0.3 to 2.5)"));
                }
                self.eye_height_m = v;
            }
            "refresh_rates" => {
                let mut v = Vec::new();
                for p in value.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                    let r = num(p)?;
                    if !(20.0..=240.0).contains(&r) {
                        return Err(format!("refresh_rates: {r} is out of range (20 to 240)"));
                    }
                    v.push(r);
                }
                if v.is_empty() {
                    return Err("refresh_rates: at least one".into());
                }
                self.refresh_rates = v;
                if !self.refresh_rates.contains(&self.default_refresh) {
                    self.default_refresh = self.refresh_rates[0];
                }
            }
            "default_refresh" => {
                let r = num(value)?;
                if !(20.0..=240.0).contains(&r) {
                    return Err(format!("default_refresh: {r} is out of range (20 to 240)"));
                }
                if !self.refresh_rates.contains(&r) {
                    self.refresh_rates.push(r);
                }
                self.default_refresh = r;
            }
            "controllers" => {
                if !CONTROLLERS.iter().any(|(p, _, _)| *p == value) {
                    return Err(format!("controllers: unknown interaction profile {value:?}"));
                }
                self.controllers = value.into();
            }
            "extensions" => {
                let mut v = Vec::new();
                for e in value.split(',').map(str::trim).filter(|e| !e.is_empty()) {
                    if !OPTIONAL_EXTENSIONS.iter().any(|(n, _, _)| *n == e) {
                        return Err(format!("extensions: {e:?} is not one xrmimic can advertise"));
                    }
                    if !v.iter().any(|x: &String| x == e) {
                        v.push(e.to_string());
                    }
                }
                self.extensions = v;
            }
            "play_area_m" => {
                let v = value.trim();
                if v == "0" || v.eq_ignore_ascii_case("none") || v.is_empty() {
                    self.play_area = [0.0, 0.0];
                } else {
                    let (w, d) = v
                        .split_once(['x', 'X', '*'])
                        .ok_or_else(|| format!("play_area_m: width x depth (as 3x2.5) or 0, not {v:?}"))?;
                    let (w, d) = (num(w.trim())?, num(d.trim())?);
                    if !(1.0..=20.0).contains(&w) || !(1.0..=20.0).contains(&d) {
                        return Err(format!("play_area_m: {w} x {d} is out of range (1 to 20 m a side, or 0)"));
                    }
                    self.play_area = [w, d];
                }
            }
            other => return Err(format!("unknown key {other:?}")),
        }
        Ok(())
    }
}

/// What a preset advertises: the floor and unbounded spaces, cylinder, equirect and cube
/// layers (all drawn by xrmimic), depth with projection layers (shown by the
/// window's Depth view), and the extension its own controllers need.
fn default_extensions(controllers: &str) -> Vec<String> {
    let mut v = vec![
        "XR_EXT_local_floor".to_string(),
        "XR_MSFT_unbounded_reference_space".to_string(),
        "XR_KHR_composition_layer_cylinder".to_string(),
        "XR_KHR_composition_layer_equirect2".to_string(),
        "XR_KHR_composition_layer_cube".to_string(),
        "XR_KHR_composition_layer_depth".to_string(),
        "XR_FB_composition_layer_depth_test".to_string(),
    ];
    if let Some((_, _, Some(ext))) = CONTROLLERS.iter().find(|(p, _, _)| *p == controllers) {
        v.push(ext.to_string());
    }
    // Meta's headsets track hands: their runtime offers hand tracking and the
    // EXT hand interaction profile.
    if controllers.contains("/oculus/") || controllers.contains("/meta/") || controllers.contains("/facebook/") {
        v.push("XR_EXT_hand_tracking".to_string());
        v.push("XR_EXT_hand_interaction".to_string());
    }
    v
}

fn round(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quest2_eyes_mirror_and_point_outwards() {
        let d = DeviceProfile::quest2();
        let (l, r) = (d.fov(0), d.fov(1));
        assert_eq!(l.left, -r.right);
        // Each eye sees further to its own outer side.
        assert!(-l.left > l.right);
        assert!(r.right > -r.left);
        assert_eq!((d.eye_width, d.eye_height), (1832, 1920));
    }

    #[test]
    fn every_preset_is_valid_and_round_trips() {
        let presets = DeviceProfile::presets();
        assert!(presets.len() >= 6);
        for p in presets {
            assert!(CONTROLLERS.iter().any(|(c, _, _)| *c == p.controllers), "{}", p.preset);
            assert!(p.refresh_rates.contains(&p.default_refresh), "{}", p.preset);
            let mut q = DeviceProfile::quest2();
            assert!(q.apply_ini(&p.to_ini()).is_empty(), "{}", p.preset);
            assert_eq!(q.name, p.name);
            assert_eq!((q.eye_width, q.eye_height), (p.eye_width, p.eye_height));
            assert_eq!(q.controllers, p.controllers);
            assert!((q.left_fov.left - p.left_fov.left).abs() < 1e-3, "{}", p.preset);
            assert!((q.ipd - p.ipd).abs() < 1e-6);
        }
    }

    #[test]
    fn ini_overrides_after_the_preset_and_reports_bad_lines() {
        let mut d = DeviceProfile::quest2();
        let problems = d.apply_ini(
            "# comment\npreset=index\nname=My Headset\nipd_mm=68\nfov_left=-60\n\
             eye_width=99999\nbogus=1\nrefresh_rates=90,144\n",
        );
        assert_eq!(d.preset, "index");
        assert_eq!(d.name, "My Headset");
        assert!((d.ipd - 0.068).abs() < 1e-6);
        assert!((d.left_fov.left.to_degrees() + 60.0).abs() < 1e-3);
        assert_eq!(d.eye_width, 2016, "out-of-range width is refused");
        assert_eq!(d.refresh_rates, vec![90.0, 144.0]);
        assert_eq!(problems.len(), 2, "{problems:?}");
    }

    #[test]
    fn presets_advertise_their_controllers_extension() {
        let ext = |id: &str| DeviceProfile::preset_by_id(id).unwrap().extensions;
        assert_eq!(
            ext("quest2"),
            vec![
                "XR_EXT_local_floor",
                "XR_MSFT_unbounded_reference_space",
                "XR_KHR_composition_layer_cylinder",
                "XR_KHR_composition_layer_equirect2",
                "XR_KHR_composition_layer_cube",
                "XR_KHR_composition_layer_depth",
                "XR_FB_composition_layer_depth_test",
                "XR_EXT_hand_tracking",
                "XR_EXT_hand_interaction"
            ]
        );
        assert!(ext("quest3").contains(&"XR_META_touch_controller_plus".to_string()));
        assert!(ext("reverbg2").contains(&"XR_EXT_hp_mixed_reality_controller".to_string()));
        assert!(ext("index").iter().any(|e| e == "XR_KHR_composition_layer_cylinder"), "drawn, so on");
        assert!(ext("index").iter().any(|e| e.contains("equirect")), "drawn, so on");
        assert!(ext("index").iter().any(|e| e.contains("cube")), "drawn, so on");
        assert!(!ext("index").iter().any(|e| e.contains("hand")), "an Index does not track hands");
        assert!(ext("quest3").contains(&"XR_EXT_hand_tracking".to_string()));
        assert!(ext("index").iter().any(|e| e.contains("depth")), "shown, so on");
    }

    #[test]
    fn extensions_parse_validate_and_round_trip() {
        let mut d = DeviceProfile::quest2();
        assert!(d.set("extensions", "XR_KHR_composition_layer_cylinder, XR_EXT_local_floor,XR_EXT_local_floor").is_ok());
        assert_eq!(d.extensions, vec!["XR_KHR_composition_layer_cylinder", "XR_EXT_local_floor"]);
        assert!(d.set("extensions", "XR_FB_passthrough").is_err(), "not implemented, so not offered");
        assert!(d.set("extensions", "").is_ok());
        assert!(d.extensions.is_empty());
        d.extensions = vec!["XR_KHR_composition_layer_cube".into()];
        let mut e = DeviceProfile::quest2();
        assert!(e.apply_ini(&d.to_ini()).is_empty());
        assert_eq!(e.extensions, d.extensions);
    }

    #[test]
    fn fov_signs_follow_openxr() {
        let mut d = DeviceProfile::quest2();
        assert!(d.set("fov_left", "40").is_err(), "left is negative in OpenXR");
        assert!(d.set("fov_down", "30").is_err());
        assert!(d.set("fov_up", "-30").is_err());
        assert!(d.set("fov_right", "45").is_ok());
    }

    #[test]
    fn the_play_area_round_trips_and_can_be_off() {
        let mut d = DeviceProfile::quest2();
        assert_eq!(d.play_area, [2.5, 2.5]);
        assert_eq!(DeviceProfile::preset_by_id("index").unwrap().play_area, [3.5, 3.0]);
        assert!(d.set("play_area_m", "4 x 3.25").is_ok());
        let mut e = DeviceProfile::quest2();
        assert!(e.apply_ini(&d.to_ini()).is_empty());
        assert_eq!(e.play_area, [4.0, 3.25]);
        assert!(d.set("play_area_m", "none").is_ok());
        assert!(d.to_ini().contains("play_area_m=0\n"));
        assert!(e.apply_ini(&d.to_ini()).is_empty());
        assert_eq!(e.play_area, [0.0, 0.0]);
        assert!(d.set("play_area_m", "0.5x3").is_err());
        assert!(d.set("play_area_m", "3").is_err());
    }
}
