//! The OpenXR runtimes installed on this machine, and switching the system's
//! active one (HKLM\SOFTWARE\Khronos\OpenXR\1\ActiveRuntime).

pub const KEY: &str = r"SOFTWARE\Khronos\OpenXR\1";

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeEntry {
    pub name: String,
    pub manifest: String,
    pub active: bool,
}

impl RuntimeEntry {
    pub fn label(&self) -> String {
        format!("{}{}  ({})", if self.active { "\u{25CF} " } else { "" }, self.name, self.manifest)
    }
}

/// `runtime.name` from a manifest's JSON, if present.
pub fn manifest_name(json: &str) -> Option<String> {
    let rt = &json[json.find("\"runtime\"")?..];
    let after = &rt[rt.find("\"name\"")? + 6..];
    let after = &after[after.find(':')? + 1..];
    let start = after.find('"')? + 1;
    let end = start + after[start..].find('"')?;
    Some(after[start..end].to_string())
}

/// SteamVR's runtime folder from openvrpaths.vrpath (`"runtime": ["C:\\...\\SteamVR"]`).
pub fn steamvr_dir(vrpath_json: &str) -> Option<String> {
    let rt = &vrpath_json[vrpath_json.find("\"runtime\"")?..];
    let start = rt.find('[')? + 1;
    let first = &rt[start..];
    let q1 = first.find('"')? + 1;
    let q2 = q1 + first[q1..].find('"')?;
    Some(first[q1..q2].replace("\\\\", "\\"))
}

/// Merge candidate manifest paths into a list without duplicates, active first.
pub fn build_list(candidates: &[String], active: Option<&str>, read: impl Fn(&str) -> Option<String>) -> Vec<RuntimeEntry> {
    let norm = |p: &str| p.replace('/', "\\").to_ascii_lowercase();
    let mut out: Vec<RuntimeEntry> = Vec::new();
    for c in active.into_iter().map(str::to_string).chain(candidates.iter().cloned()) {
        if c.is_empty() || out.iter().any(|e| norm(&e.manifest) == norm(&c)) {
            continue;
        }
        let name = read(&c).and_then(|j| manifest_name(&j)).unwrap_or_else(|| {
            std::path::Path::new(&c).file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or(c.clone())
        });
        let is_active = active.is_some_and(|a| norm(a) == norm(&c));
        out.push(RuntimeEntry { name, manifest: c, active: is_active });
    }
    out
}

#[cfg(windows)]
mod win {
    use super::*;
    use crate::xlog;
    use windows::core::{HSTRING, PCWSTR};
    use windows::Win32::Foundation::*;
    use windows::Win32::System::Registry::*;
    use windows::Win32::System::Threading::{GetExitCodeProcess, WaitForSingleObject, INFINITE};
    use windows::Win32::UI::Shell::{ShellExecuteExW, SEE_MASK_NOCLOSEPROCESS, SHELLEXECUTEINFOW};
    use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;

    fn open_key() -> Option<HKEY> {
        let mut k = HKEY::default();
        let r = unsafe {
            RegOpenKeyExW(HKEY_LOCAL_MACHINE, &HSTRING::from(KEY), Some(0), KEY_READ | KEY_WOW64_64KEY, &mut k)
        };
        (r == ERROR_SUCCESS).then_some(k)
    }

