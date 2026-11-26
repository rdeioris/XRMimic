//! The headset window's Input tab: every input of the controllers' current
//! interaction profile, one row per input with the left and the right hand's
//! controls side by side, to press, touch, pull and move them by hand.
//! Buttons are checkboxes, triggers and squeezes sliders, sticks and
//! trackpads a pad to drag a dot in (right-click or double-click centres it).
//! What the tab sets is held until changed (or "Release"), and combines with
//! the keyboard and gamepad (the larger value / any press wins). Each row
//! shows the app's actions bound to that input and what the app reads now.
//! "Hands" (or the H key) puts the controllers down: the session reports the
//! app's hand interaction profile and hand joints, and the rows become the
//! pinch, the grasp, each finger's curl and whether the hand is in view.

use crate::shared::Shared;
use crate::xlog;
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use windows::core::{w, HSTRING, PCWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::*;
use windows::Win32::UI::Controls::{InitCommonControlsEx, ICC_BAR_CLASSES, INITCOMMONCONTROLSEX};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetCapture, ReleaseCapture, SetCapture};
use windows::Win32::UI::WindowsAndMessaging::*;
use std::sync::atomic::Ordering;
use xrmimic_core::bindings::{hand_inputs, inputs, read, resolve, write, InputRow, Kind, Source, Value};
use xrmimic_core::sim::{ControllerInput, SimState, LEFT, RIGHT};

const ID_RELEASE: usize = 390;
const ID_HANDS: usize = 392;
const ID_CELL0: usize = 400;
const BS_AUTOCHECKBOX: u32 = 0x3;
const BM_GETCHECK: u32 = 0xF0;
const BM_SETCHECK: u32 = 0xF1;
const BN_CLICKED: u32 = 0;
const TBS_NOTICKS: u32 = 0x10;
const TBM_GETPOS: u32 = 0x0400;
const TBM_SETPOS: u32 = 0x0405;
const TBM_SETRANGEMIN: u32 = 0x0407;
const TBM_SETRANGEMAX: u32 = 0x0408;
const TB_ENDTRACK: usize = 8;
const PAD: i32 = 64;

/// One hand's control for a row.
struct Cell {
    input: InputRow,
    /// A checkbox, a slider (0..100) or a pad.
    control: HWND,
    value: HWND,
}

/// One input of the profile: its name, the app's actions on it, and a cell
/// per hand that has it.
struct Row {
    kind: Kind,
    source: Source,
    label: HWND,
    actions: HWND,
    cells: [Option<Cell>; 2],
}

struct Controls {
    frame: HWND,
    hinst: HINSTANCE,
    font: HFONT,
    title: HWND,
    hands_box: HWND,
    heads: [HWND; 2],
    release: [HWND; 2],
    /// Each hand's haptics: what plays, or the last one and how many.
    haptic: [HWND; 2],
    note: HWND,
    rows: Vec<Row>,
    /// The profile the rows were built for, whether the app had attached,
    /// and whether they are for tracked hands.
    profile: String,
    attached: bool,
    hands: bool,
    visible: bool,
    /// Panel origin and size: (x0, width, height, toolbar).
    area: (i32, i32, i32, i32),
}

thread_local! {
    static UI: RefCell<Option<Controls>> = const { RefCell::new(None) };
    static SHARED: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
    /// What the tab holds on each hand.
    static HELD: RefCell<[ControllerInput; 2]> = RefCell::new([ControllerInput::default(); 2]);
    /// Each pad's position, x and y in -1..1.
    static PADS: RefCell<HashMap<isize, [f32; 2]>> = RefCell::new(HashMap::new());
    static VALUES_AT: RefCell<Option<Instant>> = const { RefCell::new(None) };
    /// Tests: inputs to set once the app's profile is known (XRMIMIC_TEST_INPUT).
    static PENDING_TEST: RefCell<Option<String>> = const { RefCell::new(None) };
}

unsafe fn make(c: &Controls, class: PCWSTR, text: &str, style: u32, id: usize) -> windows::core::Result<HWND> {
    let t = HSTRING::from(text);
    let h = CreateWindowExW(
        WINDOW_EX_STYLE(0),
        class,
        PCWSTR(t.as_ptr()),
        WS_CHILD | WINDOW_STYLE(style) | if c.visible { WS_VISIBLE } else { WINDOW_STYLE(0) },
        0,
        0,
        10,
        10,
        Some(c.frame),
        Some(HMENU(id as _)),
        Some(c.hinst),
        None,
    )?;
    SendMessageW(h, WM_SETFONT, Some(WPARAM(c.font.0 as usize)), Some(LPARAM(1)));
    Ok(h)
}

