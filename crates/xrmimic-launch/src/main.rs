//! xrmimic-launch: helpers that run outside Chrome's sandbox.
//!
//!   xrmimic-launch serve <dir> <port>          serve <dir>'s files on http://127.0.0.1:<port>/
//!                                            (the XRMimic test page), until killed
//!   xrmimic-launch watch <dir> <pid> [<srv>]   Windows: serve runtime-switch requests from the
//!                                            headset window until process <pid> exits, then
//!                                            stop process <srv> (the page server) if given
//!   xrmimic-launch list                        Windows: print the runtimes the window lists

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("serve") if args.len() == 4 => {
            let dir = PathBuf::from(&args[2]);
            let port: u16 = args[3].parse().expect("port");
            if let Err(e) = serve(&dir, port) {
                eprintln!("xrmimic-launch serve: {e}");
                std::process::exit(1);
            }
        }
        #[cfg(windows)]
        Some("watch") if args.len() == 4 || args.len() == 5 => {
            let dir = PathBuf::from(&args[2]);
            let pid: u32 = args[3].parse().expect("pid");
            win::watch(&dir, pid);
            if let Some(server) = args.get(4).and_then(|s| s.parse().ok()) {
                win::stop(server);
            }
        }
        #[cfg(windows)]
        Some("list") => {
            for e in xrmimic_runtime::runtimes::list() {
                println!("{}", e.label());
            }
        }
        _ => {
            eprintln!("usage: xrmimic-launch serve <dir> <port> | watch <dir> <pid> [<server pid>] | list");
            std::process::exit(2);
        }
    }
}

// ---------------------------------------------------------------- page server

/// A minimal static file server for the test page: GET only, files under
/// `dir` only, bound to the loopback address (WebXR needs a secure context,
/// which http://localhost is).
fn serve(dir: &Path, port: u16) -> std::io::Result<()> {
    let root = dir.canonicalize()?;
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    println!("serving {} on http://localhost:{port}/", root.display());
    for stream in listener.incoming().flatten() {
        let root = root.clone();
        std::thread::spawn(move || {
            let _ = answer(stream, &root);
        });
    }
    Ok(())
}

fn content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|e| e.to_str()).unwrap_or("") {
        "html" => "text/html; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "json" => "application/json",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        _ => "application/octet-stream",
    }
}

/// Map a request path to a file under `root`, refusing anything outside it.
fn resolve(root: &Path, url_path: &str) -> Option<PathBuf> {
    let path = url_path.split(['?', '#']).next().unwrap_or("/");
    let rel = path.trim_start_matches('/');
    let rel = if rel.is_empty() { "index.html" } else { rel };
    if rel.split('/').any(|c| c == ".." || c.contains('\\') || c.contains(':')) {
        return None;
    }
    let p = root.join(rel).canonicalize().ok()?;
    (p.starts_with(root) && p.is_file()).then_some(p)
}

fn answer(stream: TcpStream, root: &Path) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut line = String::new();
    reader.read_line(&mut line)?;
    // Drain the headers.
    let mut h = String::new();
    while reader.read_line(&mut h)? > 2 {
        h.clear();
    }
    let mut out = stream;
    let mut parts = line.split_whitespace();
    let (method, target) = (parts.next().unwrap_or(""), parts.next().unwrap_or("/"));
    let reply = |out: &mut TcpStream, status: &str, ty: &str, body: &[u8]| -> std::io::Result<()> {
        write!(
            out,
            "HTTP/1.1 {status}\r\nContent-Type: {ty}\r\nContent-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n",
            body.len()
        )?;
        out.write_all(body)
    };
    if method != "GET" {
        return reply(&mut out, "405 Method Not Allowed", "text/plain", b"GET only\n");
    }
    match resolve(root, target).and_then(|p| std::fs::read(&p).ok().map(|b| (p, b))) {
        Some((p, body)) => reply(&mut out, "200 OK", content_type(&p), &body),
        None => reply(&mut out, "404 Not Found", "text/plain", b"not found\n"),
    }
}

// ---------------------------------------------------------------- Windows

#[cfg(windows)]
mod win {
    use std::path::Path;
    use std::time::Duration;
    use xrmimic_runtime::runtimes;

    /// The headset window (in Chrome's AppContainer) cannot ask for elevation;
    /// it drops `switch-request.txt` here and waits for `switch-result.txt`.
    pub fn watch(dir: &Path, pid: u32) {
        let request = dir.join(runtimes::REQUEST);
        let result = dir.join(runtimes::RESULT);
        while alive(pid) {
            if let Ok(manifest) = std::fs::read_to_string(&request) {
                let _ = std::fs::remove_file(&request);
                let manifest = manifest.trim().to_string();
                println!("switching the system OpenXR runtime to {manifest}");
                let answer = if std::env::var_os("XRMIMIC_SWITCH_DRYRUN").is_some() {
                    // Tests: answer without elevating or touching the registry.
                    println!("dry run: not changed");
                    "ok".to_string()
                } else {
                    match runtimes::set_active_elevated(&manifest) {
                        Ok(()) => "ok".to_string(),
                        Err(e) => e,
                    }
                };
                let tmp = dir.join("switch-result.tmp");
                if std::fs::write(&tmp, &answer).is_ok() {
                    let _ = std::fs::rename(&tmp, &result);
                }
            }
            std::thread::sleep(Duration::from_millis(250));
        }
    }

    fn alive(pid: u32) -> bool {
        use windows::Win32::Foundation::{CloseHandle, WAIT_TIMEOUT};
        use windows::Win32::System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE};
        unsafe {
            match OpenProcess(PROCESS_SYNCHRONIZE, false, pid) {
                Ok(h) => {
                    let running = WaitForSingleObject(h, 0) == WAIT_TIMEOUT;
                    let _ = CloseHandle(h);
                    running
                }
                Err(_) => false,
            }
        }
    }

    /// Stop a process we started (the page server), by PID.
    pub fn stop(pid: u32) {
        use windows::Win32::Foundation::CloseHandle;
        use windows::Win32::System::Threading::{OpenProcess, TerminateProcess, PROCESS_TERMINATE};
        unsafe {
            if let Ok(h) = OpenProcess(PROCESS_TERMINATE, false, pid) {
                let _ = TerminateProcess(h, 0);
                let _ = CloseHandle(h);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_stays_inside_the_folder() {
        let dir = std::env::temp_dir().join(format!("xrmimic-serve-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("index.html"), "x").unwrap();
        std::fs::write(dir.join("sub").join("a.js"), "y").unwrap();
        let root = dir.canonicalize().unwrap();
        assert!(resolve(&root, "/").unwrap().ends_with("index.html"));
        assert!(resolve(&root, "/sub/a.js?v=1").unwrap().ends_with("a.js"));
        assert!(resolve(&root, "/../secret").is_none());
        assert!(resolve(&root, "/sub/../../x").is_none());
        assert!(resolve(&root, "/c:/windows/win.ini").is_none());
        assert!(resolve(&root, "/missing.html").is_none());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
