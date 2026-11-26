//! The headset window's Device tab: the headset xrmimic pretends to be.

use crate::devstore;
use std::cell::RefCell;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::HFONT;
use windows::Win32::UI::WindowsAndMessaging::*;
use xrmimic_core::device::{DeviceProfile, CONTROLLERS, OPTIONAL_EXTENSIONS};

const ID_PRESET: usize = 200;
const ID_CONTROLLERS: usize = 201;
const ID_APPLY: usize = 202;
const ID_REVERT: usize = 203;
const ID_FIELD0: usize = 210;
const ID_EXT0: usize = 240;
const BS_AUTOCHECKBOX: u32 = 0x3;
const BM_GETCHECK: u32 = 0xF0;
const BM_SETCHECK: u32 = 0xF1;

const CBS_DROPDOWNLIST: u32 = 0x0003;
const CB_ADDSTRING: u32 = 0x0143;
const CB_GETCURSEL: u32 = 0x0147;
const CB_SETCURSEL: u32 = 0x014E;
const CBN_SELCHANGE: u32 = 1;
const BN_CLICKED: u32 = 0;
const ES_AUTOHSCROLL: u32 = 0x80;

/// Text fields: (settings key, label).
const FIELDS: &[(&str, &str)] = &[
    ("name", "System name"),
    ("vendor_id", "Vendor ID"),
    ("eye_width", "Per-eye width (px)"),
    ("eye_height", "Per-eye height (px)"),
    ("fov_left", "FOV left (deg, negative)"),
    ("fov_right", "FOV right (deg)"),
    ("fov_up", "FOV up (deg)"),
    ("fov_down", "FOV down (deg, negative)"),
    ("ipd_mm", "IPD (mm)"),
    ("eye_height_m", "Eye height (m)"),
    ("refresh_rates", "Refresh rates (Hz, comma list)"),
    ("default_refresh", "Refresh rate used (Hz)"),
    ("play_area_m", "Play area (m, width x depth; 0 = none)"),
];

struct Controls {
    labels: Vec<HWND>,
    preset: HWND,
    fields: Vec<HWND>,
    controllers: HWND,
    ext_title: HWND,
    exts: Vec<HWND>,
    apply: HWND,
    revert: HWND,
    status: HWND,
    note: HWND,
}

thread_local! {
    static UI: RefCell<Option<Controls>> = const { RefCell::new(None) };
}