unsafe fn set_text(h: HWND, s: &str) {
    let t = HSTRING::from(s);
    let _ = SetWindowTextW(h, PCWSTR(t.as_ptr()));
}

// ---------------------------------------------------------------- the pad

/// Paint a pad: a circle, its cross, and the dot where the stick is.
unsafe fn paint_pad(hwnd: HWND) {
    let mut ps = PAINTSTRUCT::default();
    let dc = BeginPaint(hwnd, &mut ps);
    let mut r = RECT::default();
    let _ = GetClientRect(hwnd, &mut r);
    let bg = CreateSolidBrush(COLORREF(0x00F4F4F4));
    FillRect(dc, &r, bg);
    let _ = DeleteObject(bg.into());
    let (w, h) = (r.right, r.bottom);
    let pen = CreatePen(PS_SOLID, 1, COLORREF(0x00909090));
    let old_pen = SelectObject(dc, pen.into());
    let old_brush = SelectObject(dc, GetStockObject(NULL_BRUSH));
    let _ = Ellipse(dc, 1, 1, w - 1, h - 1);
    let _ = MoveToEx(dc, w / 2, 3, None);
    let _ = LineTo(dc, w / 2, h - 3);
    let _ = MoveToEx(dc, 3, h / 2, None);
    let _ = LineTo(dc, w - 3, h / 2);
    let v = PADS.with(|p| p.borrow().get(&(hwnd.0 as isize)).copied().unwrap_or([0.0; 2]));
    let (cx, cy) = (w as f32 / 2.0 + v[0] * (w as f32 / 2.0 - 5.0), h as f32 / 2.0 - v[1] * (h as f32 / 2.0 - 5.0));
    let dot = CreateSolidBrush(COLORREF(0x00D07020));
    SelectObject(dc, dot.into());
    let _ = Ellipse(dc, cx as i32 - 5, cy as i32 - 5, cx as i32 + 5, cy as i32 + 5);
    SelectObject(dc, old_brush);
    SelectObject(dc, old_pen);
    let _ = DeleteObject(dot.into());
    let _ = DeleteObject(pen.into());
    let _ = EndPaint(hwnd, &ps);
}

/// Put a pad's dot at `v` (kept inside the circle) and tell the row.
unsafe fn set_pad(hwnd: HWND, v: [f32; 2]) {
    let m = v[0].hypot(v[1]);
    let v = if m > 1.0 { [v[0] / m, v[1] / m] } else { v };
    PADS.with(|p| p.borrow_mut().insert(hwnd.0 as isize, v));
    let _ = InvalidateRect(Some(hwnd), None, true);
    pad_changed(hwnd);
}

unsafe fn pad_from_mouse(hwnd: HWND, lp: LPARAM) {
    let (x, y) = ((lp.0 & 0xffff) as i16 as f32, ((lp.0 >> 16) & 0xffff) as i16 as f32);
    let mut r = RECT::default();
    let _ = GetClientRect(hwnd, &mut r);
    let (hw, hh) = (r.right as f32 / 2.0 - 5.0, r.bottom as f32 / 2.0 - 5.0);
    set_pad(hwnd, [(x - r.right as f32 / 2.0) / hw, -(y - r.bottom as f32 / 2.0) / hh]);
}

