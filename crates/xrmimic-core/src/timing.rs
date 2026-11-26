//! Frame pacing in nanoseconds of a monotonic clock (XrTime).

#[derive(Clone, Copy, Debug)]
pub struct FrameClock {
    pub period_ns: i64,
    /// Time of the vsync all others are multiples of.
    pub epoch_ns: i64,
}

impl FrameClock {
    pub fn new(hz: f32, epoch_ns: i64) -> FrameClock {
        FrameClock { period_ns: (1e9 / hz as f64).round() as i64, epoch_ns }
    }

    /// First vsync strictly after `now`.
    pub fn next_vsync(&self, now: i64) -> i64 {
        let since = now - self.epoch_ns;
        let n = since.div_euclid(self.period_ns) + 1;
        self.epoch_ns + n * self.period_ns
    }

    /// The vsync xrWaitFrame should wake at, given the last one it returned.
    /// Skips ahead when the app missed frames, never returns the same one twice.
    pub fn wake_after(&self, last_wake: i64, now: i64) -> i64 {
        let next = self.next_vsync(now.max(last_wake));
        if next <= last_wake {
            last_wake + self.period_ns
        } else {
            next
        }
    }

    /// Free-running pacing: at most one frame per period, but a late app is
    /// never pushed to the next vsync boundary (there is no real display).
    pub fn wake_free(&self, last_wake: i64, now: i64) -> i64 {
        (last_wake + self.period_ns).max(now)
    }

    /// Predicted display time for a frame whose xrWaitFrame woke at `wake`:
    /// one period of render latency later.
    pub fn display_time(&self, wake: i64) -> i64 {
        wake + self.period_ns
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ninety_hz_period() {
        assert_eq!(FrameClock::new(90.0, 0).period_ns, 11_111_111);
        assert_eq!(FrameClock::new(72.0, 0).period_ns, 13_888_889);
    }

    #[test]
    fn next_vsync_is_strictly_later_and_aligned() {
        let c = FrameClock::new(100.0, 1_000);
        assert_eq!(c.next_vsync(1_000), 1_000 + 10_000_000);
        assert_eq!(c.next_vsync(1_001), 1_000 + 10_000_000);
        assert_eq!(c.next_vsync(10_001_000), 20_001_000);
        // Before the epoch still aligns.
        assert_eq!(c.next_vsync(-5_000_000), 1_000);
    }

    #[test]
    fn free_running_keeps_the_period_and_never_waits_when_late() {
        let c = FrameClock::new(100.0, 0);
        assert_eq!(c.wake_free(0, 1_000), 10_000_000, "early: wait out the period");
        assert_eq!(c.wake_free(10_000_000, 23_000_000), 23_000_000, "late: go now, not at 30 ms");
    }

    #[test]
    fn wake_never_repeats_and_skips_missed_frames() {
        let c = FrameClock::new(100.0, 0);
        let w1 = c.wake_after(0, 1);
        assert_eq!(w1, 10_000_000);
        // Called again immediately: next frame, not the same one.
        assert_eq!(c.wake_after(w1, w1), 20_000_000);
        // The app stalled for 3.5 frames: wake at the next boundary after now.
        assert_eq!(c.wake_after(w1, 45_000_000), 50_000_000);
        assert_eq!(c.display_time(50_000_000), 60_000_000);
    }
}
