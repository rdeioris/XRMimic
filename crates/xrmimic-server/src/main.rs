//! xrmimic-server: the XRMimic headset (the simulated user, the window, the
//! control API) in a process of its own. Apps whose runtime is started with
//! XRMIMIC_SERVER=local use it over a named pipe (XRMIMIC_SERVER_PIPE, default
//! `xrmimic`), Chrome's sandboxed XR process included.

#[cfg(windows)]
fn main() {
    std::process::exit(xrmimic_runtime::server::run());
}

#[cfg(not(windows))]
fn main() {
    eprintln!("xrmimic-server runs on Windows only for now");
    std::process::exit(2);
}