unsafe extern "system" fn pad_proc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_PAINT => {
            paint_pad(hwnd);
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            SetCapture(hwnd);
            pad_from_mouse(hwnd, lp);
            LRESULT(0)
        }
        WM_MOUSEMOVE if GetCapture() == hwnd => {
            pad_from_mouse(hwnd, lp);
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            let _ = ReleaseCapture();
            LRESULT(0)
        }
        WM_RBUTTONDOWN | WM_LBUTTONDBLCLK => {
            set_pad(hwnd, [0.0, 0.0]);
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

// ---------------------------------------------------------------- the tab

pub unsafe fn create(frame: HWND, hinst: HINSTANCE, font: HFONT, shared: Arc<Shared>) -> windows::core::Result<()> {
    let icc = INITCOMMONCONTROLSEX { dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_BAR_CLASSES };
    let _ = InitCommonControlsEx(&icc);
    let class = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        style: CS_DBLCLKS | CS_HREDRAW | CS_VREDRAW,
        lpfnWndProc: Some(pad_proc),
        hInstance: hinst,
        hCursor: LoadCursorW(None, IDC_HAND)?,
        lpszClassName: w!("xrmimic-pad"),
        ..Default::default()
    };
    RegisterClassExW(&class);
    SHARED.with(|s| *s.borrow_mut() = Some(shared));
    PENDING_TEST.with(|p| *p.borrow_mut() = std::env::var("XRMIMIC_TEST_INPUT").ok());
    let mut c = Controls {
        frame,
        hinst,
        font,
        title: HWND::default(),
        hands_box: HWND::default(),
        heads: [HWND::default(); 2],
        release: [HWND::default(); 2],
        haptic: [HWND::default(); 2],
        note: HWND::default(),
        rows: Vec::new(),
        profile: String::new(),
        attached: false,
        hands: false,
        visible: false,
        area: (0, 600, 802, 34),
    };
    c.title = make(&c, w!("STATIC"), "", 0, 0)?;
    c.hands_box = make(&c, w!("BUTTON"), "Hands (put the controllers down; H)", WS_TABSTOP.0 | BS_AUTOCHECKBOX, ID_HANDS)?;
    let on = SHARED.with(|s| s.borrow().as_ref().is_some_and(|s| s.hands.load(Ordering::SeqCst)));
    SendMessageW(c.hands_box, BM_SETCHECK, Some(WPARAM(on as usize)), None);
    c.heads = [make(&c, w!("STATIC"), "Left", 0, 0)?, make(&c, w!("STATIC"), "Right", 0, 0)?];
    c.release = [
        make(&c, w!("BUTTON"), "Release", WS_TABSTOP.0, ID_RELEASE)?,
        make(&c, w!("BUTTON"), "Release", WS_TABSTOP.0, ID_RELEASE + 1)?,
    ];
    c.haptic = [make(&c, w!("STATIC"), "", 0, 0)?, make(&c, w!("STATIC"), "", 0, 0)?];
    c.note = make(
        &c,
        w!("STATIC"),
        "Held until changed, combined with the keyboard and gamepad. Pads: drag the dot; right-click or \
         double-click centres it. Under each input: the app's actions bound to it.",
        0,
        0,
    )?;
    UI.with(|u| *u.borrow_mut() = Some(c));
    Ok(())
}

/// What to show: the profile the app's session reports (before any app
/// attached: the device's controllers), whether it attached, and whether
/// hands are in use.
fn current_profile() -> (String, bool, bool) {
    let (p, attached, hands) = SHARED
        .with(|s| {
            s.borrow().as_ref().map(|s| {
                (s.profile.lock().unwrap().clone(), s.attached.load(Ordering::SeqCst), s.hands.load(Ordering::SeqCst))
            })
        })
        .unwrap_or_default();
    if !attached && !hands {
        (crate::devstore::current().controllers, false, false)
    } else {
        (p, attached, hands)
    }
}

/// Put the controllers down or pick them up (the checkbox and the H key).
pub unsafe fn toggle_hands() {
    let on = SHARED.with(|s| {
        s.borrow().as_ref().map(|s| !s.hands.fetch_xor(true, Ordering::SeqCst)).unwrap_or(false)
    });
    UI.with(|u| {
        if let Some(c) = u.borrow().as_ref() {
            SendMessageW(c.hands_box, BM_SETCHECK, Some(WPARAM(on as usize)), None);
        }
    });
}

/// A binding's source belongs to a row: the same source, or a component of
/// that row's stick or trackpad.
fn same_input(bound: Source, row: Source) -> bool {
    use Source::*;
    bound == row
        || (row == Thumbstick && matches!(bound, ThumbstickX | ThumbstickY))
        || (row == Trackpad && matches!(bound, TrackpadX | TrackpadY))
}

