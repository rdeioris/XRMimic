//! The "headset" on Linux: no window. The simulated user's thread (started by
//! crate::headset) moves the head and controllers; the test hooks
//! (`XRMIMIC_TEST_*`) and the control API drive it.

use crate::shared::Shared;
use crate::xlog;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use xrmimic_core::sim::SimState;

pub struct Window {
    shared: Arc<Shared>,
}

impl Window {
    pub fn spawn(_luid: (u32, i32), _sim: Arc<Mutex<SimState>>, shared: Arc<Shared>) -> Window {
        xlog!("headless headset (no display on this platform)");
        Window { shared }
    }

    pub fn stop(self) {
        self.shared.stop.store(true, Ordering::SeqCst);
    }
}