unsafe fn make(
    frame: HWND,
    hinst: HINSTANCE,
    class: PCWSTR,
    text: &str,
    style: u32,
    ex: WINDOW_EX_STYLE,
    id: usize,
    font: HFONT,
) -> windows::core::Result<HWND> {
    let t = HSTRING::from(text);
    let h = CreateWindowExW(
        ex,
        class,
        PCWSTR(t.as_ptr()),
        WS_CHILD | WINDOW_STYLE(style),
        0,
        0,
        10,
        10,
        Some(frame),
        Some(HMENU(id as _)),
        Some(hinst),
        None,
    )?;
    SendMessageW(h, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
    Ok(h)
}

unsafe fn set_text(h: HWND, s: &str) {
    let t = HSTRING::from(s);
    let _ = SetWindowTextW(h, PCWSTR(t.as_ptr()));
}

unsafe fn get_text(h: HWND) -> String {
    let mut buf = [0u16; 512];
    let n = GetWindowTextW(h, &mut buf) as usize;
    String::from_utf16_lossy(&buf[..n])
}

pub unsafe fn create(frame: HWND, hinst: HINSTANCE, font: HFONT) -> windows::core::Result<()> {
    let st = |text: &str| make(frame, hinst, w!("STATIC"), text, 0, WINDOW_EX_STYLE(0), 0, font);
    let mut labels = vec![st("Preset")?];
    let preset = make(
        frame,
        hinst,
        w!("COMBOBOX"),
        "",
        WS_TABSTOP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST,
        WINDOW_EX_STYLE(0),
        ID_PRESET,
        font,
    )?;
    for p in DeviceProfile::presets() {
        let t = HSTRING::from(format!("{} ({})", p.name, p.preset));
        SendMessageW(preset, CB_ADDSTRING, None, Some(LPARAM(t.as_ptr() as isize)));
    }
    let custom = HSTRING::from("Custom (edited values)");
    SendMessageW(preset, CB_ADDSTRING, None, Some(LPARAM(custom.as_ptr() as isize)));
    let mut fields = Vec::new();
    for (i, (_, label)) in FIELDS.iter().enumerate() {
        labels.push(st(label)?);
        fields.push(make(
            frame,
            hinst,
            w!("EDIT"),
            "",
            WS_TABSTOP.0 | WS_BORDER.0 | ES_AUTOHSCROLL,
            WINDOW_EX_STYLE(0),
            ID_FIELD0 + i,
            font,
        )?);
    }
    labels.push(st("Controllers (interaction profile)")?);
    let controllers = make(
        frame,
        hinst,
        w!("COMBOBOX"),
        "",
        WS_TABSTOP.0 | WS_VSCROLL.0 | CBS_DROPDOWNLIST,
        WINDOW_EX_STYLE(0),
        ID_CONTROLLERS,
        font,
    )?;
    for (path, label, _) in CONTROLLERS {
        let t = HSTRING::from(format!("{label}  ({path})"));
        SendMessageW(controllers, CB_ADDSTRING, None, Some(LPARAM(t.as_ptr() as isize)));
    }
    let ext_title = st("Advertised OpenXR extensions (take effect when Chrome restarts)")?;
    let mut exts = Vec::new();
    for (i, (name, _, what)) in OPTIONAL_EXTENSIONS.iter().enumerate() {
        exts.push(make(
            frame,
            hinst,
            w!("BUTTON"),
            &format!("{name}  ({what})"),
            WS_TABSTOP.0 | BS_AUTOCHECKBOX,
            WINDOW_EX_STYLE(0),
            ID_EXT0 + i,
            font,
        )?);
    }
    let apply = make(frame, hinst, w!("BUTTON"), "Apply", WS_TABSTOP.0, WINDOW_EX_STYLE(0), ID_APPLY, font)?;
    let revert = make(frame, hinst, w!("BUTTON"), "Revert", WS_TABSTOP.0, WINDOW_EX_STYLE(0), ID_REVERT, font)?;
    let status = st("")?;
    let note = st(
        "Name, size, refresh rate and controllers take effect at the next Enter VR; IPD and field of \
         view at once; extensions when Chrome restarts (Chrome keeps one OpenXR instance while it \
         runs). A WebXR page never sees the system name: it sees the controllers' profiles (Chrome \
         turns the name \"Oculus Quest2\" into oculus-touch-v3), the per-eye size, field of view, IPD, \
         frame rate, and the features the extensions allow.",
    )?;
    UI.with(|u| {
        *u.borrow_mut() =
            Some(Controls { labels, preset, fields, controllers, ext_title, exts, apply, revert, status, note })
    });
    load(&devstore::current());
    Ok(())
}

/// Put a device's values into the form.
unsafe fn load(d: &DeviceProfile) {
    UI.with(|u| {
        let u = u.borrow();
        let Some(c) = u.as_ref() else { return };
        let ini = d.to_ini();
        for (i, (key, _)) in FIELDS.iter().enumerate() {
            let v = ini
                .lines()
                .find_map(|l| l.strip_prefix(key).and_then(|r| r.strip_prefix('=')))
                .unwrap_or("")
                .to_string();
            set_text(c.fields[i], &v);
        }
        let presets = DeviceProfile::presets();
        let idx = presets.iter().position(|p| p == d).unwrap_or(presets.len());
        SendMessageW(c.preset, CB_SETCURSEL, Some(WPARAM(idx)), None);
        let ci = CONTROLLERS.iter().position(|(p, _, _)| *p == d.controllers).unwrap_or(0);
        SendMessageW(c.controllers, CB_SETCURSEL, Some(WPARAM(ci)), None);
        for (i, (name, _, _)) in OPTIONAL_EXTENSIONS.iter().enumerate() {
            let on = d.extensions.iter().any(|e| e == name);
            SendMessageW(c.exts[i], BM_SETCHECK, Some(WPARAM(on as usize)), None);
        }
    });
}

/// The Device page in the panel (x0.., `w` wide): the fields, the
/// controllers, Apply / Revert, then the extensions and the note.
pub unsafe fn layout(x0: i32, w: i32, _h: i32, toolbar: i32) {
    UI.with(|u| {
        let u = u.borrow();
        let Some(c) = u.as_ref() else { return };
        let (lx, cx, row) = (x0 + 12, x0 + 196, 26);
        let cw = (w - 208).max(80);
        let mut y = toolbar + 10;
        let _ = MoveWindow(c.labels[0], lx, y + 3, 180, 20, true);
        let _ = MoveWindow(c.preset, cx, y, cw, 300, true);
        y += row + 4;
        for (i, f) in c.fields.iter().enumerate() {
            let _ = MoveWindow(c.labels[i + 1], lx, y + 3, 180, 20, true);
            let _ = MoveWindow(*f, cx, y, cw, 22, true);
            y += row;
        }
        let _ = MoveWindow(c.labels[FIELDS.len() + 1], lx, y + 3, 180, 20, true);
        let _ = MoveWindow(c.controllers, cx, y, cw, 300, true);
        y += row + 4;
        let _ = MoveWindow(c.apply, cx, y, 90, 24, true);
        let _ = MoveWindow(c.revert, cx + 100, y, 90, 24, true);
        y += 28;
        let _ = MoveWindow(c.status, lx, y, (w - 24).max(50), 20, true);
        y += 26;
        let _ = MoveWindow(c.ext_title, lx, y, (w - 24).max(100), 20, true);
        y += 22;
        for h in &c.exts {
            let _ = MoveWindow(*h, lx, y, (w - 24).max(100), 20, true);
            y += 21;
        }
        y += 6;
        let _ = MoveWindow(c.note, lx, y, (w - 24).max(100), 80, true);
    });
}

pub unsafe fn show(on: bool) {
    UI.with(|u| {
        let u = u.borrow();
        let Some(c) = u.as_ref() else { return };
        let cmd = if on { SW_SHOW } else { SW_HIDE };
        for h in c.labels.iter().chain(c.fields.iter()) {
            let _ = ShowWindow(*h, cmd);
        }
        for h in c.exts.iter() {
            let _ = ShowWindow(*h, cmd);
        }
        for h in [c.preset, c.controllers, c.ext_title, c.apply, c.revert, c.status, c.note] {
            let _ = ShowWindow(h, cmd);
        }
    });
}

/// Read the form into a device on top of the current one; errors per field.
unsafe fn read_form() -> Result<DeviceProfile, Vec<String>> {
    let mut d = devstore::current();
    let mut errors = Vec::new();
    UI.with(|u| {
        let u = u.borrow();
        let Some(c) = u.as_ref() else { return };
        let presets = DeviceProfile::presets();
        let sel = SendMessageW(c.preset, CB_GETCURSEL, None, None).0;
        if sel >= 0 && (sel as usize) < presets.len() {
            d = presets[sel as usize].clone();
        } else {
            d.preset = "custom".into();
        }
        for (i, (key, label)) in FIELDS.iter().enumerate() {
            if let Err(e) = d.set(key, get_text(c.fields[i]).trim()) {
                errors.push(format!("{label}: {e}"));
            }
        }
        let ci = SendMessageW(c.controllers, CB_GETCURSEL, None, None).0;
        if ci >= 0 {
            d.controllers = CONTROLLERS[ci as usize].0.to_string();
        }
        d.extensions = OPTIONAL_EXTENSIONS
            .iter()
            .enumerate()
            .filter(|(i, _)| SendMessageW(c.exts[*i], BM_GETCHECK, None, None).0 == 1)
            .map(|(_, (name, _, _))| name.to_string())
            .collect();
        // Edited values that no longer match the chosen preset make it custom.
        if !presets.iter().any(|p| *p == d) {
            d.preset = "custom".into();
        }
    });
    if errors.is_empty() {
        Ok(d)
    } else {
        Err(errors)
    }
}

unsafe fn status(text: &str) {
    UI.with(|u| {
        if let Some(c) = u.borrow().as_ref() {
            set_text(c.status, text);
        }
    });
}

/// WM_COMMAND for the Device tab's controls; true if it was one of them.
pub unsafe fn on_command(id: usize, code: u32) -> bool {
    match (id, code) {
        (ID_PRESET, CBN_SELCHANGE) => {
            let sel = UI.with(|u| u.borrow().as_ref().map(|c| SendMessageW(c.preset, CB_GETCURSEL, None, None).0));
            let presets = DeviceProfile::presets();
            if let Some(p) = sel.filter(|s| *s >= 0).and_then(|s| presets.get(s as usize)) {
                load(p);
                status("Preset loaded into the form: Apply to use it.");
            }
            true
        }
        (ID_APPLY, BN_CLICKED) => {
            match read_form() {
                Ok(d) => {
                    let ext_changed = devstore::current().extensions != d.extensions;
                    let summary = format!(
                        "{} ({}x{} per eye){}",
                        d.name,
                        d.eye_width,
                        d.eye_height,
                        if ext_changed { "; extensions change after a Chrome restart" } else { "" }
                    );
                    match devstore::set(d.clone()) {
                        Ok(Some(p)) => status(&format!("Applied {summary}; saved to {}", p.display())),
                        Ok(None) => status(&format!("Applied {summary} (not saved: XRMIMIC_DEVICE_FILE is not set)")),
                        Err(e) => status(&format!("Applied {summary}, but saving failed: {e}")),
                    }
                    load(&d);
                }
                Err(errors) => status(&format!("Not applied: {}", errors.join("; "))),
            }
            true
        }
        (ID_REVERT, BN_CLICKED) => {
            load(&devstore::current());
            status("Form reset to the device in use.");
            true
        }
        (ID_PRESET | ID_CONTROLLERS | ID_APPLY | ID_REVERT, _) => true,
        (i, _) if (ID_FIELD0..ID_FIELD0 + FIELDS.len()).contains(&i) => true,
        (i, _) if (ID_EXT0..ID_EXT0 + OPTIONAL_EXTENSIONS.len()).contains(&i) => true,
        _ => false,
    }
}

/// Tests: fill the form from a preset id and apply it, as a user would.
pub unsafe fn apply_preset_for_test(id: &str) {
    let presets = DeviceProfile::presets();
    let Some(i) = presets.iter().position(|p| p.preset == id) else { return };
    UI.with(|u| {
        if let Some(c) = u.borrow().as_ref() {
            SendMessageW(c.preset, CB_SETCURSEL, Some(WPARAM(i)), None);
        }
    });
    on_command(ID_PRESET, CBN_SELCHANGE);
    on_command(ID_APPLY, BN_CLICKED);
}