/// Destroy the rows and build them for `profile` (tracked `hands` or
/// controllers); the tab's held values reset.
unsafe fn rebuild(profile: &str, attached: bool, hands: bool) {
    HELD.with(|h| *h.borrow_mut() = [ControllerInput::default(); 2]);
    publish();
    PADS.with(|p| p.borrow_mut().clear());
    let bound = SHARED.with(|s| s.borrow().as_ref().map(|s| s.bound.lock().unwrap().clone())).unwrap_or_default();
    UI.with(|u| {
        let mut u = u.borrow_mut();
        let Some(c) = u.as_mut() else { return };
        for r in c.rows.drain(..) {
            let _ = DestroyWindow(r.label);
            let _ = DestroyWindow(r.actions);
            for cell in r.cells.iter().flatten() {
                let _ = DestroyWindow(cell.control);
                let _ = DestroyWindow(cell.value);
            }
        }
        let hand_profile = xrmimic_core::device::HAND_PROFILES.iter().find(|(p, _, _)| *p == profile);
        let title = if hands {
            match hand_profile {
                Some((p, label, _)) => format!("Tracked hands: {label}\n{p}"),
                None if attached => "Tracked hands: joints only\n(the app suggested no hand interaction profile)".to_string(),
                None => "Tracked hands\n(no app has attached yet)".to_string(),
            }
        } else {
            let name = xrmimic_core::device::CONTROLLERS
                .iter()
                .find(|(p, _, _)| *p == profile)
                .map(|(_, l, _)| *l)
                .unwrap_or("unknown controller");
            format!("{name}\n{profile}{}", if attached { "" } else { "  (the device's; no app has attached yet)" })
        };
        set_text(c.title, &title);
        // One row per input, in the left hand's order, then what only the right has.
        let per_hand = if hands {
            let p = hand_profile.map(|(p, _, _)| *p).unwrap_or("");
            [hand_inputs(p, LEFT), hand_inputs(p, RIGHT)]
        } else {
            [inputs(profile, LEFT), inputs(profile, RIGHT)]
        };
        let mut sources: Vec<(Source, Kind)> = Vec::new();
        for list in &per_hand {
            for r in list {
                if !sources.iter().any(|(s, _)| *s == r.source) {
                    sources.push((r.source, r.kind));
                }
            }
        }
        let mut id = ID_CELL0;
        for (source, kind) in sources {
            let ins: [Option<&InputRow>; 2] = [0, 1].map(|h| per_hand[h].iter().find(|r| r.source == source));
            let names: [Option<String>; 2] = ins.map(|i| i.map(|i| i.paths.join(", ")));
            let label = match (&names[0], &names[1]) {
                (Some(l), Some(r)) if l == r => l.clone(),
                (Some(l), Some(r)) => format!("{l}  |  {r}"),
                (Some(l), None) => format!("{l}  (left)"),
                (None, Some(r)) => format!("{r}  (right)"),
                (None, None) => continue,
            };
            // The app's actions on this input, per hand.
            let acts: [Vec<String>; 2] = [LEFT, RIGHT].map(|hand| {
                let side = if hand == LEFT { "left" } else { "right" };
                let mut v: Vec<String> = bound
                    .iter()
                    .filter(|(path, _)| {
                        path.starts_with(&format!("/user/hand/{side}/"))
                            && resolve(profile, path).is_some_and(|(_, s)| same_input(s, source))
                    })
                    .map(|(_, a)| a.clone())
                    .collect();
                v.dedup();
                v
            });
            let actions = match (acts[0].is_empty(), acts[1].is_empty()) {
                (true, true) => String::new(),
                _ if acts[0] == acts[1] => format!("-> {}", acts[0].join(", ")),
                _ => format!("-> L: {}  R: {}", acts[0].join(", "), acts[1].join(", ")),
            };
            let Ok(label_h) = make(c, w!("STATIC"), &label, 0, 0) else { continue };
            let actions_h = make(c, w!("STATIC"), &actions, 0, 0).unwrap_or_default();
            let mut cells: [Option<Cell>; 2] = [None, None];
            for hand in [LEFT, RIGHT] {
                let Some(input) = ins[hand] else { continue };
                let control = match kind {
                    Kind::Button => make(c, w!("BUTTON"), "held", WS_TABSTOP.0 | BS_AUTOCHECKBOX, id + hand),
                    Kind::Axis => make(c, w!("msctls_trackbar32"), "", WS_TABSTOP.0 | TBS_NOTICKS, id + hand).inspect(|s| {
                        SendMessageW(*s, TBM_SETRANGEMIN, Some(WPARAM(0)), Some(LPARAM(0)));
                        SendMessageW(*s, TBM_SETRANGEMAX, Some(WPARAM(1)), Some(LPARAM(100)));
                    }),
                    Kind::Stick => make(c, w!("xrmimic-pad"), "", 0, id + hand),
                };
                let Ok(control) = control else { continue };
                let value = make(c, w!("STATIC"), "", 0, 0).unwrap_or_default();
                cells[hand] = Some(Cell { input: input.clone(), control, value });
            }
            c.rows.push(Row { kind, source, label: label_h, actions: actions_h, cells });
            id += 2;
        }
        c.profile = profile.to_string();
        c.attached = attached;
        c.hands = hands;
    });
    let (x0, w, h, t) = UI.with(|u| u.borrow().as_ref().map(|c| c.area).unwrap_or((0, 600, 802, 34)));
    layout(x0, w, h, t);
}

