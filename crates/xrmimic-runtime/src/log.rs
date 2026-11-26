//! Call log. Every entry point is counted; the first calls of each (and then
//! every 1000th) are written with their arguments to `XRMIMIC_LOG`, or to
//! `%TEMP%\xrmimic.log` when that is unset.

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

struct Logger {
    file: Option<File>,
    counts: HashMap<&'static str, u64>,
    start: Instant,
}

static LOGGER: LazyLock<Mutex<Logger>> = LazyLock::new(|| {
    let path = std::env::var_os("XRMIMIC_LOG").map(std::path::PathBuf::from).unwrap_or_else(|| {
        std::env::temp_dir().join("xrmimic.log")
    });
    let file = OpenOptions::new().create(true).append(true).open(&path).ok();
    let mut l = Logger { file, counts: HashMap::new(), start: Instant::now() };
    let exe = std::env::current_exe().map(|p| p.display().to_string()).unwrap_or_default();
    let line = format!(
        "---- xrmimic {} loaded in pid {} ({exe})",
        env!("CARGO_PKG_VERSION"),
        std::process::id()
    );
    l.write(&line);
    Mutex::new(l)
});

impl Logger {
    fn write(&mut self, line: &str) {
        if let Some(f) = self.file.as_mut() {
            let t = self.start.elapsed().as_secs_f64();
            let _ = writeln!(f, "[{t:10.4}] {line}");
            let _ = f.flush();
        }
    }
}

/// Count a call to `name`; returns whether this call should be logged in detail.
pub fn hit(name: &'static str) -> bool {
    let mut l = LOGGER.lock().unwrap_or_else(|e| e.into_inner());
    let c = l.counts.entry(name).or_insert(0);
    *c += 1;
    *c <= 8 || *c % 1000 == 0
}

pub fn line(s: &str) {
    let mut l = LOGGER.lock().unwrap_or_else(|e| e.into_inner());
    l.write(s);
}

/// Totals per entry point, sorted by name.
pub fn counts() -> Vec<(&'static str, u64)> {
    let l = LOGGER.lock().unwrap_or_else(|e| e.into_inner());
    let mut v: Vec<_> = l.counts.iter().map(|(k, v)| (*k, *v)).collect();
    v.sort();
    v
}

#[macro_export]
macro_rules! xlog {
    ($($arg:tt)*) => { $crate::log::line(&format!($($arg)*)) };
}

/// 32-bit top-down BMP from BGRA pixels.
pub fn write_bmp(path: &std::path::Path, w: u32, h: u32, bgra: &[u8]) -> std::io::Result<()> {
    let mut out = Vec::with_capacity(54 + bgra.len());
    let size = 54 + bgra.len() as u32;
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&size.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(w as i32).to_le_bytes());
    out.extend_from_slice(&(-(h as i32)).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]);
    out.extend_from_slice(bgra);
    std::fs::write(path, out)
}
