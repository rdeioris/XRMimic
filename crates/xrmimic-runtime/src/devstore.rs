//! The device the runtime pretends to be, shared by the OpenXR side and the
//! headset window's Device tab, and saved between runs.
//!
//! Start-up: `XRMIMIC_DEVICE_FILE` (if it exists) on top of Quest 2, then
//! `XRMIMIC_DEVICE=<preset>` if set (tests and quick switches).

use crate::xlog;
use std::sync::{LazyLock, RwLock};
use xrmimic_core::device::DeviceProfile;

static DEVICE: LazyLock<RwLock<DeviceProfile>> = LazyLock::new(|| {
    let mut d = DeviceProfile::quest2();
    if let Some(path) = file() {
        if let Ok(text) = std::fs::read_to_string(&path) {
            for p in d.apply_ini(&text) {
                xlog!("{}: {p}", path.display());
            }
        }
    }
    if let Ok(id) = std::env::var("XRMIMIC_DEVICE") {
        match DeviceProfile::preset_by_id(&id) {
            Some(p) => d = p,
            None => xlog!("XRMIMIC_DEVICE: unknown preset {id:?}"),
        }
    }
    xlog!("device: {} ({}), {}x{} per eye, controllers {}", d.name, d.preset, d.eye_width, d.eye_height, d.controllers);
    RwLock::new(d)
});

fn file() -> Option<std::path::PathBuf> {
    std::env::var_os("XRMIMIC_DEVICE_FILE").map(std::path::PathBuf::from)
}

pub fn current() -> DeviceProfile {
    DEVICE.read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Replace the device and save it; returns where it was saved (if anywhere).
/// (The Device tab's Apply; Linux has no window and only reads the file.)
#[cfg_attr(not(windows), allow(dead_code))]
pub fn set(d: DeviceProfile) -> Result<Option<std::path::PathBuf>, String> {
    xlog!("device changed: {} ({}), {}x{} per eye, controllers {}", d.name, d.preset, d.eye_width, d.eye_height, d.controllers);
    let text = d.to_ini();
    *DEVICE.write().unwrap_or_else(|e| e.into_inner()) = d;
    match file() {
        Some(p) => std::fs::write(&p, text).map(|()| Some(p.clone())).map_err(|e| format!("{}: {e}", p.display())),
        None => Ok(None),
    }
}