/// Lay the tab out in the panel (x0.., `w` wide).
pub unsafe fn layout(x0: i32, w: i32, h: i32, toolbar: i32) {
    UI.with(|u| {
        let mut u = u.borrow_mut();
        let Some(c) = u.as_mut() else { return };
        c.area = (x0, w, h, toolbar);
        let top = toolbar + 8;
        let _ = MoveWindow(c.title, x0 + 10, top, (w - 250).max(50), 32, true);
        let _ = MoveWindow(c.hands_box, x0 + w - 236, top, 228, 20, true);
        // Columns: the input's name, then the left hand's cell, then the right's.
        let cell_w = ((w - 20) * 30 / 100).clamp(150, 200);
        let (lx, rx) = (x0 + w - 10 - 2 * cell_w - 10, x0 + w - 10 - cell_w);
        let name_w = (lx - x0 - 16).max(80);
        let y = top + 38;
        for (hand, x) in [(LEFT, lx), (RIGHT, rx)] {
            let _ = MoveWindow(c.release[hand], x, y, 70, 22, true);
            let _ = MoveWindow(c.heads[hand], x + 76, y + 4, cell_w - 76, 18, true);
            let _ = MoveWindow(c.haptic[hand], x, y + 26, cell_w, 34, true);
        }
        let mut ry = y + 64;
        for r in &c.rows {
            let row_h = if r.kind == Kind::Stick { PAD + 8 } else { 40 };
            let _ = MoveWindow(r.label, x0 + 10, ry + 2, name_w, 18, true);
            let _ = MoveWindow(r.actions, x0 + 18, ry + 20, name_w - 8, 18, true);
            for (hand, x) in [(LEFT, lx), (RIGHT, rx)] {
                let Some(cell) = &r.cells[hand] else { continue };
                match r.kind {
                    Kind::Button => {
                        let _ = MoveWindow(cell.control, x, ry + 2, 60, 20, true);
                        let _ = MoveWindow(cell.value, x + 66, ry + 4, cell_w - 70, 18, true);
                    }
                    Kind::Axis => {
                        let _ = MoveWindow(cell.control, x - 4, ry, cell_w - 56, 26, true);
                        let _ = MoveWindow(cell.value, x + cell_w - 54, ry + 4, 50, 18, true);
                    }
                    Kind::Stick => {
                        let _ = MoveWindow(cell.control, x, ry, PAD, PAD, true);
                        let _ = MoveWindow(cell.value, x + PAD + 6, ry + PAD / 2 - 18, cell_w - PAD - 8, 36, true);
                    }
                }
            }
            ry += row_h;
        }
        let _ = MoveWindow(c.note, x0 + 10, (ry + 6).min(h - 40), (w - 20).max(50), 36, true);
    });
}

pub unsafe fn show(on: bool) {
    UI.with(|u| {
        let mut u = u.borrow_mut();
        let Some(c) = u.as_mut() else { return };
        c.visible = on;
        let cmd = if on { SW_SHOW } else { SW_HIDE };
        let mut all =
            vec![c.title, c.hands_box, c.heads[0], c.heads[1], c.release[0], c.release[1], c.haptic[0], c.haptic[1], c.note];
        for r in &c.rows {
            all.extend([r.label, r.actions]);
            for cell in r.cells.iter().flatten() {
                all.extend([cell.control, cell.value]);
            }
        }
        for h in all {
            let _ = ShowWindow(h, cmd);
        }
    });
}

/// Hand the held values to the simulated user.
fn publish() {
    let held = HELD.with(|h| *h.borrow());
    SHARED.with(|s| {
        if let Some(s) = s.borrow().as_ref() {
            *s.tab_input.lock().unwrap() = held;
        }
    });
}

