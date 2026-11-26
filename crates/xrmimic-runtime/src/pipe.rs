//! Named pipes between the runtime in an app and `xrmimic-server` on the same
//! machine (Windows), and the messages they carry.
//!
//! A pipe is the one channel that leaves Chrome's sandbox (its XR process
//! runs in an AppContainer, which cannot reach localhost over TCP): the
//! server's pipe admits app containers and low-integrity processes. The
//! server also takes the app's shared texture and fence handles out of the
//! app's process (`DuplicateHandle`), so frames are never copied.
//!
//! Messages are JSON, each preceded by its length (u32, little-endian).

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::io::{Read, Write};
use xrmimic_core::device::DeviceProfile;
use xrmimic_core::haptics::Vibration;
use xrmimic_core::sim::SimState;

/// The server's pipe: `XRMIMIC_SERVER_PIPE`, or `xrmimic`.
pub fn pipe_path() -> String {
    let name = std::env::var("XRMIMIC_SERVER_PIPE").ok().filter(|n| !n.is_empty()).unwrap_or_else(|| "xrmimic".into());
    format!(r"\\.\pipe\{name}")
}

/// Bumped when a message changes shape.
pub const PROTOCOL: u32 = 1;

/// What a connection is for: requests and their replies, or the server's pushes.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    Requests,
    Pushes,
}

/// The first message on each connection.
#[derive(Serialize, Deserialize, Debug)]
pub struct Hello {
    pub protocol: u32,
    pub pid: u32,
    pub role: Role,
    /// Ties a process's two connections together.
    pub client: u64,
}

/// A session's simulated user and headset state, as the server last saw it.
/// `seq` orders snapshots from the push thread and the replies.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct UserState {
    pub seq: u64,
    pub user: SimState,
    pub hands: bool,
    pub close_requested: bool,
    pub depth_flip: bool,
}

/// Requests from the runtime in an app to the server.
#[derive(Serialize, Deserialize, Debug)]
pub enum Request {
    /// The device the server simulates.
    Device,
    /// A new session: its headset starts on the server.
    StartSession {
        app_name: String,
        hands: bool,
        depth_flip: bool,
        depth_flip_auto: bool,
        /// The app's graphics adapter (LUID low, high), for the window.
        luid: (u32, i32),
        /// The app's XRMIMIC_TEST_* variables: the window's test hooks.
        test_env: Vec<(String, String)>,
    },
    /// The next frame's start: waits while the server holds frames back.
    WaitFrame { session: u64, last_wake: i64, now: i64, on_grid: bool },
    /// A frame was copied into the shared textures (handle values in the app's process).
    Frame { session: u64, slot: crate::gfx::Slot, frame: u64, shown: u64 },
    /// The session's state (for the control API).
    State { session: u64, state: String, running: bool },
    Profile { session: u64, profile: String, bound: Vec<(String, String)> },
    Haptic { session: u64, hand: usize, vibration: Option<Vibration>, now: i64 },
    RequestClose { session: u64 },
    EndSession { session: u64 },
}

#[derive(Serialize, Deserialize, Debug)]
pub enum Reply {
    Device { device: DeviceProfile },
    Started { session: u64, period_ns: i64, stepped: bool },
    /// When the frame starts, and the user as of then (on the frame-stepped
    /// clock the pushes alone could be a frame behind).
    Wake { wake: i64, user: UserState },
    Haptic { was_playing: bool },
    Ok,
    Error { message: String },
}

/// The server's pushes to an app's runtime.
#[derive(Serialize, Deserialize, Debug)]
pub enum Push {
    /// A session's user and the headset's state, many times a second.
    User { session: u64, user: UserState },
    /// The device changed (the Device tab, the control API).
    Device { device: DeviceProfile },
}

pub fn send<T: Serialize>(w: &mut impl Write, msg: &T) -> std::io::Result<()> {
    let body = bincode::serialize(msg).map_err(std::io::Error::other)?;
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    w.write_all(&out)?;
    w.flush()
}

pub fn recv<T: DeserializeOwned>(r: &mut impl Read) -> std::io::Result<T> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let n = u32::from_le_bytes(len) as usize;
    if n > 64 << 20 {
        return Err(std::io::Error::other(format!("message of {n} bytes")));
    }
    let mut body = vec![0u8; n];
    r.read_exact(&mut body)?;
    bincode::deserialize(&body).map_err(std::io::Error::other)
}

