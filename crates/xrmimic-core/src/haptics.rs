//! Haptic feedback the app sends a controller (xrApplyHapticFeedback): what
//! each hand is told to do, kept so the headset window can show it and a
//! gamepad can rumble with it.

/// XR_MIN_HAPTIC_DURATION: the shortest pulse the runtime can make.
pub const MIN_HAPTIC_DURATION: i64 = -1;
/// XR_INFINITE_DURATION: until stopped.
pub const INFINITE_DURATION: i64 = i64::MAX;
/// The shortest pulse xrmimic makes (ns).
pub const SHORTEST_NS: i64 = 10_000_000;
/// How long the window keeps showing a pulse at least (ns), so a 10 ms one
/// is still seen.
pub const SHOWN_AT_LEAST_NS: i64 = 120_000_000;

/// One XrHapticVibration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Vibration {
    /// 0..1.
    pub amplitude: f32,
    /// Hz; 0 = unspecified (XR_FREQUENCY_UNSPECIFIED).
    pub frequency: f32,
    /// ns; None = until stopped.
    pub duration_ns: Option<i64>,
}

impl Vibration {
    /// From XrHapticVibration's fields: the minimum duration becomes the
    /// shortest pulse, the infinite one no end, the amplitude 0..1.
    pub fn from_xr(duration: i64, frequency: f32, amplitude: f32) -> Vibration {
        let duration_ns = match duration {
            INFINITE_DURATION => None,
            d if d <= 0 || d < SHORTEST_NS => Some(SHORTEST_NS),
            d => Some(d),
        };
        let amplitude = if amplitude.is_finite() { amplitude.clamp(0.0, 1.0) } else { 0.0 };
        let frequency = if frequency.is_finite() { frequency.max(0.0) } else { 0.0 };
        Vibration { amplitude, frequency, duration_ns }
    }

    /// "0.80 at 160 Hz for 100 ms".
    pub fn describe(&self) -> String {
        let f = if self.frequency > 0.0 { format!("{:.0} Hz", self.frequency) } else { "any Hz".to_string() };
        let d = match self.duration_ns {
            None => "until stopped".to_string(),
            Some(ns) => format!("for {:.0} ms", ns as f64 / 1e6),
        };
        format!("{:.2} at {f} {d}", self.amplitude)
    }
}

/// One hand's haptics.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct HapticState {
    /// The vibration playing and when it started (ns), until it ends or is stopped.
    pub playing: Option<(Vibration, i64)>,
    /// The last vibration sent and when, kept after it ends.
    pub last: Option<(Vibration, i64)>,
    /// Vibrations sent so far.
    pub count: u64,
}

impl HapticState {
    /// A new vibration replaces the one playing.
    pub fn apply(&mut self, v: Vibration, now: i64) {
        self.playing = Some((v, now));
        self.last = Some((v, now));
        self.count += 1;
    }

    pub fn stop(&mut self, now: i64) {
        // The last vibration, if it is the one playing, played until now.
        if let (Some((_, at)), Some(last)) = (self.playing, self.last.as_mut()) {
            if last.1 == at {
                last.0.duration_ns = Some((now - at).max(0));
            }
        }
        self.playing = None;
    }

    /// The amplitude playing at `now` (0 when nothing is).
    pub fn amplitude(&self, now: i64) -> f32 {
        match self.playing {
            Some((v, at)) if v.duration_ns.is_none_or(|d| now - at < d) => v.amplitude,
            _ => 0.0,
        }
    }

    /// What the window shows at `now`: the amplitude playing, or the last
    /// one for a moment after a short pulse ends (but not after a stop).
    pub fn shown(&self, now: i64) -> f32 {
        let a = self.amplitude(now);
        if a > 0.0 {
            return a;
        }
        match (self.playing, self.last) {
            (Some((v, at)), _) if now - at < SHOWN_AT_LEAST_NS => v.amplitude,
            _ => 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations_amplitudes_and_frequencies_are_read_as_the_spec_says() {
        let v = Vibration::from_xr(MIN_HAPTIC_DURATION, 0.0, 0.5);
        assert_eq!(v.duration_ns, Some(SHORTEST_NS));
        assert_eq!(Vibration::from_xr(INFINITE_DURATION, 160.0, 2.0), Vibration {
            amplitude: 1.0,
            frequency: 160.0,
            duration_ns: None
        });
        assert_eq!(Vibration::from_xr(100_000_000, -3.0, f32::NAN).amplitude, 0.0);
        assert_eq!(Vibration::from_xr(100_000_000, -3.0, 0.3).frequency, 0.0);
        assert_eq!(Vibration::from_xr(1, 0.0, 0.3).duration_ns, Some(SHORTEST_NS), "shorter than we can");
        assert_eq!(Vibration::from_xr(100_000_000, 160.0, 0.8).describe(), "0.80 at 160 Hz for 100 ms");
    }

    #[test]
    fn a_pulse_plays_for_its_duration_and_shows_a_little_longer() {
        let mut h = HapticState::default();
        h.apply(Vibration::from_xr(MIN_HAPTIC_DURATION, 0.0, 0.6), 1_000);
        assert_eq!(h.amplitude(1_000 + SHORTEST_NS - 1), 0.6);
        assert_eq!(h.amplitude(1_000 + SHORTEST_NS), 0.0);
        assert_eq!(h.shown(1_000 + SHORTEST_NS), 0.6, "a 10 ms pulse stays visible");
        assert_eq!(h.shown(1_000 + SHOWN_AT_LEAST_NS), 0.0);
        assert_eq!(h.count, 1);
    }

    #[test]
    fn stop_ends_an_endless_one_and_a_new_one_replaces_it() {
        let mut h = HapticState::default();
        h.apply(Vibration::from_xr(INFINITE_DURATION, 0.0, 0.4), 0);
        assert_eq!(h.amplitude(10_000_000_000), 0.4);
        h.apply(Vibration::from_xr(INFINITE_DURATION, 0.0, 0.9), 5);
        assert_eq!(h.amplitude(6), 0.9);
        h.stop(1_000_000);
        assert_eq!((h.amplitude(1_000_001), h.shown(1_000_001)), (0.0, 0.0));
        assert_eq!(h.last.unwrap().0.duration_ns, Some(1_000_000 - 5), "what played");
        assert_eq!(h.count, 2);
    }
}