/// Read one cell's control into the held values.
unsafe fn apply_cell(r: &Row, hand: usize) {
    let Some(cell) = &r.cells[hand] else { return };
    let v = match r.kind {
        Kind::Button => Value::Bool(SendMessageW(cell.control, BM_GETCHECK, None, None).0 == 1),
        Kind::Axis => Value::Float(SendMessageW(cell.control, TBM_GETPOS, None, None).0 as f32 / 100.0),
        Kind::Stick => Value::Vec2(PADS.with(|p| p.borrow().get(&(cell.control.0 as isize)).copied().unwrap_or([0.0; 2]))),
    };
    HELD.with(|h| write(&mut h.borrow_mut()[hand], cell.input.source, v));
    publish();
}

/// Find the row and hand of a control.
fn find(c: &Controls, control: HWND) -> Option<(usize, usize)> {
    c.rows.iter().enumerate().find_map(|(i, r)| {
        (0..2).find(|h| r.cells[*h].as_ref().is_some_and(|cell| cell.control == control)).map(|h| (i, h))
    })
}

unsafe fn pad_changed(pad: HWND) {
    UI.with(|u| {
        let u = u.borrow();
        let Some(c) = u.as_ref() else { return };
        if let Some((i, h)) = find(c, pad) {
            apply_cell(&c.rows[i], h);
        }
    });
}

/// WM_HSCROLL from one of the tab's sliders; true if it was one.
pub unsafe fn on_scroll(slider: HWND, code: usize) -> bool {
    let found = UI.with(|u| {
        let u = u.borrow();
        let Some(c) = u.as_ref() else { return false };
        let Some((i, h)) = find(c, slider) else { return false };
        apply_cell(&c.rows[i], h);
        true
    });
    if found && code == TB_ENDTRACK {
        crate::window::focus_view();
    }
    found
}

/// WM_COMMAND for the tab's buttons; true if it was one of them.
pub unsafe fn on_command(id: usize, code: u32) -> bool {
    if id == ID_HANDS {
        if code == BN_CLICKED {
            let on = UI.with(|u| u.borrow().as_ref().is_some_and(|c| SendMessageW(c.hands_box, BM_GETCHECK, None, None).0 == 1));
            SHARED.with(|s| {
                if let Some(s) = s.borrow().as_ref() {
                    s.hands.store(on, Ordering::SeqCst);
                }
            });
            crate::window::focus_view();
        }
        return true;
    }
    if id == ID_RELEASE || id == ID_RELEASE + 1 {
        if code == BN_CLICKED {
            release(id - ID_RELEASE);
            crate::window::focus_view();
        }
        return true;
    }
    if id < ID_CELL0 {
        return false;
    }
    let found = UI.with(|u| {
        let u = u.borrow();
        let Some(c) = u.as_ref() else { return false };
        let (i, hand) = ((id - ID_CELL0) / 2, (id - ID_CELL0) % 2);
        let Some(r) = c.rows.get(i) else { return false };
        if code == BN_CLICKED && r.kind == Kind::Button {
            apply_cell(r, hand);
        }
        true
    });
    if found && code == BN_CLICKED {
        crate::window::focus_view();
    }
    found
}

/// Let go of everything the tab holds on one hand.
unsafe fn release(hand: usize) {
    HELD.with(|h| h.borrow_mut()[hand] = ControllerInput::default());
    publish();
    let pads: Vec<HWND> = UI.with(|u| {
        let u = u.borrow();
        let Some(c) = u.as_ref() else { return Vec::new() };
        let mut pads = Vec::new();
        for r in &c.rows {
            let Some(cell) = &r.cells[hand] else { continue };
            match r.kind {
                Kind::Button => {
                    SendMessageW(cell.control, BM_SETCHECK, Some(WPARAM(0)), None);
                }
                Kind::Axis => {
                    SendMessageW(cell.control, TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(0)));
                }
                Kind::Stick => pads.push(cell.control),
            }
        }
        pads
    });
    for p in pads {
        PADS.with(|m| m.borrow_mut().insert(p.0 as isize, [0.0; 2]));
        let _ = InvalidateRect(Some(p), None, true);
    }
}

