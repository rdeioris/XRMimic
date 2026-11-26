//! The headset window's Logs tab: every OpenXR call the runtime received,
//! newest last, in a virtual list (it only formats the rows on screen).

use crate::trace::{self, Entry};
use std::cell::RefCell;
use std::sync::Arc;
use windows::core::{w, HSTRING, PCWSTR, PWSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Gdi::HFONT;
use windows::Win32::UI::Controls::*;
use windows::Win32::UI::WindowsAndMessaging::*;

pub const ID_TABS: usize = 101;
const ID_FILTER: usize = 102;
const ID_LIVE: usize = 103;
const ID_CLEAR: usize = 104;
const ID_HIDE_FRAME: usize = 105;
const ID_LIST: usize = 106;

const TCM_INSERTITEMW: u32 = 0x133E;
const TCM_GETCURSEL: u32 = 0x130B;
const TCM_SETCURSEL: u32 = 0x130C;
const TCN_SELCHANGE: u32 = (-551i32) as u32;
const LVM_INSERTCOLUMNW: u32 = 0x1061;
const LVM_SETITEMCOUNT: u32 = 0x102F;
const LVM_ENSUREVISIBLE: u32 = 0x1013;
const LVM_SETEXTENDEDLISTVIEWSTYLE: u32 = 0x1036;
const LVM_GETNEXTITEM: u32 = 0x100C;
const LVM_REDRAWITEMS: u32 = 0x1015;
const LVN_GETDISPINFOW: u32 = (-177i32) as u32;
const NM_DBLCLK: u32 = (-3i32) as u32;
const LVS_REPORT: u32 = 0x1;
const LVS_SINGLESEL: u32 = 0x4;
const LVS_SHOWSELALWAYS: u32 = 0x8;
const LVS_OWNERDATA: u32 = 0x1000;
const LVS_EX_FULLROWSELECT: usize = 0x20;
const LVS_EX_DOUBLEBUFFER: usize = 0x10000;
const LVSICF_NOINVALIDATEALL: usize = 0x1;
const LVSICF_NOSCROLL: usize = 0x2;
const BS_AUTOCHECKBOX: u32 = 0x3;
const BM_GETCHECK: u32 = 0xF0;
const BM_SETCHECK: u32 = 0xF1;
const ES_AUTOHSCROLL: u32 = 0x80;
const EN_CHANGE: u32 = 0x300;
const BN_CLICKED: u32 = 0;

#[derive(Clone, Copy)]
struct Controls {
    tabs: HWND,
    label: HWND,
    filter: HWND,
    live: HWND,
    hide_frame: HWND,
    clear: HWND,
    count: HWND,
    list: HWND,
}

#[derive(Default)]
struct Data {
    /// The rows shown: matching entries, oldest first.
    rows: Vec<Arc<Entry>>,
    last_seq: u64,
    /// Lower-case filter; a row matches if its name, arguments or outputs contain it.
    filter: String,
    live: bool,
    hide_frame: bool,
    shown_count: usize,
}

thread_local! {
    static UI: RefCell<Option<Controls>> = const { RefCell::new(None) };
    static DATA: RefCell<Data> = RefCell::new(Data { live: true, ..Default::default() });
}

fn ui() -> Option<Controls> {
    UI.with(|u| *u.borrow())
}

unsafe fn child(
    frame: HWND,
    hinst: HINSTANCE,
    class: PCWSTR,
    text: &str,
    style: u32,
    id: usize,
    font: HFONT,
) -> windows::core::Result<HWND> {
    let t = HSTRING::from(text);
    let h = CreateWindowExW(
        WINDOW_EX_STYLE(0),
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

/// The tab strip (visible) and the Logs page's controls (hidden until chosen).
pub unsafe fn create(frame: HWND, hinst: HINSTANCE, font: HFONT) -> windows::core::Result<()> {
    let icc = INITCOMMONCONTROLSEX {
        dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32,
        dwICC: ICC_LISTVIEW_CLASSES | ICC_TAB_CLASSES,
    };
    let _ = InitCommonControlsEx(&icc);
    let tabs = child(frame, hinst, w!("SysTabControl32"), "", WS_VISIBLE.0 | WS_CLIPSIBLINGS.0, ID_TABS, font)?;
    for (i, name) in ["Logs", "Device", "Input"].iter().enumerate() {
        let mut text: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let item = TCITEMW { mask: TCIF_TEXT, pszText: PWSTR(text.as_mut_ptr()), ..Default::default() };
        SendMessageW(tabs, TCM_INSERTITEMW, Some(WPARAM(i)), Some(LPARAM(&item as *const _ as isize)));
    }
    let label = child(frame, hinst, w!("STATIC"), "Filter:", 0, 0, font)?;
    let filter = child(frame, hinst, w!("EDIT"), "", WS_TABSTOP.0 | WS_BORDER.0 | ES_AUTOHSCROLL, ID_FILTER, font)?;
    let live = child(frame, hinst, w!("BUTTON"), "Live (follow new calls)", WS_TABSTOP.0 | BS_AUTOCHECKBOX, ID_LIVE, font)?;
    SendMessageW(live, BM_SETCHECK, Some(WPARAM(1)), None);
    let hide_frame =
        child(frame, hinst, w!("BUTTON"), "Hide per-frame calls", WS_TABSTOP.0 | BS_AUTOCHECKBOX, ID_HIDE_FRAME, font)?;
    let clear = child(frame, hinst, w!("BUTTON"), "Clear", WS_TABSTOP.0, ID_CLEAR, font)?;
    let count = child(frame, hinst, w!("STATIC"), "", 0, 0, font)?;
    let list = CreateWindowExW(
        WS_EX_CLIENTEDGE,
        w!("SysListView32"),
        None,
        WS_CHILD | WS_TABSTOP | WINDOW_STYLE(LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS | LVS_OWNERDATA),
        0,
        0,
        10,
        10,
        Some(frame),
        Some(HMENU(ID_LIST as _)),
        Some(hinst),
        None,
    )?;
    SendMessageW(list, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
    let ex = LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER;
    SendMessageW(list, LVM_SETEXTENDEDLISTVIEWSTYLE, Some(WPARAM(ex)), Some(LPARAM(ex as isize)));
    for (i, (name, width)) in
        [("#", 70), ("ms", 90), ("thread", 50), ("call", 230), ("result", 190), ("arguments => outputs", 1400)]
            .iter()
            .enumerate()
    {
        let mut text: Vec<u16> = name.encode_utf16().chain(Some(0)).collect();
        let col = LVCOLUMNW {
            mask: LVCF_TEXT | LVCF_WIDTH,
            cx: *width,
            pszText: PWSTR(text.as_mut_ptr()),
            ..Default::default()
        };
        SendMessageW(list, LVM_INSERTCOLUMNW, Some(WPARAM(i)), Some(LPARAM(&col as *const _ as isize)));
    }
    UI.with(|u| *u.borrow_mut() = Some(Controls { tabs, label, filter, live, hide_frame, clear, count, list }));
    Ok(())
}

/// Place the tabs at the top of the panel (x0.., `w` wide) and the Logs page below them.
pub unsafe fn layout(x0: i32, w: i32, h: i32, toolbar: i32) {
    let Some(c) = ui() else { return };
    let _ = MoveWindow(c.tabs, x0 + 6, 6, (w - 12).clamp(10, 300), toolbar - 6, true);
    let y = toolbar + 6;
    let _ = MoveWindow(c.label, x0 + 8, y + 4, 40, 18, true);
    let _ = MoveWindow(c.filter, x0 + 50, y, (w - 140).max(60), 22, true);
    let _ = MoveWindow(c.clear, x0 + w - 80, y, 70, 22, true);
    let y2 = y + 28;
    let _ = MoveWindow(c.live, x0 + 8, y2, 160, 20, true);
    let _ = MoveWindow(c.hide_frame, x0 + 176, y2, 150, 20, true);
    let y3 = y2 + 24;
    let _ = MoveWindow(c.count, x0 + 8, y3, (w - 16).max(10), 18, true);
    let top = y3 + 24;
    let _ = MoveWindow(c.list, x0, top, w.max(1), (h - top).max(1), true);
}

/// Show or hide the tab strip (with the panel).
pub unsafe fn show_tabs(on: bool) {
    if let Some(c) = ui() {
        let _ = ShowWindow(c.tabs, if on { SW_SHOW } else { SW_HIDE });
    }
}

/// The selected tab: 0 = Logs, 1 = Device, 2 = Input.
pub unsafe fn current_tab() -> usize {
    ui().map(|c| SendMessageW(c.tabs, TCM_GETCURSEL, None, None).0.max(0) as usize).unwrap_or(0)
}

/// Tick "Hide per-frame calls" (used by the test snapshot).
pub unsafe fn set_hide_per_frame(on: bool) {
    if let Some(c) = ui() {
        SendMessageW(c.hide_frame, BM_SETCHECK, Some(WPARAM(on as usize)), None);
        DATA.with(|d| d.borrow_mut().hide_frame = on);
    }
}

pub unsafe fn select_tab(i: usize) {
    if let Some(c) = ui() {
        SendMessageW(c.tabs, TCM_SETCURSEL, Some(WPARAM(i)), None);
    }
}

pub unsafe fn show(on: bool) {
    let Some(c) = ui() else { return };
    let cmd = if on { SW_SHOW } else { SW_HIDE };
    for h in [c.label, c.filter, c.live, c.hide_frame, c.clear, c.count, c.list] {
        let _ = ShowWindow(h, cmd);
    }
    if on {
        refresh(true);
    }
}

fn matches(e: &Entry, f: &str, hide_frame: bool) -> bool {
    if hide_frame && e.per_frame() {
        return false;
    }
    f.is_empty()
        || e.name.to_ascii_lowercase().contains(f)
        || e.args.to_ascii_lowercase().contains(f)
        || e.outs.to_ascii_lowercase().contains(f)
        || format!("{:?}", e.result).to_ascii_lowercase().contains(f)
}

/// Pick up new calls (incrementally), or rebuild the rows after a filter change.
pub unsafe fn refresh(rebuild: bool) {
    let Some(c) = ui() else { return };
    let (n, live, total) = DATA.with(|d| {
        let mut d = d.borrow_mut();
        if !d.live && !rebuild {
            return (d.rows.len(), false, 0);
        }
        let all = trace::snapshot();
        let first_seq = all.first().map(|e| e.seq).unwrap_or(0);
        if rebuild {
            d.rows.clear();
            d.last_seq = 0;
        }
        // Drop rows that fell out of the ring, then add what is new.
        let keep_from = d.rows.partition_point(|e| e.seq < first_seq);
        d.rows.drain(..keep_from);
        let last = d.last_seq;
        let start = all.partition_point(|e| e.seq <= last);
        let f = d.filter.clone();
        let hide = d.hide_frame;
        for e in &all[start..] {
            if matches(e, &f, hide) {
                d.rows.push(e.clone());
            }
        }
        if let Some(e) = all.last() {
            d.last_seq = e.seq;
        }
        d.shown_count = d.rows.len();
        (d.rows.len(), d.live, all.len())
    });
    // No borrow is held here: the list asks for row text synchronously.
    SendMessageW(c.list, LVM_SETITEMCOUNT, Some(WPARAM(n)), Some(LPARAM((LVSICF_NOINVALIDATEALL | LVSICF_NOSCROLL) as isize)));
    if live && n > 0 {
        SendMessageW(c.list, LVM_ENSUREVISIBLE, Some(WPARAM(n - 1)), Some(LPARAM(0)));
    }
    SendMessageW(c.list, LVM_REDRAWITEMS, Some(WPARAM(0)), Some(LPARAM(n as isize)));
    if total > 0 || rebuild {
        let text = HSTRING::from(format!(
            "{n} shown of the last {total} calls (keeps {}); double-click a row for the full text",
            trace::CAPACITY
        ));
        let _ = SetWindowTextW(c.count, PCWSTR(text.as_ptr()));
    }
}

fn cell(e: &Entry, col: i32) -> String {
    match col {
        0 => e.seq.to_string(),
        1 => format!("{:.3}", e.t_ms),
        2 => format!("t{}", e.thread),
        3 => e.name.to_string(),
        4 => format!("{:?} ({} us)", e.result, e.dur_us),
        _ => {
            if e.outs.is_empty() {
                e.args.clone()
            } else {
                format!("{} => {}", e.args, e.outs)
            }
        }
    }
}

/// WM_NOTIFY from the tabs or the list. `Some(tab)` when the tab changed.
pub unsafe fn on_notify(lp: LPARAM) -> Option<usize> {
    let hdr = &*(lp.0 as *const NMHDR);
    match (hdr.idFrom, hdr.code) {
        (ID_TABS, TCN_SELCHANGE) => Some(current_tab()),
        (ID_LIST, LVN_GETDISPINFOW) => {
            let di = &mut *(lp.0 as *mut NMLVDISPINFOW);
            let item = &mut di.item;
            if item.mask.0 & LVIF_TEXT.0 != 0 && !item.pszText.is_null() && item.cchTextMax > 0 {
                let text = DATA.with(|d| d.borrow().rows.get(item.iItem as usize).map(|e| cell(e, item.iSubItem)));
                if let Some(t) = text {
                    let max = item.cchTextMax as usize - 1;
                    let mut n = 0;
                    for u in t.encode_utf16().take(max) {
                        *item.pszText.0.add(n) = u;
                        n += 1;
                    }
                    *item.pszText.0.add(n) = 0;
                }
            }
            None
        }
        (ID_LIST, NM_DBLCLK) => {
            let c = ui()?;
            let sel = SendMessageW(c.list, LVM_GETNEXTITEM, Some(WPARAM(usize::MAX)), Some(LPARAM(2))).0;
            if sel >= 0 {
                if let Some(line) = DATA.with(|d| d.borrow().rows.get(sel as usize).map(|e| e.line())) {
                    // Ctrl+C in the message box copies the whole text.
                    let text = HSTRING::from(line.as_str());
                    MessageBoxW(Some(hdr.hwndFrom), PCWSTR(text.as_ptr()), w!("xrmimic: OpenXR call"), MB_OK);
                }
            }
            None
        }
        _ => None,
    }
}

/// WM_COMMAND for the Logs page's controls; true if it was one of them.
pub unsafe fn on_command(id: usize, code: u32) -> bool {
    let Some(c) = ui() else { return false };
    match (id, code) {
        (ID_FILTER, EN_CHANGE) => {
            let mut buf = [0u16; 256];
            let n = GetWindowTextW(c.filter, &mut buf) as usize;
            let f = String::from_utf16_lossy(&buf[..n]).to_ascii_lowercase();
            DATA.with(|d| d.borrow_mut().filter = f);
            refresh(true);
            true
        }
        (ID_LIVE, BN_CLICKED) => {
            let on = SendMessageW(c.live, BM_GETCHECK, None, None).0 == 1;
            DATA.with(|d| d.borrow_mut().live = on);
            if on {
                refresh(false);
            }
            true
        }
        (ID_HIDE_FRAME, BN_CLICKED) => {
            let on = SendMessageW(c.hide_frame, BM_GETCHECK, None, None).0 == 1;
            DATA.with(|d| d.borrow_mut().hide_frame = on);
            refresh(true);
            true
        }
        (ID_CLEAR, BN_CLICKED) => {
            trace::clear();
            refresh(true);
            true
        }
        (ID_FILTER | ID_LIVE | ID_HIDE_FRAME | ID_CLEAR, _) => true,
        _ => false,
    }
}