/// Connect to the server's pipe (waits up to `timeout` for a free instance).
pub fn connect(timeout: std::time::Duration) -> std::io::Result<std::fs::File> {
    let path = pipe_path();
    let deadline = std::time::Instant::now() + timeout;
    loop {
        match std::fs::OpenOptions::new().read(true).write(true).open(&path) {
            Ok(f) => return Ok(f),
            // ERROR_PIPE_BUSY: every instance is taken; ERROR_FILE_NOT_FOUND:
            // the server is still starting. Try again shortly.
            Err(e) if matches!(e.raw_os_error(), Some(231 | 2)) && std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(e) => return Err(e),
        }
    }
}

#[cfg(windows)]
pub mod server {
    //! The server's end: pipe instances that admit sandboxed apps, and
    //! handles taken out of a client's process.

    use std::os::windows::io::FromRawHandle;
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::{CloseHandle, DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE, HLOCAL, LocalFree};
    use windows::Win32::Security::Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1};
    use windows::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
    use windows::Win32::Storage::FileSystem::{FILE_FLAGS_AND_ATTRIBUTES, PIPE_ACCESS_DUPLEX};
    use windows::Win32::System::Pipes::{
        ConnectNamedPipe, CreateNamedPipeW, GetNamedPipeClientProcessId, PIPE_READMODE_BYTE, PIPE_TYPE_BYTE,
        PIPE_UNLIMITED_INSTANCES, PIPE_WAIT,
    };
    use windows::Win32::System::Threading::{GetCurrentProcess, OpenProcess, PROCESS_DUP_HANDLE};

    /// Everyone and every app container may connect; low integrity may write.
    const SDDL: &str = "D:(A;;GA;;;WD)(A;;GA;;;AC)(A;;GA;;;S-1-15-2-2)S:(ML;;NW;;;LW)";

    /// Wait for a client on a new instance of the pipe; returns the
    /// connection and the client's process id.
    pub fn accept(path: &str) -> std::io::Result<(std::fs::File, u32)> {
        unsafe {
            let mut sd = PSECURITY_DESCRIPTOR::default();
            ConvertStringSecurityDescriptorToSecurityDescriptorW(&HSTRING::from(SDDL), SDDL_REVISION_1, &mut sd, None)
                .map_err(std::io::Error::other)?;
            let sa = SECURITY_ATTRIBUTES {
                nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32,
                lpSecurityDescriptor: sd.0,
                bInheritHandle: false.into(),
            };
            let name = HSTRING::from(path);
            let h = CreateNamedPipeW(
                PCWSTR(name.as_ptr()),
                FILE_FLAGS_AND_ATTRIBUTES(PIPE_ACCESS_DUPLEX.0),
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT,
                PIPE_UNLIMITED_INSTANCES,
                1 << 16,
                1 << 16,
                0,
                Some(&sa),
            );
            let _ = LocalFree(Some(HLOCAL(sd.0)));
            if h.is_invalid() {
                return Err(std::io::Error::last_os_error());
            }
            // ERROR_PIPE_CONNECTED (535): the client was quicker than us; fine.
            if let Err(e) = ConnectNamedPipe(h, None) {
                if e.code().0 as u32 != 0x8007_0217 {
                    let _ = CloseHandle(h);
                    return Err(std::io::Error::other(e));
                }
            }
            let mut pid = 0u32;
            let _ = GetNamedPipeClientProcessId(h, &mut pid);
            Ok((std::fs::File::from_raw_handle(h.0), pid))
        }
    }

    /// A process to take handles from.
    pub struct ClientProcess(HANDLE);

    unsafe impl Send for ClientProcess {}
    unsafe impl Sync for ClientProcess {}

    impl ClientProcess {
        pub fn open(pid: u32) -> std::io::Result<ClientProcess> {
            unsafe { OpenProcess(PROCESS_DUP_HANDLE, false, pid).map(ClientProcess).map_err(std::io::Error::other) }
        }

        /// The client's handle value `h` as a handle of this process.
        pub fn take(&self, h: usize) -> std::io::Result<usize> {
            unsafe {
                let mut out = HANDLE::default();
                DuplicateHandle(self.0, HANDLE(h as _), GetCurrentProcess(), &mut out, 0, false, DUPLICATE_SAME_ACCESS)
                    .map_err(std::io::Error::other)?;
                Ok(out.0 as usize)
            }
        }
    }

    impl Drop for ClientProcess {
        fn drop(&mut self) {
            unsafe {
                let _ = CloseHandle(self.0);
            }
        }
    }

    pub fn close(h: usize) {
        unsafe {
            let _ = CloseHandle(HANDLE(h as _));
        }
    }
}
