//! The "headset" on Linux: no window. The simulated user's thread moves the
//! head and controllers; only the test hooks (`XRMIMIC_TEST_*`) drive it.

use crate::shared::Shared;
use crate::xlog;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use xrmimic_core::sim::SimState;

pub struct Window {
    shared: Arc<Shared>,
    user: Option<JoinHandle<()>>,
}

impl Window {
    pub fn spawn(_gfx: &crate::gfx::SessionGfx, sim: Arc<Mutex<SimState>>, shared: Arc<Shared>) -> Window {
        xlog!("headless headset (no display on this platform)");
        let user = crate::simthread::spawn(sim, shared.clone());
        Window { shared, user }
    }

    pub fn stop(mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.user.take() {
            let _ = t.join();
        }
    }
}