    pub fn active_runtime() -> Option<String> {
        let mut buf = vec![0u16; 1024];
        let mut size = (buf.len() * 2) as u32;
        let r = unsafe {
            RegGetValueW(
                HKEY_LOCAL_MACHINE,
                &HSTRING::from(KEY),
                &HSTRING::from("ActiveRuntime"),
                RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_SUBKEY_WOW6464KEY,
                None,
                Some(buf.as_mut_ptr() as *mut _),
                Some(&mut size),
            )
        };
        if r != ERROR_SUCCESS {
            return None;
        }
        let n = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..n]))
    }

    fn available_runtimes() -> Vec<String> {
        let Some(root) = open_key() else { return Vec::new() };
        let mut sub = HKEY::default();
        let mut out = Vec::new();
        unsafe {
            if RegOpenKeyExW(root, &HSTRING::from("AvailableRuntimes"), Some(0), KEY_READ | KEY_WOW64_64KEY, &mut sub)
                == ERROR_SUCCESS
            {
                for i in 0.. {
                    let mut name = vec![0u16; 1024];
                    let mut len = name.len() as u32;
                    let mut data = 0u32;
                    let mut dlen = 4u32;
                    let r = RegEnumValueW(
                        sub,
                        i,
                        Some(windows::core::PWSTR(name.as_mut_ptr())),
                        &mut len,
                        None,
                        None,
                        Some(&mut data as *mut u32 as *mut u8),
                        Some(&mut dlen),
                    );
                    if r != ERROR_SUCCESS {
                        break;
                    }
                    // A value of 0 means enabled.
                    if data == 0 {
                        out.push(String::from_utf16_lossy(&name[..len as usize]));
                    }
                }
                let _ = RegCloseKey(sub);
            }
            let _ = RegCloseKey(root);
        }
        out
    }

    /// Every runtime manifest we can find, the system's active one marked.
    pub fn list() -> Vec<RuntimeEntry> {
        let mut candidates = available_runtimes();
        if let Some(local) = std::env::var_os("LOCALAPPDATA") {
            let vrpath = std::path::Path::new(&local).join("openvr").join("openvrpaths.vrpath");
            if let Some(dir) = std::fs::read_to_string(vrpath).ok().and_then(|j| steamvr_dir(&j)) {
                let m = std::path::Path::new(&dir).join("steamxr_win64.json");
                if m.exists() {
                    candidates.push(m.display().to_string());
                }
            }
        }
        // Runtimes the launcher found outside Chrome's sandbox (which cannot
        // read the user's AppData), separated by ';'.
        if let Ok(extra) = std::env::var("XRMIMIC_EXTRA_RUNTIMES") {
            candidates.extend(extra.split(';').filter(|s| !s.is_empty()).map(String::from));
        }
        if let Some(home) = std::env::var_os("XRMIMIC_HOME") {
            let m = std::path::Path::new(&home).join("xrmimic.json");
            if m.exists() {
                candidates.push(m.display().to_string());
            }
        }
        let active = active_runtime();
        build_list(&candidates, active.as_deref(), |p| std::fs::read_to_string(p).ok())
    }

    pub const REQUEST: &str = "switch-request.txt";
    pub const RESULT: &str = "switch-result.txt";

    /// Make `manifest` the system's active runtime. Inside Chrome's sandbox
    /// the request goes to the launcher's helper (`XRMIMIC_SWITCH_DIR`), which
    /// runs outside it; otherwise it is done here. Blocks until it finishes.
    pub fn set_active(manifest: &str) -> Result<(), String> {
        let Some(dir) = std::env::var_os("XRMIMIC_SWITCH_DIR").map(std::path::PathBuf::from) else {
            return set_active_elevated(manifest);
        };
        let _ = std::fs::remove_file(dir.join(RESULT));
        let tmp = dir.join("switch-request.tmp");
        std::fs::write(&tmp, manifest).map_err(|e| format!("cannot write request: {e}"))?;
        std::fs::rename(&tmp, dir.join(REQUEST)).map_err(|e| format!("cannot post request: {e}"))?;
        let start = std::time::Instant::now();
        while start.elapsed().as_secs() < 600 {
            if let Ok(r) = std::fs::read_to_string(dir.join(RESULT)) {
                let _ = std::fs::remove_file(dir.join(RESULT));
                return if r.trim() == "ok" { Ok(()) } else { Err(r.trim().to_string()) };
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
        let _ = std::fs::remove_file(dir.join(REQUEST));
        Err("the launcher's helper did not answer (is xrmimic-launch watch running?)".into())
    }

    /// An elevated `reg add`, so Windows asks the user (UAC) every time.
    pub fn set_active_elevated(manifest: &str) -> Result<(), String> {
        let params = format!(r#"add "HKLM\{KEY}" /v ActiveRuntime /t REG_EXPAND_SZ /d "{manifest}" /f /reg:64"#);
        let verb = HSTRING::from("runas");
        let file = HSTRING::from("reg.exe");
        let params_w = HSTRING::from(params.as_str());
        let mut info = SHELLEXECUTEINFOW {
            cbSize: std::mem::size_of::<SHELLEXECUTEINFOW>() as u32,
            fMask: SEE_MASK_NOCLOSEPROCESS,
            lpVerb: PCWSTR(verb.as_ptr()),
            lpFile: PCWSTR(file.as_ptr()),
            lpParameters: PCWSTR(params_w.as_ptr()),
            nShow: SW_HIDE.0,
            ..Default::default()
        };
        let previous = active_runtime();
        unsafe {
            ShellExecuteExW(&mut info).map_err(|e| format!("not switched: {e}"))?;
            let mut code = 1u32;
            if !info.hProcess.is_invalid() {
                WaitForSingleObject(info.hProcess, INFINITE);
                let _ = GetExitCodeProcess(info.hProcess, &mut code);
                let _ = CloseHandle(info.hProcess);
            }
            if code != 0 {
                return Err(format!("reg.exe exited with {code}"));
            }
        }
        xlog!("ActiveRuntime changed: {previous:?} -> {manifest}");
        Ok(())
    }
}

#[cfg(windows)]
pub use win::{list, set_active, set_active_elevated, REQUEST, RESULT};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_runtime_name() {
        let j = r#"{ "file_format_version": "1.0.0", "runtime": { "name": "Oculus", "library_path": "x.dll" } }"#;
        assert_eq!(manifest_name(j).as_deref(), Some("Oculus"));
        assert_eq!(manifest_name(r#"{"runtime":{"library_path":"x.dll"}}"#), None);
    }

    #[test]
    fn finds_steamvr_folder() {
        let j = r#"{ "config": ["C:\\Steam\\config"], "runtime": [ "C:\\Program Files (x86)\\Steam\\steamapps\\common\\SteamVR" ], "version": 1 }"#;
        assert_eq!(steamvr_dir(j).as_deref(), Some(r"C:\Program Files (x86)\Steam\steamapps\common\SteamVR"));
    }

    #[test]
    fn list_dedupes_and_marks_active_first() {
        let c = vec![r"C:\a\meta.json".to_string(), r"E:\xrmimic\xrmimic.json".to_string(), r"c:/A/META.json".to_string()];
        let l = build_list(&c, Some(r"E:\xrmimic\xrmimic.json"), |p| {
            Some(format!(r#"{{"runtime":{{"name":"{}"}}}}"#, if p.contains("meta") || p.contains("META") { "Meta" } else { "XRMimic" }))
        });
        assert_eq!(l.len(), 2);
        assert!(l[0].active && l[0].name == "XRMimic");
        assert!(!l[1].active && l[1].name == "Meta");
        assert!(l[0].label().starts_with('\u{25CF}'));
    }
}
