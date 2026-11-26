//! XrTime is nanoseconds of QueryPerformanceCounter.

#[cfg(windows)]
pub fn qpc_frequency() -> i64 {
    use std::sync::OnceLock;
    static F: OnceLock<i64> = OnceLock::new();
    *F.get_or_init(|| {
        let mut f = 0i64;
        unsafe {
            let _ = windows::Win32::System::Performance::QueryPerformanceFrequency(&mut f);
        }
        f.max(1)
    })
}

#[cfg(windows)]
pub fn qpc_now() -> i64 {
    let mut c = 0i64;
    unsafe {
        let _ = windows::Win32::System::Performance::QueryPerformanceCounter(&mut c);
    }
    c
}

pub fn ticks_to_ns(ticks: i64, freq: i64) -> i64 {
    ((ticks as i128 * 1_000_000_000) / freq as i128) as i64
}

pub fn ns_to_ticks(ns: i64, freq: i64) -> i64 {
    ((ns as i128 * freq as i128) / 1_000_000_000) as i64
}

#[cfg(windows)]
pub fn now_ns() -> i64 {
    ticks_to_ns(qpc_now(), qpc_frequency())
}

/// CLOCK_MONOTONIC, the clock XR_KHR_convert_timespec_time maps XrTime to.
#[cfg(unix)]
pub fn now_ns() -> i64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    ts.tv_sec * 1_000_000_000 + ts.tv_nsec
}

/// Sleep until the monotonic clock reaches `target_ns` (coarse sleep, then spin).
pub fn sleep_until(target_ns: i64) {
    loop {
        let now = now_ns();
        let left = target_ns - now;
        if left <= 0 {
            return;
        }
        if left > 2_000_000 {
            std::thread::sleep(std::time::Duration::from_nanos((left - 1_500_000) as u64));
        } else {
            std::thread::yield_now();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tick_conversion_round_trips() {
        let f = 10_000_000; // typical QPC frequency
        for t in [0i64, 1, 123_456_789, 9_000_000_000_000] {
            assert_eq!(ns_to_ticks(ticks_to_ns(t, f), f), t);
        }
        assert_eq!(ticks_to_ns(10_000_000, f), 1_000_000_000);
    }
}