/// Called every window loop turn: rebuild when the profile changes, refresh
/// what the app reads while the tab shows.
pub unsafe fn tick(sim: &Mutex<SimState>) {
    let (profile, attached, hands) = current_profile();
    let stale = UI.with(|u| {
        u.borrow().as_ref().is_some_and(|c| c.profile != profile || c.attached != attached || c.hands != hands)
    });
    if stale {
        rebuild(&profile, attached, hands);
    }
    if attached {
        if let Some(t) = PENDING_TEST.with(|p| p.borrow_mut().take()) {
            apply_test_input(&t);
        }
    }
    let visible = UI.with(|u| u.borrow().as_ref().is_some_and(|c| c.visible));
    let due = VALUES_AT.with(|v| v.borrow().is_none_or(|t| t.elapsed().as_millis() >= 100));
    if !visible || !due {
        return;
    }
    VALUES_AT.with(|v| *v.borrow_mut() = Some(Instant::now()));
    let (controllers, placed) = {
        let s = sim.lock().unwrap();
        (s.controllers, s.placed.map(|p| p.is_some()))
    };
    let haptics = SHARED.with(|s| s.borrow().as_ref().map(|s| *s.haptics.lock().unwrap()));
    let now = crate::time::now_ns();
    UI.with(|u| {
        let u = u.borrow();
        let Some(c) = u.as_ref() else { return };
        for hand in [LEFT, RIGHT] {
            let name = if hand == LEFT { "Left" } else { "Right" };
            set_text(c.heads[hand], &if placed[hand] { format!("{name} (put down; R)") } else { name.to_string() });
            let h = haptics.map(|h| h[hand]).unwrap_or_default();
            let text = match (h.amplitude(now) > 0.0, h.last) {
                (true, Some((v, _))) => format!("Haptic: buzzing {}", v.describe()),
                (false, Some((v, _))) => format!("Haptic: idle; last {}
{} sent", v.describe(), h.count),
                (_, None) => "Haptic: none sent".to_string(),
            };
            set_text(c.haptic[hand], &text);
        }
        for r in &c.rows {
            for hand in [LEFT, RIGHT] {
                let Some(cell) = &r.cells[hand] else { continue };
                let text = match (r.kind, read(&controllers[hand], r.source)) {
                    (Kind::Button, v) => if v.as_bool() { "pressed" } else { "-" }.to_string(),
                    (_, Value::Vec2(v)) => format!("x {:.2}\ny {:.2}", v[0], v[1]),
                    (_, v) => format!("{:.2}", v.as_float()),
                };
                set_text(cell.value, &text);
            }
        }
    });
}

/// Tests: "right trigger/value=0.7; right a/click=1; left thumbstick=0.5,-0.25",
/// set through the controls as clicks and drags would.
unsafe fn apply_test_input(spec: &str) {
    for item in spec.split(';').map(str::trim).filter(|s| !s.is_empty()) {
        let Some((lhs, value)) = item.split_once('=') else { continue };
        let Some((side, path)) = lhs.trim().split_once(' ') else { continue };
        let hand = if side == "left" { LEFT } else { RIGHT };
        let nums: Vec<f32> = value.split(',').filter_map(|v| v.trim().parse().ok()).collect();
        let num = |k: usize| nums.get(k).copied().unwrap_or(0.0);
        // Find the cell and set its control (a pad after the borrow: it calls back in).
        let pad = UI.with(|u| {
            let u = u.borrow();
            let c = u.as_ref()?;
            let r = c
                .rows
                .iter()
                .find(|r| r.cells[hand].as_ref().is_some_and(|cell| cell.input.paths.contains(&path.trim())))?;
            let cell = r.cells[hand].as_ref()?;
            match r.kind {
                Kind::Button => {
                    SendMessageW(cell.control, BM_SETCHECK, Some(WPARAM((num(0) > 0.5) as usize)), None);
                    apply_cell(r, hand);
                    Some(None)
                }
                Kind::Axis => {
                    let p = (num(0) * 100.0).round() as isize;
                    SendMessageW(cell.control, TBM_SETPOS, Some(WPARAM(1)), Some(LPARAM(p)));
                    apply_cell(r, hand);
                    Some(None)
                }
                Kind::Stick => Some(Some(cell.control)),
            }
        });
        match pad {
            None => xlog!("XRMIMIC_TEST_INPUT: no {side} input {path} in this profile"),
            Some(p) => {
                if let Some(p) = p {
                    set_pad(p, [num(0), num(1)]);
                }
                xlog!("XRMIMIC_TEST_INPUT: set {side} {path} = {value}");
            }
        }
    }
}
