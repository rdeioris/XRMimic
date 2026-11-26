//! The "headset": a desktop window on its own thread and its own D3D11 device.
//! It shows the latest submitted frame (both eyes side by side) and turns
//! mouse, keyboard and gamepad input into the simulated user's motion.

use crate::shared::Shared;
use crate::xlog;
use std::cell::RefCell;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Instant;
use windows::core::{s, w, Interface, PCSTR};
use windows::Win32::Foundation::*;
use windows::Win32::Graphics::Direct3D::Fxc::D3DCompile;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::Input::KeyboardAndMouse::*;
use windows::Win32::UI::Input::XboxController::*;
use windows::Win32::UI::WindowsAndMessaging::*;
use crate::runtimes::{self, RuntimeEntry};
use windows::core::HSTRING;
use windows::Win32::Graphics::Gdi::{GetStockObject, DEFAULT_GUI_FONT, HBRUSH};
use xrmimic_core::sim::{ControllerInput, InputFrame, SimState, LEFT, RIGHT};

pub struct Window {
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl Window {
    pub fn spawn(gfx: &crate::gfx::SessionGfx, sim: Arc<Mutex<SimState>>, shared: Arc<Shared>) -> Window {
        let luid = gfx.luid;
        let s = shared.clone();
        let luid = (luid.LowPart, luid.HighPart);
        let thread = std::thread::Builder::new()
            .name("xrmimic-window".into())
            .spawn(move || {
                let luid = LUID { LowPart: luid.0, HighPart: luid.1 };
                if let Err(e) = run(luid, sim, s) {
                    xlog!("window thread failed: {e:?}");
                }
            })
            .ok();
        Window { shared, thread }
    }

    pub fn stop(mut self) {
        self.shared.stop.store(true, Ordering::SeqCst);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
enum Drag {
    Look,
    Pan,
    /// Ctrl + left drag: move the active controller.
    Hand,
    /// Ctrl + right drag: turn it.
    HandTurn,
}

struct WinInput {
    keys: [bool; 256],
    mouse_dx: f32,
    mouse_dy: f32,
    pan_dx: f32,
    pan_dy: f32,
    zoom: f32,
    /// The active controller's own motion (see InputFrame::hand_dx).
    hand: [f32; 5],
    hand_reset: bool,
    /// A mouse drag in progress on the view: what it does and the last cursor position.
    drag: Option<(Drag, i32, i32)>,
    toggle: bool,
    resized: Option<(u32, u32)>,
    /// The runtime list is open (so a selection is a deliberate choice).
    dropped: bool,
    /// Runtime picked from the list, by index.
    choose: Option<usize>,
    refresh: bool,
}

impl Default for WinInput {
    fn default() -> Self {
        WinInput {
            keys: [false; 256],
            mouse_dx: 0.0,
            mouse_dy: 0.0,
            pan_dx: 0.0,
            pan_dy: 0.0,
            zoom: 0.0,
            hand: [0.0; 5],
            hand_reset: false,
            drag: None,
            toggle: false,
            resized: None,
            dropped: false,
            choose: None,
            refresh: false,
        }
    }
}

/// MK_CONTROL in a mouse message's wParam: Ctrl is held.
const MK_CONTROL: usize = 0x0008;
/// Height of the strip above the eye view that holds the runtime list.
const TOOLBAR: i32 = 34;
const ID_COMBO: usize = 100;
const ID_DEPTH: usize = 101;
const ID_CENTRE: usize = 102;
const ID_PANEL: usize = 103;
const ID_REPROJECT: usize = 104;
/// Width of the panel on the right that holds the Logs, Device and Input tabs.
const PANEL_W: i32 = 600;
const BS_AUTOCHECKBOX: u32 = 0x0003;
const BM_GETCHECK: u32 = 0x00F0;
const BM_SETCHECK: u32 = 0x00F1;
const WM_RUNTIME_SWITCHED: u32 = WM_APP + 1;
const CBS_DROPDOWNLIST: u32 = 0x0003;
const CB_ADDSTRING: u32 = 0x0143;
const CB_GETCURSEL: u32 = 0x0147;
const CB_RESETCONTENT: u32 = 0x014B;
const CB_SETCURSEL: u32 = 0x014E;
const CBN_SELCHANGE: u32 = 1;
const CBN_DROPDOWN: u32 = 7;
const CBN_CLOSEUP: u32 = 8;
const CBN_SELENDOK: u32 = 9;

#[derive(Clone, Copy)]
struct Hwnds {
    frame: HWND,
    view: HWND,
    combo: HWND,
    depth: HWND,
    centre: HWND,
    reproject: HWND,
    panel: HWND,
}

thread_local! {
    static INPUT: RefCell<WinInput> = RefCell::new(WinInput::default());
    static SHARED: RefCell<Option<Arc<Shared>>> = const { RefCell::new(None) };
    static HWNDS: RefCell<Option<Hwnds>> = const { RefCell::new(None) };
    static ENTRIES: RefCell<Vec<RuntimeEntry>> = const { RefCell::new(Vec::new()) };
    /// The frame's client size, for laying out again when the panel toggles.
    static FRAME_SIZE: RefCell<(i32, i32)> = const { RefCell::new((0, 0)) };
}

/// Keys go back to the headset view (WASD), e.g. after a click in the panel.
pub(crate) unsafe fn focus_view() {
    if let Some(h) = hwnds() {
        let _ = SetFocus(Some(h.view));
    }
}

unsafe fn panel_on() -> bool {
    hwnds().is_some_and(|h| SendMessageW(h.panel, BM_GETCHECK, None, None).0 == 1)
}

/// The headset view on the left, always shown, with the runtime list and its
/// checkboxes above it; the panel (Logs, Device, Input tabs) on the right.
unsafe fn layout_frame(w: i32, h: i32) {
    let Some(hw) = hwnds() else { return };
    FRAME_SIZE.with(|f| *f.borrow_mut() = (w, h));
    let on = panel_on();
    let panel_w = if on { PANEL_W.min(w - 240).max(0) } else { 0 };
    let view_w = (w - panel_w).max(1);
    let _ = MoveWindow(hw.view, 0, TOOLBAR, view_w, (h - TOOLBAR).max(1), true);
    let _ = MoveWindow(hw.combo, 8, 6, (view_w - 8 - 336).max(120), 320, true);
    let _ = MoveWindow(hw.reproject, view_w - 322, 8, 82, 20, true);
    let _ = MoveWindow(hw.depth, view_w - 236, 8, 64, 20, true);
    let _ = MoveWindow(hw.centre, view_w - 166, 8, 70, 20, true);
    let _ = MoveWindow(hw.panel, view_w - 90, 8, 80, 20, true);
    crate::logview::layout(view_w, panel_w, h, TOOLBAR);
    crate::devicetab::layout(view_w, panel_w, h, TOOLBAR);
    crate::inputtab::layout(view_w, panel_w, h, TOOLBAR);
    crate::logview::show_tabs(on);
    show_tab(if on { crate::logview::current_tab() } else { usize::MAX });
}

fn hwnds() -> Option<Hwnds> {
    HWNDS.with(|h| *h.borrow())
}

/// Refill the runtime list and select the system's active runtime.
unsafe fn fill_combo() {
    let Some(h) = hwnds() else { return };
    let entries = runtimes::list();
    SendMessageW(h.combo, CB_RESETCONTENT, None, None);
    let mut active = None;
    for (i, e) in entries.iter().enumerate() {
        let label = HSTRING::from(e.label().as_str());
        SendMessageW(h.combo, CB_ADDSTRING, None, Some(LPARAM(label.as_ptr() as isize)));
        if e.active {
            active = Some(i);
        }
    }
    let sel = active.map(|i| i as isize).unwrap_or(-1);
    SendMessageW(h.combo, CB_SETCURSEL, Some(WPARAM(sel as usize)), None);
    ENTRIES.with(|en| *en.borrow_mut() = entries);
}

fn with_input<R>(f: impl FnOnce(&mut WinInput) -> R) -> R {
    INPUT.with(|i| f(&mut i.borrow_mut()))
}

/// Cursor position in a mouse message (client coordinates, may be negative).
fn mouse_pos(lp: LPARAM) -> (i32, i32) {
    ((lp.0 & 0xffff) as i16 as i32, ((lp.0 >> 16) & 0xffff) as i16 as i32)
}

/// Start a drag on the view. The cursor stays visible; Win32 capture keeps the
/// drag going if it leaves the window.
unsafe fn begin_drag(hwnd: HWND, kind: Drag, lp: LPARAM) {
    let _ = SetFocus(Some(hwnd));
    let (x, y) = mouse_pos(lp);
    with_input(|i| i.drag = Some((kind, x, y)));
    SetCapture(hwnd);
}

unsafe fn end_drag() {
    if with_input(|i| i.drag.take()).is_some() {
        let _ = ReleaseCapture();
    }
}

/// The panel's page: 0 = Logs, 1 = Device, 2 = Input (anything else: none,
/// the panel is hidden). The headset view stays visible either way.
unsafe fn show_tab(tab: usize) {
    crate::logview::show(tab == 0);
    crate::devicetab::show(tab == 1);
    crate::inputtab::show(tab == 2);
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    match msg {
        WM_KEYDOWN | WM_SYSKEYDOWN => {
            let vk = (wp.0 & 0xff) as usize;
            let repeat = lp.0 & (1 << 30) != 0;
            with_input(|i| {
                i.keys[vk] = true;
                if vk == VK_TAB.0 as usize && !repeat {
                    i.toggle = true;
                }
            });
            // R: the active controller follows the head again.
            if vk == b'R' as usize && !repeat {
                with_input(|i| i.hand_reset = true);
            }
            // H: put the controllers down / pick them up.
            if vk == b'H' as usize && !repeat {
                crate::inputtab::toggle_hands();
            }
            if msg == WM_SYSKEYDOWN && vk == VK_F4.0 as usize {
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
            LRESULT(0)
        }
        WM_KEYUP | WM_SYSKEYUP => {
            with_input(|i| i.keys[(wp.0 & 0xff) as usize] = false);
            LRESULT(0)
        }
        // On the view: left drag looks around, right drag pans, the wheel
        // zooms; with Ctrl they move and turn the active controller instead.
        WM_LBUTTONDOWN | WM_RBUTTONDOWN => {
            if hwnds().is_some_and(|h| h.view == hwnd) {
                let ctrl = wp.0 & MK_CONTROL != 0;
                let kind = match (msg == WM_LBUTTONDOWN, ctrl) {
                    (true, false) => Drag::Look,
                    (false, false) => Drag::Pan,
                    (true, true) => Drag::Hand,
                    (false, true) => Drag::HandTurn,
                };
                begin_drag(hwnd, kind, lp);
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            let (x, y) = mouse_pos(lp);
            with_input(|i| {
                if let Some((kind, lx, ly)) = i.drag {
                    let (dx, dy) = ((x - lx) as f32, (y - ly) as f32);
                    match kind {
                        Drag::Look => {
                            i.mouse_dx += dx;
                            i.mouse_dy += dy;
                        }
                        Drag::Pan => {
                            i.pan_dx += dx;
                            i.pan_dy += dy;
                        }
                        Drag::Hand => {
                            i.hand[0] += dx;
                            i.hand[1] += dy;
                        }
                        Drag::HandTurn => {
                            i.hand[3] += dx;
                            i.hand[4] += dy;
                        }
                    }
                    i.drag = Some((kind, x, y));
                }
            });
            LRESULT(0)
        }
        WM_LBUTTONUP | WM_RBUTTONUP => {
            let kinds = if msg == WM_LBUTTONUP { [Drag::Look, Drag::Hand] } else { [Drag::Pan, Drag::HandTurn] };
            if with_input(|i| i.drag.is_some_and(|(k, _, _)| kinds.contains(&k))) {
                end_drag();
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            with_input(|i| i.drag = None);
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            let notches = ((wp.0 >> 16) & 0xffff) as i16 as f32 / 120.0;
            if wp.0 & MK_CONTROL != 0 {
                with_input(|i| i.hand[2] += notches);
            } else {
                with_input(|i| i.zoom += notches);
            }
            LRESULT(0)
        }
        WM_KILLFOCUS => {
            end_drag();
            with_input(|i| i.keys = [false; 256]);
            LRESULT(0)
        }
        WM_SIZE => {
            let (w, h) = ((lp.0 & 0xffff) as u32, ((lp.0 >> 16) & 0xffff) as u32);
            if hwnds().is_some_and(|hw| hw.frame == hwnd) {
                // The frame lays out its children; the view reports its own size.
                layout_frame(w as i32, h as i32);
                return LRESULT(0);
            }
            with_input(|i| i.resized = Some((w, h)));
            LRESULT(0)
        }
        WM_COMMAND if (wp.0 & 0xffff) == ID_COMBO => {
            let code = ((wp.0 >> 16) & 0xffff) as u32;
            match code {
                CBN_DROPDOWN => {
                    with_input(|i| i.dropped = true);
                    fill_combo(); // pick up changes made by other tools
                }
                CBN_SELENDOK if with_input(|i| i.dropped) => {
                    let sel = SendMessageW(HWND(lp.0 as _), CB_GETCURSEL, None, None).0;
                    if sel >= 0 {
                        with_input(|i| i.choose = Some(sel as usize));
                    }
                }
                // Typing in the closed list (e.g. W/A/S/D) must not switch runtimes.
                CBN_SELCHANGE if !with_input(|i| i.dropped) => fill_combo(),
                CBN_CLOSEUP => {
                    with_input(|i| i.dropped = false);
                    if let Some(h) = hwnds() {
                        let _ = SetFocus(Some(h.view));
                    }
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_COMMAND if (wp.0 & 0xffff) == ID_PANEL => {
            let (w, h) = FRAME_SIZE.with(|f| *f.borrow());
            layout_frame(w, h);
            focus_view();
            LRESULT(0)
        }
        WM_COMMAND if matches!(wp.0 & 0xffff, ID_DEPTH | ID_CENTRE | ID_REPROJECT) => {
            // The checkbox toggled itself; keys go back to the view (WASD).
            if let Some(h) = hwnds() {
                let _ = SetFocus(Some(h.view));
            }
            LRESULT(0)
        }
        WM_COMMAND => {
            let code = ((wp.0 >> 16) & 0xffff) as u32;
            let id = wp.0 & 0xffff;
            if !crate::logview::on_command(id, code)
                && !crate::devicetab::on_command(id, code)
                && !crate::inputtab::on_command(id, code)
            {
                return DefWindowProcW(hwnd, msg, wp, lp);
            }
            LRESULT(0)
        }
        WM_HSCROLL if lp.0 != 0 && crate::inputtab::on_scroll(HWND(lp.0 as _), wp.0 & 0xffff) => LRESULT(0),
        WM_NOTIFY => {
            if let Some(tab) = crate::logview::on_notify(lp) {
                show_tab(tab);
            }
            LRESULT(0)
        }
        WM_RUNTIME_SWITCHED => {
            with_input(|i| i.refresh = true);
            LRESULT(0)
        }
        WM_CLOSE => {
            // Closing the headset ends the session; the runtime destroys the
            // window once the app has ended it.
            SHARED.with(|s| {
                if let Some(s) = s.borrow().as_ref() {
                    s.close_requested.store(true, Ordering::SeqCst);
                }
            });
            let _ = ShowWindow(hwnd, SW_HIDE);
            end_drag();
            LRESULT(0)
        }
        _ => DefWindowProcW(hwnd, msg, wp, lp),
    }
}

const SHADER: &str = r#"
Texture2D tex : register(t0);
SamplerState smp : register(s0);
struct V { float4 pos : SV_Position; float2 uv : TEXCOORD0; };
V vs(uint id : SV_VertexID) {
    V o;
    float2 uv = float2((id << 1) & 2, id & 2);
    o.pos = float4(uv * float2(2, -2) + float2(-1, 1), 0, 1);
    o.uv = uv;
    return o;
}
float4 ps(V i) : SV_Target { return float4(tex.Sample(smp, i.uv).rgb, 1); }

// Quad and cylinder layers, placed by mvp, the texture's top row at the top.
// shape.x: 0 = quad (the unit square, sized by mvp), 1 = cylinder (shape.y =
// radius, shape.z = central angle, shape.w = height; math::cylinder_point).
// 2 = equirect (shape.y = radius, shape.z = horizontal angle, shape.w = upper
// angle, shape2.x = lower angle; math::equirect_point).
// 3 = cube (shape.y = half the cube's size): 36 vertices of a cube around
// the eye; the pixel shader looks the cube map up by direction.
// Drawn as a grid of cells of 6 vertices: 1 x 1 for a quad, 64 x 1 for a
// cylinder, 64 x 32 for an equirect, row by row.
cbuffer Layer : register(b0) { float4x4 mvp; float4 shape; float4 shape2; };
// dir.w = 1 for a cube: dir.xyz is then the direction in the layer's space.
// vz: the point's distance along the eye's view axis (clip w).
struct VQ { float4 pos : SV_Position; float2 uv : TEXCOORD0; float4 dir : TEXCOORD1; float vz : TEXCOORD2; };
VQ qvs(uint id : SV_VertexID) {
    VQ o;
    if (shape.x > 2.5) {
        // Faces -X +X -Y +Y -Z +Z as quads of corners (bit 0: +x, 1: +y, 2: +z).
        const uint quad[24] = { 0, 2, 6, 4,  1, 3, 7, 5,  0, 1, 5, 4,  2, 3, 7, 6,  0, 1, 3, 2,  4, 5, 7, 6 };
        const uint tri[6] = { 0, 1, 2, 0, 2, 3 };
        uint c = quad[(id / 6) * 4 + tri[id % 6]];
        float3 d = float3((c & 1) ? 1 : -1, (c & 2) ? 1 : -1, (c & 4) ? 1 : -1);
        o.pos = mul(mvp, float4(d * shape.y, 1));
        o.uv = float2(0, 0);
        o.dir = float4(d, 1);
        o.vz = o.pos.w;
        return o;
    }
    o.dir = float4(0, 0, 0, 0);
    const float2 corner[6] = { float2(0, 1), float2(0, 0), float2(1, 0),
                               float2(0, 1), float2(1, 0), float2(1, 1) };
    bool cyl = shape.x > 0.5 && shape.x < 1.5;
    bool eq = shape.x > 1.5;
    uint segs_x = (cyl || eq) ? 64 : 1;
    float segs_y = eq ? 32.0 : 1.0;
    uint cell = id / 6;
    float u = (float(cell % segs_x) + corner[id % 6].x) / float(segs_x);
    float v = (float(cell / segs_x) + corner[id % 6].y) / segs_y;
    float3 p;
    if (eq) {
        float th = (u - 0.5) * shape.z;
        float ph = shape.w - v * (shape.w - shape2.x);
        p = shape.y * float3(cos(ph) * sin(th), sin(ph), -cos(ph) * cos(th));
    } else if (cyl) {
        float a = (u - 0.5) * shape.z;
        p = float3(shape.y * sin(a), (0.5 - v) * shape.w, -shape.y * cos(a));
    } else {
        p = float3(u - 0.5, 0.5 - v, 0);
    }
    o.pos = mul(mvp, float4(p, 1));
    o.uv = float2(u, v);
    o.vz = o.pos.w;
    return o;
}
// XR_FB_composition_layer_depth_test: lt.x = 1 to test, lt.y = the
// XrCompareOpFB (0 never, 1 less, 2 equal, 3 less or equal, 4 greater, 5 not
// equal, 6 greater or equal, 7 always), comparing the layer's distance with
// the projection layer's at that pixel (nearer = less). lvp = the eye's
// viewport (x, y, w, h), lrect / linfo = its depth image as in the depth view.
cbuffer LayerDepth : register(b3) { float4 lt; float4 lrect; float4 linfo; float4 lvp; };
Texture2D<float> ldep : register(t3);
bool depth_pass(float4 pos, float vz) {
    if (lt.x < 0.5) return true;
    float2 uv = saturate((pos.xy - lvp.xy) / lvp.zw);
    float d = ldep.Load(int3(lrect.xy + uv * (lrect.zw - 1), 0));
    float z = saturate((d - linfo.x) / (linfo.y - linfo.x));
    float n = linfo.z, f = linfo.w, scene;
    if (isinf(n)) scene = f / max(z, 1e-6);
    else if (isinf(f)) scene = n / max(1 - z, 1e-6);
    else scene = n * f / (f - z * (f - n));
    bool eq = abs(vz - scene) <= 1e-3 * scene;
    uint op = (uint)(lt.y + 0.5);
    if (op == 0) return false;
    if (op == 1) return vz < scene && !eq;
    if (op == 2) return eq;
    if (op == 3) return vz < scene || eq;
    if (op == 4) return vz > scene && !eq;
    if (op == 5) return !eq;
    if (op == 6) return vz > scene || eq;
    return true;
}
// A cube's six faces sit side by side in the texture, +X -X +Y -Y +Z -Z;
// the standard cube lookup (math::cube_lookup) picks face and texel.
// Depth view: one eye's depth buffer as greys, nearer brighter
// (math::depth_distance, math::depth_shade). drect = the eye's rectangle in
// the texture (x, y, w, h texels), dinfo = (min, max, near, far).
cbuffer DepthView : register(b1) { float4 drect; float4 dinfo; };
Texture2D<float> dtex : register(t1);
float4 dps(V i) : SV_Target {
    float2 pix = drect.xy + saturate(i.uv) * (drect.zw - 1);
    float d = dtex.Load(int3(pix, 0));
    float z = saturate((d - dinfo.x) / (dinfo.y - dinfo.x));
    float n = dinfo.z, f = dinfo.w, dist;
    if (isinf(n)) dist = f / max(z, 1e-6);
    else if (isinf(f)) dist = n / max(1 - z, 1e-6);
    else dist = n * f / (f - z * (f - n));
    float g = 1 / (1 + 0.5 * max(dist, 0));
    return float4(g, g, g, 1);
}
// Centre view: a camera between the eyes (ctan = its tangents left, right,
// up, down; cq / cpos = its orientation and position, cpos.w = 1 when both
// eyes sent depth) reads the composed eyes in `tex` (left half, right half).
// Without depth each direction is read as if far away, from the left eye on
// the left and the right eye on the right (a soft seam between). With depth
// the centre ray is marched through each eye's depth buffer to the surface it
// hits, so near things land where the centre camera would see them.
// The same pass reprojects: the camera is then one eye where it is now
// (cpick.x = 0 left, 1 right: read that eye wherever it sees the point; -1
// for the centre view's blend), reading the frame drawn from older poses.
cbuffer Centre : register(b2) {
    float4 ctan; float4 cq; float4 cpos;
    float4 eq0; float4 eq1; float4 ep0; float4 ep1;
    float4 et0; float4 et1; float4 dr0; float4 dr1; float4 di0; float4 di1;
    float4 cpick;
};
Texture2D<float> cdep0 : register(t1);
Texture2D<float> cdep1 : register(t2);
float3 qrot(float4 q, float3 v) { float3 t = 2 * cross(q.xyz, v); return v + q.w * t + cross(q.xyz, t); }
float3 qinv(float4 q, float3 v) { return qrot(float4(-q.xyz, q.w), v); }
// Where a vector in eye e's own space lands in that eye (0..1), if it does.
bool eye_uv(int e, float3 local, out float2 uv) {
    float4 t = e == 0 ? et0 : et1;
    float z = -local.z;
    float2 tn = local.xy / max(z, 1e-6);
    uv = float2((tn.x - t.x) / (t.y - t.x), (t.z - tn.y) / (t.z - t.w));
    return z > 1e-6 && all(uv >= 0) && all(uv <= 1);
}
float3 to_eye(int e, float3 world) {
    return e == 0 ? qinv(eq0, world - ep0.xyz) : qinv(eq1, world - ep1.xyz);
}
float4 eye_colour(int e, float2 uv) {
    uint w, h;
    tex.GetDimensions(w, h);
    float ew = w * 0.5;
    uv.x = clamp(uv.x, 0.5 / ew, 1 - 0.5 / ew);
    return tex.SampleLevel(smp, float2((e + uv.x) * 0.5, uv.y), 0);
}
// Eye e's depth (planar, metres) at uv (math::depth_distance).
float scene_z(int e, float2 uv) {
    float4 r = e == 0 ? dr0 : dr1;
    float4 i = e == 0 ? di0 : di1;
    int3 pix = int3(r.xy + saturate(uv) * (r.zw - 1), 0);
    float d = e == 0 ? cdep0.Load(pix) : cdep1.Load(pix);
    float z = saturate((d - i.x) / (i.y - i.x));
    float n = i.z, f = i.w;
    if (isinf(n)) return f / max(z, 1e-6);
    if (isinf(f)) return n / max(1 - z, 1e-6);
    return n * f / (f - z * (f - n));
}
// March the centre ray o + dir * t (t = depth in the centre camera) through
// eye e, near to far, evenly in 1 / t. 1: it hits a surface (uv, t);
// 0: it passes behind something this eye sees in front (this eye cannot see
// that point); 2: nothing closer than 6.7 m: read it as far away.
int march(int e, float3 o, float3 dir, out float2 uv, out float t) {
    const int N = 64;
    const float s0 = 1 / 0.15;
    const float ds = s0 / N;
    float prev = s0 + ds;
    uv = float2(0, 0);
    t = 0;
    [loop] for (int k = 0; k < N; k++) {
        float s = s0 - k * ds;
        float3 l = to_eye(e, o + dir / s);
        float2 u;
        if (!eye_uv(e, l, u)) { prev = s; continue; }
        float zs = scene_z(e, u);
        if (1 / -l.z <= 1 / zs) {
            if (1 / zs - 1 / -l.z > 2.5 * ds) return 0;
            // Refine between the last point in front and this one.
            float a = prev, b = s;
            [loop] for (int j = 0; j < 6; j++) {
                float m = 0.5 * (a + b);
                float3 lm = to_eye(e, o + dir / m);
                float2 um;
                if (eye_uv(e, lm, um) && 1 / -lm.z <= 1 / scene_z(e, um)) b = m; else a = m;
            }
            eye_uv(e, to_eye(e, o + dir / b), uv);
            t = 1 / b;
            return 1;
        }
        prev = s;
    }
    // Nothing nearer than 6.7 m along the ray: read it as far away, unless
    // this eye sees something near in that direction (it is in the way).
    if (eye_uv(e, to_eye(e, o + dir * 1e4), uv) && scene_z(e, uv) < 0.9 / ds) return 0;
    return 2;
}
float4 cps(V i) : SV_Target {
    float2 p = saturate(i.uv);
    float3 dc = float3(lerp(ctan.x, ctan.y, p.x), lerp(ctan.z, ctan.w, p.y), -1);
    float3 dir = qrot(cq, dc);
    float wr = smoothstep(-0.05, 0.05, dc.x);
    float2 ul, ur;
    bool vl = eye_uv(0, qinv(eq0, dir), ul);
    bool vr = eye_uv(1, qinv(eq1, dir), ur);
    if (cpos.w > 0.5) {
        float2 hl, hr;
        float tl, tr;
        int ml = march(0, cpos.xyz, dir, hl, tl);
        int mr = march(1, cpos.xyz, dir, hr, tr);
        if (ml == 1 && mr == 1) {
            ul = hl; ur = hr; vl = true; vr = true;
            // The eyes disagree: one sees past an edge the other does not; the nearer hit wins.
            if (abs(tl - tr) > 0.1 * min(tl, tr)) wr = tr < tl ? 1 : 0;
        } else if (ml == 1) {
            ul = hl; vl = true; vr = false;
        } else if (mr == 1) {
            ur = hr; vr = true; vl = false;
        } else {
            vl = vl && ml != 0;
            vr = vr && mr != 0;
        }
    }
    if (cpick.x >= 0) {
        if (cpick.x < 0.5 && vl) wr = 0;
        else if (cpick.x > 0.5 && vr) wr = 1;
    }
    if (!vl && !vr) return float4(0, 0, 0, 1);
    if (!vl) wr = 1;
    if (!vr) wr = 0;
    float3 c = float3(0, 0, 0);
    if (wr < 1) c += (1 - wr) * eye_colour(0, ul).rgb;
    if (wr > 0) c += wr * eye_colour(1, ur).rgb;
    return float4(c, 1);
}
float4 qps(VQ i) : SV_Target {
    if (!depth_pass(i.pos, i.vz)) discard;
    if (i.dir.w < 0.5) return tex.Sample(smp, i.uv);
    float3 d = i.dir.xyz;
    float3 a = abs(d);
    float face, sc, tc, ma;
    if (a.x >= a.y && a.x >= a.z) {
        ma = a.x;
        if (d.x > 0) { face = 0; sc = -d.z; tc = -d.y; } else { face = 1; sc = d.z; tc = -d.y; }
    } else if (a.y >= a.z) {
        ma = a.y;
        if (d.y > 0) { face = 2; sc = d.x; tc = d.z; } else { face = 3; sc = d.x; tc = -d.z; }
    } else {
        ma = a.z;
        if (d.z > 0) { face = 4; sc = d.x; tc = -d.y; } else { face = 5; sc = -d.x; tc = -d.y; }
    }
    float2 uv = (float2(sc, tc) / ma + 1) * 0.5;
    uint w, h;
    tex.GetDimensions(w, h);
    float fw = w / 6.0;
    // Stay half a texel inside the face so filtering does not bleed into the next one.
    uv = clamp(uv, float2(0.5 / fw, 0.5 / h), float2(1 - 0.5 / fw, 1 - 0.5 / h));
    return tex.SampleLevel(smp, float2((face + uv.x) / 6, uv.y), 0);
}
// Play-area boundary (bounded floor): four grid walls 2.5 m tall around the
// STAGE origin, shape.xy = half width and half depth, shape2.xyz = the head.
// Each wall shows only near the head, fading out by 1.2 m away.
struct VG { float4 pos : SV_Position; float3 world : TEXCOORD0; };
VG gvs(uint id : SV_VertexID) {
    const float2 c[4] = { float2(-1, -1), float2(1, -1), float2(1, 1), float2(-1, 1) };
    const float2 tri[6] = { float2(0, 0), float2(1, 0), float2(1, 1), float2(0, 0), float2(1, 1), float2(0, 1) };
    uint wall = id / 6;
    float2 t = tri[id % 6];
    float2 xz = lerp(c[wall] * shape.xy, c[(wall + 1) % 4] * shape.xy, t.x);
    float3 p = float3(xz.x, t.y * 2.5, xz.y);
    VG o;
    o.pos = mul(mvp, float4(p, 1));
    o.world = p;
    return o;
}
float4 gps(VG i) : SV_Target {
    float fade = saturate((1.2 - distance(i.world, shape2.xyz)) / 0.8);
    float2 q = float2(i.world.x + i.world.z, i.world.y) / 0.25;
    float2 g = abs(frac(q - 0.5) - 0.5) / max(fwidth(q), 1e-4);
    float grid = 1 - saturate(min(g.x, g.y));
    return float4(0.35, 0.7, 1.0, fade * (0.12 + 0.88 * grid));
}
"#;

unsafe fn compile(entry: PCSTR, target: PCSTR) -> windows::core::Result<Vec<u8>> {
    let mut blob = None;
    let mut err = None;
    let r = D3DCompile(
        SHADER.as_ptr() as *const _,
        SHADER.len(),
        s!("xrmimic"),
        None,
        None,
        entry,
        target,
        0,
        0,
        &mut blob,
        Some(&mut err),
    );
    if let Err(e) = r {
        if let Some(err) = err {
            let msg = std::slice::from_raw_parts(err.GetBufferPointer() as *const u8, err.GetBufferSize());
            xlog!("shader: {}", String::from_utf8_lossy(msg));
        }
        return Err(e);
    }
    let blob = blob.unwrap();
    Ok(std::slice::from_raw_parts(blob.GetBufferPointer() as *const u8, blob.GetBufferSize()).to_vec())
}

/// The centre view's pass: one camera between the eyes (math::centre_eye)
/// reads the composed eyes, through their depth buffers when both sent one.
#[allow(clippy::too_many_arguments)]
unsafe fn draw_centre(
    context: &ID3D11DeviceContext,
    rtv: &ID3D11RenderTargetView,
    comp: &(u32, u32, bool, ID3D11RenderTargetView, ID3D11ShaderResourceView),
    views: &[(xrmimic_core::math::Pose, xrmimic_core::math::Fov)],
    depth: &[crate::gfx::DepthSlot],
    opened_depth: &[Option<OpenedQuad>],
    latest: usize,
    size: (u32, u32),
    (vs, cps, cb): (Option<&ID3D11VertexShader>, Option<&ID3D11PixelShader>, &ID3D11Buffer),
) {
    use xrmimic_core::math::centre_eye;
    let (cpose, cfov) = centre_eye((&views[0].0, views[0].1), (&views[1].0, views[1].1));
    let ct = [cfov.left.tan(), cfov.right.tan(), cfov.up.tan(), cfov.down.tan()];
    // Square pixels: the viewport keeps the camera's tangent extents' ratio.
    let aspect = (ct[1] - ct[0]) / (ct[2] - ct[3]);
    let (cw, ch) = (size.0 as f32, size.1 as f32);
    let (mut w, mut h) = (cw, cw / aspect);
    if h > ch {
        h = ch;
        w = ch * aspect;
    }
    let vp = D3D11_VIEWPORT {
        TopLeftX: (cw - w) * 0.5,
        TopLeftY: (ch - h) * 0.5,
        Width: w,
        Height: h,
        MinDepth: 0.0,
        MaxDepth: 1.0,
    };
    draw_view(context, rtv, comp, views, depth, opened_depth, latest, (&cpose, cfov), -1.0, vp, (vs, cps, cb));
}

/// One camera (`camera`, drawn into `vp`) reading the composed eyes, through
/// their depth buffers when both sent one: the centre view (`pick` -1), or
/// an eye reprojected to where it is now (`pick` = that eye).
#[allow(clippy::too_many_arguments)]
unsafe fn draw_view(
    context: &ID3D11DeviceContext,
    rtv: &ID3D11RenderTargetView,
    comp: &(u32, u32, bool, ID3D11RenderTargetView, ID3D11ShaderResourceView),
    views: &[(xrmimic_core::math::Pose, xrmimic_core::math::Fov)],
    depth: &[crate::gfx::DepthSlot],
    opened_depth: &[Option<OpenedQuad>],
    latest: usize,
    (cpose, cfov): (&xrmimic_core::math::Pose, xrmimic_core::math::Fov),
    pick: f32,
    vp: D3D11_VIEWPORT,
    (vs, cps, cb): (Option<&ID3D11VertexShader>, Option<&ID3D11PixelShader>, &ID3D11Buffer),
) {
    use xrmimic_core::math::{Fov, Pose};
    let tans = |f: Fov| [f.left.tan(), f.right.tan(), f.up.tan(), f.down.tan()];
    let ct = tans(cfov);
    // Both eyes' depth buffers, or none.
    let find = |e: usize| {
        depth
            .iter()
            .enumerate()
            .find(|(i, d)| d.eye == e && opened_depth.get(*i).is_some_and(|o| o.is_some()))
    };
    let (dl, dr) = (find(0), find(1));
    let has_depth = dl.is_some() && dr.is_some();
    let q4 = |p: &Pose| [p.orientation.x, p.orientation.y, p.orientation.z, p.orientation.w];
    let p4 = |p: &Pose, w: f32| [p.position.x, p.position.y, p.position.z, w];
    let rect = |d: Option<(usize, &crate::gfx::DepthSlot)>| {
        d.map(|(_, d)| [d.rect[0] as f32, d.rect[1] as f32, d.rect[2] as f32, d.rect[3] as f32]).unwrap_or([0.0; 4])
    };
    let info = |d: Option<(usize, &crate::gfx::DepthSlot)>| {
        d.map(|(_, d)| [d.info.min, d.info.max, d.info.near, d.info.far]).unwrap_or([0.0, 1.0, 1.0, 2.0])
    };
    let mut v: Vec<f32> = Vec::with_capacity(56);
    v.extend(ct);
    v.extend(q4(cpose));
    v.extend(p4(cpose, if has_depth { 1.0 } else { 0.0 }));
    v.extend(q4(&views[0].0));
    v.extend(q4(&views[1].0));
    v.extend(p4(&views[0].0, 0.0));
    v.extend(p4(&views[1].0, 0.0));
    v.extend(tans(views[0].1));
    v.extend(tans(views[1].1));
    v.extend(rect(dl));
    v.extend(rect(dr));
    v.extend(info(dl));
    v.extend(info(dr));
    v.extend([pick, 0.0, 0.0, 0.0]);
    let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
    if context.Map(cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped)).is_ok() {
        std::ptr::copy_nonoverlapping(v.as_ptr(), mapped.pData as *mut f32, v.len());
        context.Unmap(cb, 0);
    }
    context.OMSetRenderTargets(Some(&[Some(rtv.clone())]), None);
    context.RSSetViewports(Some(&[vp]));
    context.VSSetShader(vs, None);
    context.PSSetShader(cps, None);
    context.PSSetConstantBuffers(2, Some(&[Some(cb.clone())]));
    let depth_srv = |d: Option<(usize, &crate::gfx::DepthSlot)>| {
        d.and_then(|(i, _)| opened_depth[i].as_ref()).map(|o| o.srvs[latest % o.srvs.len()].clone())
    };
    let srvs = if has_depth { [Some(comp.4.clone()), depth_srv(dl), depth_srv(dr)] } else { [Some(comp.4.clone()), None, None] };
    context.PSSetShaderResources(0, Some(&srvs));
    context.Draw(3, 0);
    context.PSSetShaderResources(0, Some(&[None, None, None]));
}

struct OpenedQuad {
    generation: u64,
    srvs: Vec<ID3D11ShaderResourceView>,
}

/// A layer's picture with a full mip chain, so layers seen small or at a
/// slant are filtered instead of aliasing: the newest ring texture is copied
/// into mip 0 and the rest generated, once per new frame.
struct MippedLayer {
    generation: u64,
    rings: Vec<ID3D11Texture2D>,
    tex: ID3D11Texture2D,
    srv: ID3D11ShaderResourceView,
    /// The ring index last copied in (a new frame always moves it on).
    filled: Option<usize>,
}

/// A mipmapped copy of a ring texture's size and format, or None if the
/// format cannot have generated mips (the layer is then drawn from mip 0 only).
unsafe fn make_mipped(device: &ID3D11Device, ring: &ID3D11Texture2D) -> Option<(ID3D11Texture2D, ID3D11ShaderResourceView)> {
    let mut d = D3D11_TEXTURE2D_DESC::default();
    ring.GetDesc(&mut d);
    if d.Width < 2 && d.Height < 2 {
        return None;
    }
    let desc = D3D11_TEXTURE2D_DESC {
        MipLevels: 0,
        ArraySize: 1,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: D3D11_RESOURCE_MISC_GENERATE_MIPS.0 as u32,
        ..d
    };
    let mut tex = None;
    if let Err(e) = device.CreateTexture2D(&desc, None, Some(&mut tex)) {
        xlog!("mipmapped layer texture {}x{} format {} failed: {e:?}", d.Width, d.Height, d.Format.0);
        return None;
    }
    let tex = tex?;
    let mut srv = None;
    device.CreateShaderResourceView(&tex, None, Some(&mut srv)).ok()?;
    Some((tex, srv?))
}

struct Opened {
    generation: u64,
    srvs: Vec<ID3D11ShaderResourceView>,
    fence: ID3D11Fence,
    aspect: f32,
    srgb_out: bool,
}

fn run(luid: LUID, sim: Arc<Mutex<SimState>>, shared: Arc<Shared>) -> windows::core::Result<()> {
    unsafe {
        SHARED.with(|s| *s.borrow_mut() = Some(shared.clone()));
        let hidden = std::env::var_os("XRMIMIC_HIDDEN").is_some_and(|v| v != "0");
        let hinst = GetModuleHandleW(None)?;
        let class = WNDCLASSEXW {
            cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
            style: CS_HREDRAW | CS_VREDRAW,
            lpfnWndProc: Some(wndproc),
            hInstance: hinst.into(),
            hCursor: LoadCursorW(None, IDC_ARROW)?,
            hbrBackground: HBRUSH((15 + 1) as _), // COLOR_BTNFACE + 1,
            lpszClassName: w!("xrmimic-headset"),
            ..Default::default()
        };
        RegisterClassExW(&class);
        let view_class = WNDCLASSEXW { hbrBackground: HBRUSH::default(), lpszClassName: w!("xrmimic-view"), ..class };
        RegisterClassExW(&view_class);
        let style = WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN;
        // The view as before (1464 wide) plus the panel, within the screen.
        let screen_w = GetSystemMetrics(SM_CXSCREEN);
        let client_w = (1464 + PANEL_W).min(screen_w * 95 / 100).max(1000);
        let mut r = RECT { left: 0, top: 0, right: client_w, bottom: 768 + TOOLBAR };
        let _ = AdjustWindowRect(&mut r, style, false);
        // Tests only: a snapshot of the window, taken off-screen.
        let snapshot = std::env::var_os("XRMIMIC_TEST_SNAPSHOT").map(std::path::PathBuf::from);
        let (x, y) = if snapshot.is_some() { (-3000, -3000) } else { (CW_USEDEFAULT, CW_USEDEFAULT) };
        let mut snapshot_at: Option<Instant> = None;
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("xrmimic-headset"),
            w!("XRMimic"),
            style,
            x,
            y,
            r.right - r.left,
            r.bottom - r.top,
            None,
            None,
            Some(hinst.into()),
            None,
        )?;
        let combo = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("COMBOBOX"),
            None,
            WS_CHILD | WS_VISIBLE | WS_VSCROLL | WS_TABSTOP | WINDOW_STYLE(CBS_DROPDOWNLIST),
            312,
            6,
            840,
            320,
            Some(hwnd),
            Some(HMENU(ID_COMBO as _)),
            Some(hinst.into()),
            None,
        )?;
        let font = GetStockObject(DEFAULT_GUI_FONT);
        SendMessageW(combo, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        // Depth view: each eye's depth buffer (XR_KHR_composition_layer_depth)
        // as greys instead of its colours.
        let depth_box = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("Depth"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX),
            1164,
            8,
            70,
            20,
            Some(hwnd),
            Some(HMENU(ID_DEPTH as _)),
            Some(hinst.into()),
            None,
        )?;
        SendMessageW(depth_box, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        // Tests only: start in the depth view.
        if std::env::var_os("XRMIMIC_TEST_DEPTH_VIEW").is_some_and(|v| v != "0") {
            SendMessageW(depth_box, BM_SETCHECK, Some(WPARAM(1)), None);
        }
        // Centre view: one picture from between the eyes instead of two.
        let centre_box = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("Centre"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX),
            1240,
            8,
            70,
            20,
            Some(hwnd),
            Some(HMENU(ID_CENTRE as _)),
            Some(hinst.into()),
            None,
        )?;
        SendMessageW(centre_box, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        if std::env::var_os("XRMIMIC_TEST_CENTRE_VIEW").is_some_and(|v| v != "0") {
            SendMessageW(centre_box, BM_SETCHECK, Some(WPARAM(1)), None);
        }
        // Reproject: each eye redrawn from where it is now (positional
        // timewarp through the depth buffers, rotation only without them).
        let reproject_box = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("Reproject"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX),
            1160,
            8,
            82,
            20,
            Some(hwnd),
            Some(HMENU(ID_REPROJECT as _)),
            Some(hinst.into()),
            None,
        )?;
        SendMessageW(reproject_box, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        if std::env::var_os("XRMIMIC_TEST_REPROJECT").is_some_and(|v| v != "0") {
            SendMessageW(reproject_box, BM_SETCHECK, Some(WPARAM(1)), None);
        }
        // The panel with the Logs, Device and Input tabs; untick for a wider view.
        let panel_box = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("BUTTON"),
            w!("Panel"),
            WS_CHILD | WS_VISIBLE | WS_TABSTOP | WINDOW_STYLE(BS_AUTOCHECKBOX),
            0,
            8,
            80,
            20,
            Some(hwnd),
            Some(HMENU(ID_PANEL as _)),
            Some(hinst.into()),
            None,
        )?;
        SendMessageW(panel_box, WM_SETFONT, Some(WPARAM(font.0 as usize)), Some(LPARAM(1)));
        // Tests that photograph the view start without the panel (their
        // pictures assume the view fills the window); tab tests keep it.
        let panel_default = std::env::var("XRMIMIC_TEST_PANEL")
            .map(|v| v != "0")
            .unwrap_or(snapshot.is_none() || std::env::var_os("XRMIMIC_TEST_TAB").is_some());
        SendMessageW(panel_box, BM_SETCHECK, Some(WPARAM(panel_default as usize)), None);
        let view = CreateWindowExW(
            WINDOW_EX_STYLE(0),
            w!("xrmimic-view"),
            None,
            WS_CHILD | WS_VISIBLE,
            0,
            TOOLBAR,
            1464,
            768,
            Some(hwnd),
            None,
            Some(hinst.into()),
            None,
        )?;
        HWNDS.with(|h| {
            *h.borrow_mut() =
                Some(Hwnds {
                    frame: hwnd,
                    view,
                    combo,
                    depth: depth_box,
                    centre: centre_box,
                    reproject: reproject_box,
                    panel: panel_box,
                })
        });
        fill_combo();
        crate::logview::create(hwnd, hinst.into(), windows::Win32::Graphics::Gdi::HFONT(font.0))?;
        crate::devicetab::create(hwnd, hinst.into(), windows::Win32::Graphics::Gdi::HFONT(font.0))?;
        crate::inputtab::create(hwnd, hinst.into(), windows::Win32::Graphics::Gdi::HFONT(font.0), shared.clone())?;
        // The Input tab is the one shown first.
        crate::logview::select_tab(2);
        layout_frame(client_w, 768 + TOOLBAR);
        // Tests only: apply a preset through the Device tab, as a click would.
        if let Ok(p) = std::env::var("XRMIMIC_TEST_DEVICE_APPLY") {
            crate::devicetab::apply_preset_for_test(&p);
        }
        // Tests only: open on a given tab.
        if std::env::var("XRMIMIC_TEST_TAB").is_ok_and(|t| t == "input") {
            crate::logview::select_tab(2);
            show_tab(2);
        }
        if std::env::var("XRMIMIC_TEST_TAB").is_ok_and(|t| t == "device") {
            crate::logview::select_tab(1);
            show_tab(1);
        }
        if let Some(t) = std::env::var("XRMIMIC_TEST_TAB").ok().filter(|t| t.starts_with("logs")) {
            crate::logview::set_hide_per_frame(t == "logs-hide-frame");
            crate::logview::select_tab(0);
            show_tab(0);
        }
        let mut logs_at = Instant::now();
        let switch_result: Arc<Mutex<Option<Result<(), String>>>> = Arc::default();
        let mut switching = false;
        if !hidden || snapshot.is_some() {
            let _ = ShowWindow(hwnd, SW_SHOWNOACTIVATE);
        }
        // XRMIMIC_TOPMOST=1: keep the window above others (it opens without taking
        // focus, so otherwise it can start behind the window you are using).
        if std::env::var_os("XRMIMIC_TOPMOST").is_some_and(|v| v != "0") && snapshot.is_none() {
            let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), 0, 0, 0, 0, SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE);
        }

        // Own device on the app's adapter, so the shared texture opens directly.
        let factory: IDXGIFactory4 = CreateDXGIFactory1()?;
        let adapter: IDXGIAdapter1 = factory.EnumAdapterByLuid(luid)?;
        let mut device = None;
        let mut context = None;
        D3D11CreateDevice(
            &adapter,
            D3D_DRIVER_TYPE_UNKNOWN,
            HMODULE::default(),
            D3D11_CREATE_DEVICE_BGRA_SUPPORT,
            Some(&[D3D_FEATURE_LEVEL_11_0]),
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        )?;
        let device: ID3D11Device = device.unwrap();
        let device1: ID3D11Device1 = device.cast()?;
        let device5: ID3D11Device5 = device.cast()?;
        let context: ID3D11DeviceContext = context.unwrap();
        let context4: ID3D11DeviceContext4 = context.cast()?;

        let sc_desc = DXGI_SWAP_CHAIN_DESC1 {
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            BufferUsage: DXGI_USAGE_RENDER_TARGET_OUTPUT,
            BufferCount: 2,
            SwapEffect: DXGI_SWAP_EFFECT_FLIP_DISCARD,
            ..Default::default()
        };
        let swapchain = factory.CreateSwapChainForHwnd(&device, view, &sc_desc, None, None)?;
        let _ = factory.MakeWindowAssociation(hwnd, DXGI_MWA_NO_ALT_ENTER);

        let vs_code = compile(s!("vs"), s!("vs_5_0"))?;
        let ps_code = compile(s!("ps"), s!("ps_5_0"))?;
        let qvs_code = compile(s!("qvs"), s!("vs_5_0"))?;
        let qps_code = compile(s!("qps"), s!("ps_5_0"))?;
        let gvs_code = compile(s!("gvs"), s!("vs_5_0"))?;
        let gps_code = compile(s!("gps"), s!("ps_5_0"))?;
        let dps_code = compile(s!("dps"), s!("ps_5_0"))?;
        // The centre view's shader is compiled the first time it is needed.
        let mut cps: Option<ID3D11PixelShader> = None;
        let mut centre_cb = None;
        device.CreateBuffer(
            &D3D11_BUFFER_DESC {
                ByteWidth: 14 * 16,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                ..Default::default()
            },
            None,
            Some(&mut centre_cb),
        )?;
        let centre_cb = centre_cb.unwrap();
        // The centre view's off-screen picture: both eyes side by side, with
        // their layers, at eye resolution. (width, height, sRGB, target, read)
        type Comp = (u32, u32, bool, ID3D11RenderTargetView, ID3D11ShaderResourceView);
        let make_comp = |w: u32, h: u32, srgb: bool| -> windows::core::Result<Comp> {
            let desc = D3D11_TEXTURE2D_DESC {
                Width: w,
                Height: h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_TYPELESS,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut tex = None;
            device.CreateTexture2D(&desc, None, Some(&mut tex))?;
            let tex = tex.unwrap();
            let f = if srgb { DXGI_FORMAT_B8G8R8A8_UNORM_SRGB } else { DXGI_FORMAT_B8G8R8A8_UNORM };
            let rd = D3D11_RENDER_TARGET_VIEW_DESC {
                Format: f,
                ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 } },
            };
            let mut rtv = None;
            device.CreateRenderTargetView(&tex, Some(&rd), Some(&mut rtv))?;
            let sd = D3D11_SHADER_RESOURCE_VIEW_DESC {
                Format: f,
                ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 },
                },
            };
            let mut srv = None;
            device.CreateShaderResourceView(&tex, Some(&sd), Some(&mut srv))?;
            Ok((w, h, srgb, rtv.unwrap(), srv.unwrap()))
        };
        let mut comp: Option<Comp> = None;
        let mut dps = None;
        device.CreatePixelShader(&dps_code, None, Some(&mut dps))?;
        let mut depth_cb = None;
        device.CreateBuffer(
            &D3D11_BUFFER_DESC {
                ByteWidth: 32,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                ..Default::default()
            },
            None,
            Some(&mut depth_cb),
        )?;
        let depth_cb = depth_cb.unwrap();
        let mut opened_depth: Vec<Option<OpenedQuad>> = Vec::new();
        // Which way up the app's depth is (crate::depthwatch), made on first use.
        let mut depth_watch: Option<Option<crate::depthwatch::DepthWatch>> = None;
        let mut qvs = None;
        let mut qps = None;
        device.CreateVertexShader(&qvs_code, None, Some(&mut qvs))?;
        device.CreatePixelShader(&qps_code, None, Some(&mut qps))?;
        let (mut gvs, mut gps) = (None, None);
        device.CreateVertexShader(&gvs_code, None, Some(&mut gvs))?;
        device.CreatePixelShader(&gps_code, None, Some(&mut gps))?;
        // Quad layer state: a matrix per draw, no culling, three blend modes.
        let mut quad_cb = None;
        device.CreateBuffer(
            &D3D11_BUFFER_DESC {
                ByteWidth: 96,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                ..Default::default()
            },
            None,
            Some(&mut quad_cb),
        )?;
        let quad_cb = quad_cb.unwrap();
        let mut layer_depth_cb = None;
        device.CreateBuffer(
            &D3D11_BUFFER_DESC {
                ByteWidth: 4 * 16,
                Usage: D3D11_USAGE_DYNAMIC,
                BindFlags: D3D11_BIND_CONSTANT_BUFFER.0 as u32,
                CPUAccessFlags: D3D11_CPU_ACCESS_WRITE.0 as u32,
                ..Default::default()
            },
            None,
            Some(&mut layer_depth_cb),
        )?;
        let layer_depth_cb = layer_depth_cb.unwrap();
        let mut no_cull = None;
        device.CreateRasterizerState(
            &D3D11_RASTERIZER_DESC {
                FillMode: D3D11_FILL_SOLID,
                CullMode: D3D11_CULL_NONE,
                DepthClipEnable: true.into(),
                ..Default::default()
            },
            Some(&mut no_cull),
        )?;
        let blend_state = |src: D3D11_BLEND| -> windows::core::Result<Option<ID3D11BlendState>> {
            let mut desc = D3D11_BLEND_DESC::default();
            desc.RenderTarget[0] = D3D11_RENDER_TARGET_BLEND_DESC {
                BlendEnable: true.into(),
                SrcBlend: src,
                DestBlend: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOp: D3D11_BLEND_OP_ADD,
                SrcBlendAlpha: D3D11_BLEND_ONE,
                DestBlendAlpha: D3D11_BLEND_INV_SRC_ALPHA,
                BlendOpAlpha: D3D11_BLEND_OP_ADD,
                RenderTargetWriteMask: D3D11_COLOR_WRITE_ENABLE_ALL.0 as u8,
            };
            let mut s = None;
            device.CreateBlendState(&desc, Some(&mut s))?;
            Ok(s)
        };
        let premultiplied = blend_state(D3D11_BLEND_ONE)?;
        let straight = blend_state(D3D11_BLEND_SRC_ALPHA)?;
        let mut opened_quads: Vec<Option<OpenedQuad>> = Vec::new();
        let mut mipped_quads: Vec<Option<MippedLayer>> = Vec::new();
        let mut vs = None;
        let mut ps = None;
        device.CreateVertexShader(&vs_code, None, Some(&mut vs))?;
        device.CreatePixelShader(&ps_code, None, Some(&mut ps))?;
        let sdesc = D3D11_SAMPLER_DESC {
            // Anisotropic: layers are often seen at a slant (a cylinder's sides,
            // an equirect band); trilinear between the layers' mips.
            Filter: D3D11_FILTER_ANISOTROPIC,
            MaxAnisotropy: 8,
            AddressU: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressV: D3D11_TEXTURE_ADDRESS_CLAMP,
            AddressW: D3D11_TEXTURE_ADDRESS_CLAMP,
            MaxLOD: f32::MAX,
            ..Default::default()
        };
        let mut sampler = None;
        device.CreateSamplerState(&sdesc, Some(&mut sampler))?;

        let mut opened: Option<Opened> = None;
        let mut title_at = Instant::now();
        let mut frames_at_title = 0u64;
        let mut size = (1464u32, 768u32);
        // Tests only: ask for a runtime switch once frames arrive, as a pick would.
        let mut test_switch = std::env::var("XRMIMIC_TEST_SWITCH_TO").ok();
        xlog!("window up (hidden: {hidden})");

        while !shared.stop.load(Ordering::SeqCst) {
            let mut msg = MSG::default();
            while PeekMessageW(&mut msg, None, 0, 0, PM_REMOVE).as_bool() {
                let _ = TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
            // Runtime list: a pick switches the system runtime (UAC prompt) off-thread.
            let mut pick: Option<String> = None;
            if let Some(i) = with_input(|i| i.choose.take()) {
                match ENTRIES.with(|e| e.borrow().get(i).cloned()) {
                    Some(e) if !e.active && !switching => pick = Some(e.manifest),
                    _ => fill_combo(),
                }
            }
            if !switching && test_switch.is_some() && shared.slot.lock().unwrap().frames > 0 {
                pick = pick.or_else(|| test_switch.take());
            }
            if let Some(manifest) = pick {
                switching = true;
                xlog!("switching the system runtime to {manifest}");
                let result = switch_result.clone();
                let frame = hwnd.0 as isize;
                std::thread::spawn(move || {
                    let r = runtimes::set_active(&manifest);
                    *result.lock().unwrap() = Some(r);
                    let _ = PostMessageW(Some(HWND(frame as _)), WM_RUNTIME_SWITCHED, WPARAM(0), LPARAM(0));
                });
            }
            if with_input(|i| std::mem::take(&mut i.refresh)) {
                switching = false;
                match switch_result.lock().unwrap().take() {
                    Some(Ok(())) => xlog!("runtime switch: ok"),
                    Some(Err(e)) => xlog!("runtime switch failed, unchanged: {e}"),
                    None => {}
                }
                fill_combo();
            }

            crate::inputtab::tick(&sim);
            // Input -> simulated user.
            let focused = GetForegroundWindow() == hwnd;
            let (frame, toggle, resized) = with_input(|i| {
                let f = input_frame(i, focused);
                i.mouse_dx = 0.0;
                i.mouse_dy = 0.0;
                i.pan_dx = 0.0;
                i.pan_dy = 0.0;
                i.zoom = 0.0;
                i.hand = [0.0; 5];
                i.hand_reset = false;
                (f, std::mem::take(&mut i.toggle), i.resized.take())
            });
            shared.input.lock().unwrap().update(&frame, toggle);
            let active = sim.lock().unwrap().active_controller;

            if let Some((w, h)) = resized {
                if w > 0 && h > 0 && (w, h) != size {
                    size = (w, h);
                    let _ = swapchain.ResizeBuffers(0, w, h, DXGI_FORMAT_UNKNOWN, DXGI_SWAP_CHAIN_FLAG(0));
                }
            }

            // (Re)open the shared texture when the runtime made a new one.
            let (handles, fence_handle, latest, latest_value, generation, eye_w, eye_h, format, frames, views, quads) = {
                let s = shared.slot.lock().unwrap();
                (
                    s.handles.clone(),
                    s.fence,
                    s.latest,
                    s.latest_value,
                    s.generation,
                    s.eye_w,
                    s.eye_h,
                    s.format,
                    s.frames,
                    s.views.clone(),
                    s.quads.clone(),
                )
            };
            // Depth buffers' textures, likewise, read through their depth format.
            let depth = shared.slot.lock().unwrap().depth.clone();
            opened_depth.resize_with(depth.len(), || None);
            for (i, d) in depth.iter().enumerate() {
                if opened_depth[i].as_ref().map(|o| o.generation) == Some(d.generation) {
                    continue;
                }
                let sd = D3D11_SHADER_RESOURCE_VIEW_DESC {
                    Format: DXGI_FORMAT(d.srv_format),
                    ViewDimension: D3D11_SRV_DIMENSION_TEXTURE2D,
                    Anonymous: D3D11_SHADER_RESOURCE_VIEW_DESC_0 {
                        Texture2D: D3D11_TEX2D_SRV { MostDetailedMip: 0, MipLevels: 1 },
                    },
                };
                let mut srvs = Vec::new();
                for h in &d.handles {
                    let srv = device1.OpenSharedResource1::<ID3D11Texture2D>(HANDLE(*h as _)).and_then(|tex| {
                        let mut srv = None;
                        device.CreateShaderResourceView(&tex, Some(&sd), Some(&mut srv)).map(|()| srv.unwrap())
                    });
                    match srv {
                        Ok(s) => srvs.push(s),
                        Err(e) => {
                            xlog!("opening depth buffer {i} failed: {e:?}");
                            break;
                        }
                    }
                }
                opened_depth[i] =
                    (srvs.len() == d.handles.len()).then_some(OpenedQuad { generation: d.generation, srvs });
            }
            // Quad layers' textures, (re)opened when the runtime made new ones.
            opened_quads.resize_with(quads.len(), || None);
            mipped_quads.resize_with(quads.len(), || None);
            for (i, q) in quads.iter().enumerate() {
                if opened_quads[i].as_ref().map(|o| o.generation) == Some(q.generation) {
                    continue;
                }
                let mut srvs = Vec::new();
                let mut rings = Vec::new();
                for h in &q.handles {
                    let srv = device1
                        .OpenSharedResource1::<ID3D11Texture2D>(HANDLE(*h as _))
                        .and_then(|tex| {
                            let mut srv = None;
                            device.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
                            rings.push(tex);
                            Ok(srv.unwrap())
                        });
                    match srv {
                        Ok(s) => srvs.push(s),
                        Err(e) => {
                            xlog!("opening quad layer {i} failed: {e:?}");
                            break;
                        }
                    }
                }
                let ok = srvs.len() == q.handles.len();
                opened_quads[i] = ok.then_some(OpenedQuad { generation: q.generation, srvs });
                // Cube maps keep mip 0: their faces sit side by side, and smaller
                // mips would bleed one face into the next.
                let cube = matches!(q.meta.shape, crate::shared::Shape::Cube);
                mipped_quads[i] = if ok && !cube {
                    make_mipped(&device, &rings[0]).map(|(tex, srv)| MippedLayer {
                        generation: q.generation,
                        rings,
                        tex,
                        srv,
                        filled: None,
                    })
                } else {
                    None
                };
            }
            if generation != 0 && opened.as_ref().map(|o| o.generation) != Some(generation) {
                let open = || -> windows::core::Result<(Vec<ID3D11ShaderResourceView>, ID3D11Fence)> {
                    let mut srvs = Vec::new();
                    for h in &handles {
                        let tex: ID3D11Texture2D = device1.OpenSharedResource1(HANDLE(*h as _))?;
                        let mut srv = None;
                        device.CreateShaderResourceView(&tex, None, Some(&mut srv))?;
                        srvs.push(srv.unwrap());
                    }
                    let mut fence: Option<ID3D11Fence> = None;
                    device5.OpenSharedFence(HANDLE(fence_handle as _), &mut fence)?;
                    Ok((srvs, fence.unwrap()))
                };
                match open() {
                    Ok((srvs, fence)) => {
                        let f = DXGI_FORMAT(format);
                        let srgb_out = matches!(
                            f,
                            DXGI_FORMAT_R8G8B8A8_UNORM_SRGB
                                | DXGI_FORMAT_B8G8R8A8_UNORM_SRGB
                                | DXGI_FORMAT_R16G16B16A16_FLOAT
                        );
                        opened = Some(Opened {
                            generation,
                            srvs,
                            fence,
                            aspect: (eye_w * 2) as f32 / eye_h.max(1) as f32,
                            srgb_out,
                        });
                        xlog!("window opened frame texture generation {generation}");
                    }
                    Err(e) => xlog!("OpenSharedResource1 failed: {e:?}"),
                }
            }

            // Draw.
            let back: ID3D11Texture2D = swapchain.GetBuffer(0)?;
            let srgb = opened.as_ref().is_some_and(|o| o.srgb_out);
            let rtv_desc = D3D11_RENDER_TARGET_VIEW_DESC {
                Format: if srgb { DXGI_FORMAT_B8G8R8A8_UNORM_SRGB } else { DXGI_FORMAT_B8G8R8A8_UNORM },
                ViewDimension: D3D11_RTV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_RENDER_TARGET_VIEW_DESC_0 { Texture2D: D3D11_TEX2D_RTV { MipSlice: 0 } },
            };
            let mut rtv = None;
            device.CreateRenderTargetView(&back, Some(&rtv_desc), Some(&mut rtv))?;
            let rtv = rtv.unwrap();
            context.ClearRenderTargetView(&rtv, &[0.06, 0.07, 0.09, 1.0]);
            if let Some(o) = &opened {
                let (cw, ch) = (size.0 as f32, size.1 as f32);
                let (mut vw, mut vh) = (cw, cw / o.aspect);
                if vh > ch {
                    vh = ch;
                    vw = ch * o.aspect;
                }
                let vp = D3D11_VIEWPORT {
                    TopLeftX: (cw - vw) * 0.5,
                    TopLeftY: (ch - vh) * 0.5,
                    Width: vw,
                    Height: vh,
                    MinDepth: 0.0,
                    MaxDepth: 1.0,
                };
                // Centre view: everything below is drawn off-screen at eye
                // resolution, then one camera between the eyes reads it.
                let centre_on = SendMessageW(centre_box, BM_GETCHECK, None, None).0 == 1 && views.len() == 2;
                let reproject_on =
                    !centre_on && SendMessageW(reproject_box, BM_GETCHECK, None, None).0 == 1 && views.len() == 2;
                // Both draw the eyes off-screen first, then read them back.
                let centre_on = centre_on || reproject_on;
                if centre_on && comp.as_ref().map(|c| (c.0, c.1, c.2)) != Some((eye_w * 2, eye_h, o.srgb_out)) {
                    comp = match make_comp(eye_w * 2, eye_h, o.srgb_out) {
                        Ok(c) => Some(c),
                        Err(e) => {
                            xlog!("centre view texture failed: {e:?}");
                            None
                        }
                    };
                }
                if centre_on && cps.is_none() {
                    let t = Instant::now();
                    match compile(s!("cps"), s!("ps_5_0")) {
                        Ok(code) => {
                            let _ = device.CreatePixelShader(&code, None, Some(&mut cps));
                            xlog!("centre view shader ready in {:.0} ms", t.elapsed().as_secs_f64() * 1e3);
                        }
                        Err(e) => xlog!("centre view shader failed: {e:?}"),
                    }
                }
                let centre = centre_on && comp.is_some() && cps.is_some();
                let screen_vp = vp;
                let (draw_rtv, vp) = match (&comp, centre) {
                    (Some(c), true) => (
                        c.3.clone(),
                        D3D11_VIEWPORT {
                            TopLeftX: 0.0,
                            TopLeftY: 0.0,
                            Width: (eye_w * 2) as f32,
                            Height: eye_h as f32,
                            MinDepth: 0.0,
                            MaxDepth: 1.0,
                        },
                    ),
                    _ => (rtv.clone(), vp),
                };
                if latest_value > 0 {
                    // GPU-side wait for the app's copy into this slot; the CPU
                    // and the app carry on.
                    let _ = context4.Wait(&o.fence, latest_value);
                    // Is the app's depth upside down against its colour? Turn it
                    // over if the pictures say so (unless XRMIMIC_DEPTH_FLIP fixed it).
                    if shared.depth_flip_auto.load(Ordering::Relaxed) {
                        let eye = |e: usize| {
                            depth.iter().enumerate().find_map(|(i, d)| {
                                let od = opened_depth.get(i)?.as_ref()?;
                                (d.eye == e).then(|| (od.srvs[latest % od.srvs.len()].clone(), d))
                            })
                        };
                        if let (Some((s0, d0)), Some((s1, d1)), Some(smp)) = (eye(0), eye(1), sampler.as_ref()) {
                            let watch = depth_watch.get_or_insert_with(|| crate::depthwatch::DepthWatch::new(&device));
                            if let Some(w) = watch.as_mut() {
                                use crate::depthwatch::EyeDepth;
                                let eyes = [EyeDepth { srv: &s0, slot: d0 }, EyeDepth { srv: &s1, slot: d1 }];
                                if w.tick(&context, smp, &o.srvs[latest % o.srvs.len()], eyes) {
                                    shared.depth_flip.fetch_xor(true, Ordering::Relaxed);
                                }
                            }
                        }
                    }
                    context.OMSetRenderTargets(Some(&[Some(draw_rtv.clone())]), None);
                    context.RSSetViewports(Some(&[vp]));
                    context.IASetPrimitiveTopology(D3D11_PRIMITIVE_TOPOLOGY_TRIANGLELIST);
                    context.VSSetShader(vs.as_ref(), None);
                    context.PSSetShader(ps.as_ref(), None);
                    context.PSSetShaderResources(0, Some(&[Some(o.srvs[latest % o.srvs.len()].clone())]));
                    context.PSSetSamplers(0, Some(&[sampler.clone()]));
                    context.Draw(3, 0);
                    // Quad layers, in order, over each eye, from that eye's pose.
                    if views.len() == 2 && !quads.is_empty() {
                        // Fill each layer's mip chain from its newest picture.
                        for m in mipped_quads.iter_mut().flatten() {
                            let k = latest % m.rings.len();
                            if m.filled != Some(k) {
                                context.CopySubresourceRegion(&m.tex, 0, 0, 0, 0, &m.rings[k], 0, None);
                                context.GenerateMips(&m.srv);
                                m.filled = Some(k);
                            }
                        }
                        context.VSSetShader(qvs.as_ref(), None);
                        context.PSSetShader(qps.as_ref(), None);
                        context.RSSetState(no_cull.as_ref());
                        context.VSSetConstantBuffers(0, Some(&[Some(quad_cb.clone())]));
                        for (e, (eye, fov)) in views.iter().enumerate() {
                            let half = D3D11_VIEWPORT {
                                TopLeftX: vp.TopLeftX + e as f32 * vp.Width * 0.5,
                                Width: vp.Width * 0.5,
                                ..vp
                            };
                            context.RSSetViewports(Some(&[half]));
                            for (i, q) in quads.iter().enumerate() {
                                let visible = if e == 0 { q.meta.left } else { q.meta.right };
                                let Some(oq) = opened_quads.get(i).and_then(|o| o.as_ref()) else { continue };
                                if !visible {
                                    continue;
                                }
                                use crate::shared::Shape;
                                let mut pose = q.meta.pose;
                                let (size, shape, vertices) = match q.meta.shape {
                                    Shape::Quad { width, height } => ((width, height), [0.0; 8], 6),
                                    Shape::Cylinder { radius, central_angle, height } => {
                                        ((1.0, 1.0), [1.0, radius, central_angle, height, 0.0, 0.0, 0.0, 0.0], 6 * 64)
                                    }
                                    Shape::Equirect { radius, h_angle, upper, lower, infinite } => {
                                        if infinite {
                                            pose.position = eye.position;
                                        }
                                        ((1.0, 1.0), [2.0, radius, h_angle, upper, lower, 0.0, 0.0, 0.0], 6 * 64 * 32)
                                    }
                                    Shape::Cube => {
                                        pose.position = eye.position;
                                        let half = xrmimic_core::math::FAR_SPHERE_RADIUS;
                                        ((1.0, 1.0), [3.0, half, 0.0, 0.0, 0.0, 0.0, 0.0, 0.0], 36)
                                    }
                                };
                                let m = xrmimic_core::math::quad_clip_matrix(eye, *fov, &pose, size);
                                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                                if context.Map(&quad_cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped)).is_ok() {
                                    std::ptr::copy_nonoverlapping(m.as_ptr(), mapped.pData as *mut f32, 16);
                                    std::ptr::copy_nonoverlapping(shape.as_ptr(), (mapped.pData as *mut f32).add(16), 8);
                                    context.Unmap(&quad_cb, 0);
                                }
                                let blend = match q.meta.blend {
                                    crate::shared::Blend::Opaque => None,
                                    crate::shared::Blend::Premultiplied => premultiplied.as_ref(),
                                    crate::shared::Blend::Straight => straight.as_ref(),
                                };
                                context.OMSetBlendState(blend, None, 0xffff_ffff);
                                let srv = match mipped_quads.get(i).and_then(|m| m.as_ref()) {
                                    Some(m) if m.generation == oq.generation && m.filled.is_some() => m.srv.clone(),
                                    _ => oq.srvs[latest % oq.srvs.len()].clone(),
                                };
                                context.PSSetShaderResources(0, Some(&[Some(srv)]));
                                // Depth-tested layers: against this eye's depth, if it sent one.
                                let eye_depth = q.meta.depth_test.and_then(|op| {
                                    let (i, d) = depth.iter().enumerate().find(|(_, d)| d.eye == e)?;
                                    let od = opened_depth.get(i)?.as_ref()?;
                                    Some((op, d, od.srvs[latest % od.srvs.len()].clone()))
                                });
                                let mut values = [0f32; 16];
                                if let Some((op, d, _)) = &eye_depth {
                                    let r = d.rect;
                                    values = [
                                        1.0,
                                        *op as f32,
                                        0.0,
                                        0.0,
                                        r[0] as f32,
                                        r[1] as f32,
                                        r[2] as f32,
                                        r[3] as f32,
                                        d.info.min,
                                        d.info.max,
                                        d.info.near,
                                        d.info.far,
                                        half.TopLeftX,
                                        half.TopLeftY,
                                        half.Width,
                                        half.Height,
                                    ];
                                }
                                let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                                if context.Map(&layer_depth_cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped)).is_ok() {
                                    std::ptr::copy_nonoverlapping(values.as_ptr(), mapped.pData as *mut f32, 16);
                                    context.Unmap(&layer_depth_cb, 0);
                                }
                                context.PSSetConstantBuffers(3, Some(&[Some(layer_depth_cb.clone())]));
                                context.PSSetShaderResources(3, Some(&[eye_depth.map(|(_, _, srv)| srv)]));
                                context.Draw(vertices, 0);
                            }
                        }
                        context.OMSetBlendState(None, None, 0xffff_ffff);
                        context.RSSetState(None);
                        context.PSSetShaderResources(3, Some(&[None]));
                    }
                    // The play-area boundary, where the head comes near it.
                    let [pw, pd] = crate::devstore::current().play_area;
                    let head = sim.lock().unwrap().head_pose().position;
                    let gap = (pw * 0.5 - head.x.abs()).min(pd * 0.5 - head.z.abs());
                    if views.len() == 2 && pw > 0.0 && pd > 0.0 && gap < 1.2 {
                        context.VSSetShader(gvs.as_ref(), None);
                        context.PSSetShader(gps.as_ref(), None);
                        context.RSSetState(no_cull.as_ref());
                        context.OMSetBlendState(straight.as_ref(), None, 0xffff_ffff);
                        context.VSSetConstantBuffers(0, Some(&[Some(quad_cb.clone())]));
                        context.PSSetConstantBuffers(0, Some(&[Some(quad_cb.clone())]));
                        for (e, (eye, fov)) in views.iter().enumerate() {
                            context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                                TopLeftX: vp.TopLeftX + e as f32 * vp.Width * 0.5,
                                Width: vp.Width * 0.5,
                                ..vp
                            }]));
                            let m = xrmimic_core::math::quad_clip_matrix(eye, *fov, &xrmimic_core::math::Pose::IDENTITY, (1.0, 1.0));
                            let shape = [pw * 0.5, pd * 0.5, 0.0, 0.0, head.x, head.y, head.z, 0.0];
                            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                            if context.Map(&quad_cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped)).is_ok() {
                                std::ptr::copy_nonoverlapping(m.as_ptr(), mapped.pData as *mut f32, 16);
                                std::ptr::copy_nonoverlapping(shape.as_ptr(), (mapped.pData as *mut f32).add(16), 8);
                                context.Unmap(&quad_cb, 0);
                            }
                            context.Draw(24, 0);
                        }
                        context.OMSetBlendState(None, None, 0xffff_ffff);
                        context.RSSetState(None);
                        context.RSSetViewports(Some(&[vp]));
                    }
                    // Depth view: over everything, each eye that sent depth.
                    if SendMessageW(depth_box, BM_GETCHECK, None, None).0 == 1 {
                        context.VSSetShader(vs.as_ref(), None);
                        context.PSSetShader(dps.as_ref(), None);
                        context.PSSetConstantBuffers(1, Some(&[Some(depth_cb.clone())]));
                        for (i, d) in depth.iter().enumerate() {
                            let Some(od) = opened_depth.get(i).and_then(|o| o.as_ref()) else { continue };
                            if d.eye > 1 {
                                continue;
                            }
                            context.RSSetViewports(Some(&[D3D11_VIEWPORT {
                                TopLeftX: vp.TopLeftX + d.eye as f32 * vp.Width * 0.5,
                                Width: vp.Width * 0.5,
                                ..vp
                            }]));
                            let r = d.rect;
                            let values =
                                [r[0] as f32, r[1] as f32, r[2] as f32, r[3] as f32, d.info.min, d.info.max, d.info.near, d.info.far];
                            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
                            if context.Map(&depth_cb, 0, D3D11_MAP_WRITE_DISCARD, 0, Some(&mut mapped)).is_ok() {
                                std::ptr::copy_nonoverlapping(values.as_ptr(), mapped.pData as *mut f32, 8);
                                context.Unmap(&depth_cb, 0);
                            }
                            context.PSSetShaderResources(1, Some(&[Some(od.srvs[latest % od.srvs.len()].clone())]));
                            context.Draw(3, 0);
                        }
                        context.PSSetShaderResources(1, Some(&[None]));
                    }
                    // Haptics: a bar on that hand's side of the view while it vibrates.
                    let amps = {
                        let now = crate::time::now_ns();
                        let h = shared.haptics.lock().unwrap();
                        [h[LEFT].shown(now), h[RIGHT].shown(now)]
                    };
                    if centre && reproject_on {
                        // Each eye where it is now, from the frame drawn at its older pose.
                        let now = {
                            let s = sim.lock().unwrap();
                            [s.eye_pose(0), s.eye_pose(1)]
                        };
                        for (e, (_, fov)) in views.iter().enumerate() {
                            let half = D3D11_VIEWPORT {
                                TopLeftX: screen_vp.TopLeftX + e as f32 * screen_vp.Width * 0.5,
                                Width: screen_vp.Width * 0.5,
                                ..screen_vp
                            };
                            draw_view(
                                &context,
                                &rtv,
                                comp.as_ref().unwrap(),
                                &views,
                                &depth,
                                &opened_depth,
                                latest,
                                (&now[e], *fov),
                                e as f32,
                                half,
                                (vs.as_ref(), cps.as_ref(), &centre_cb),
                            );
                        }
                    } else if centre {
                        draw_centre(
                            &context,
                            &rtv,
                            comp.as_ref().unwrap(),
                            &views,
                            &depth,
                            &opened_depth,
                            latest,
                            size,
                            (vs.as_ref(), cps.as_ref(), &centre_cb),
                        );
                    }
                    context.PSSetShaderResources(0, Some(&[None]));
                    for (hand, a) in amps.into_iter().enumerate() {
                        if a <= 0.0 {
                            continue;
                        }
                        let (x0, y0) = ((cw - vw) * 0.5, (ch - vh) * 0.5);
                        let bw = 4.0 + 14.0 * a;
                        let left = if hand == LEFT { x0 } else { x0 + vw - bw };
                        let r = RECT { left: left as i32, top: y0 as i32, right: (left + bw) as i32, bottom: (y0 + vh) as i32 };
                        context4.ClearView(&rtv, &[1.0, 0.45 + 0.3 * (1.0 - a), 0.05, 1.0], Some(&[r]));
                    }
                }
            }
            context.OMSetRenderTargets(None, None);
            drop(rtv);
            drop(back);
            let _ = swapchain.Present(1, DXGI_PRESENT(0));
            if hidden {
                std::thread::sleep(std::time::Duration::from_millis(8));
            }

            // The control API: views to switch, pictures of the window to take.
            let views = std::mem::take(&mut *shared.view_request.lock().unwrap());
            for (b, want) in [depth_box, centre_box, reproject_box].into_iter().zip(views) {
                if let Some(on) = want {
                    SendMessageW(b, BM_SETCHECK, Some(WPARAM(on as usize)), None);
                }
            }
            let waiting = std::mem::take(&mut *shared.window_requests.lock().unwrap());
            if !waiting.is_empty() {
                let image = window_image(hwnd);
                for tx in waiting {
                    let _ = tx.send(image.clone());
                }
            }

            if let Some(path) = &snapshot {
                // Shortly after the first frame arrives, once.
                if frames > 0 && snapshot_at.is_none() {
                    snapshot_at = Some(Instant::now());
                }
                // XRMIMIC_TEST_SNAPSHOT_DELAY: seconds after the first frame (0.8).
                let delay = std::env::var("XRMIMIC_TEST_SNAPSHOT_DELAY").ok().and_then(|v| v.parse().ok()).unwrap_or(0.8f32);
                if snapshot_at.is_some_and(|t| t.elapsed().as_secs_f32() > delay) {
                    match snapshot_window(hwnd, path) {
                        Ok(()) => xlog!("window snapshot written to {}", path.display()),
                        Err(e) => xlog!("window snapshot failed: {e}"),
                    }
                    snapshot_at = Some(Instant::now() + std::time::Duration::from_secs(1 << 30));
                }
            }

            if panel_on() && crate::logview::current_tab() == 0 && logs_at.elapsed().as_millis() >= 150 {
                logs_at = Instant::now();
                crate::logview::refresh(false);
            }

            if title_at.elapsed().as_secs_f32() >= 0.5 {
                let secs = title_at.elapsed().as_secs_f32();
                let fps = (frames - frames_at_title) as f32 / secs;
                frames_at_title = frames;
                title_at = Instant::now();
                let app = shared.app_name.lock().unwrap().clone();
                let hand = if active == LEFT { "LEFT" } else { "RIGHT" };

                let title = if switching {
                    "XRMimic - switching the system OpenXR runtime (answer the Windows prompt)".to_string()
                } else {
                    format!(
                        "XRMimic - {app} - {fps:.0} fps - left-drag look, right-drag pan, wheel zoom, WASD/QE move - {hand} controller (Tab; F/G trigger/grip; Ctrl+drag/wheel moves it, R returns it)"
                    )
                };
                let wide: Vec<u16> = title.encode_utf16().chain(Some(0)).collect();
                let _ = SetWindowTextW(hwnd, windows::core::PCWSTR(wide.as_ptr()));
            }
        }
        end_drag();
        let _ = DestroyWindow(hwnd);
        let _ = UnregisterClassW(w!("xrmimic-headset"), Some(hinst.into()));
        xlog!("window closed");
        Ok(())
    }
}

/// The window's client area (toolbar and eyes) as a BMP, via PrintWindow.
unsafe fn snapshot_window(hwnd: HWND, path: &std::path::Path) -> Result<(), String> {
    let i = window_image(hwnd)?;
    crate::log::write_bmp(path, i.width, i.height, &i.bgra).map_err(|e| e.to_string())
}

/// The window's client area (toolbar and eyes) as a picture, via PrintWindow.
unsafe fn window_image(hwnd: HWND) -> Result<crate::shared::Image, String> {
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::Storage::Xps::{PrintWindow, PRINT_WINDOW_FLAGS};
    let mut r = RECT::default();
    GetClientRect(hwnd, &mut r).map_err(|e| e.to_string())?;
    let (w, h) = (r.right - r.left, r.bottom - r.top);
    let screen = GetDC(None);
    let mem = CreateCompatibleDC(Some(screen));
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: w,
            biHeight: -h,
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB.0,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut bits = std::ptr::null_mut();
    let dib = CreateDIBSection(Some(mem), &bmi, DIB_RGB_COLORS, &mut bits, None, 0).map_err(|e| e.to_string())?;
    let old = SelectObject(mem, dib.into());
    // PW_CLIENTONLY | PW_RENDERFULLCONTENT: includes the flip-model swapchain.
    let ok = PrintWindow(hwnd, mem, PRINT_WINDOW_FLAGS(3)).as_bool();
    let mut px = std::slice::from_raw_parts(bits as *const u8, (w * h * 4) as usize).to_vec();
    for p in px.chunks_mut(4) {
        p[3] = 255;
    }
    SelectObject(mem, old);
    let _ = DeleteObject(dib.into());
    let _ = DeleteDC(mem);
    ReleaseDC(None, screen);
    if !ok {
        return Err("PrintWindow failed".into());
    }
    Ok(crate::shared::Image { width: w as u32, height: h as u32, bgra: px })
}

/// Keyboard and mouse to an input frame; controller input goes in slot 0 and
/// the caller moves it to the active controller.
fn input_frame(i: &WinInput, focused: bool) -> InputFrame {
    if !focused {
        return InputFrame::default();
    }
    let k = |c: u8| i.keys[c as usize];
    let axis = |pos: u8, neg: u8| (k(pos) as i32 - k(neg) as i32) as f32;
    let vk = |v: VIRTUAL_KEY| i.keys[v.0 as usize];
    let mut f = InputFrame {
        mouse_dx: i.mouse_dx,
        mouse_dy: i.mouse_dy,
        pan_dx: i.pan_dx,
        pan_dy: i.pan_dy,
        zoom: i.zoom,
        hand_dx: i.hand[0],
        hand_dy: i.hand[1],
        hand_push: i.hand[2],
        hand_turn_dx: i.hand[3],
        hand_turn_dy: i.hand[4],
        hand_reset: i.hand_reset,
        move_forward: axis(b'W', b'S'),
        move_right: axis(b'D', b'A'),
        move_up: axis(b'E', b'Q'),
        sprint: vk(VK_SHIFT),
        turn_rate: (vk(VK_LEFT) as i32 - vk(VK_RIGHT) as i32) as f32 * 1.5,
        ..Default::default()
    };
    f.controllers[0] = ControllerInput {
        thumbstick: [axis(b'L', b'J'), axis(b'I', b'K')],
        trigger: if k(b'F') { 1.0 } else { 0.0 },
        squeeze: if k(b'G') { 1.0 } else { 0.0 },
        button_lower: k(b'1'),
        button_upper: k(b'2'),
        thumbstick_click: k(b'3'),
        menu: k(b'M'),
        ..Default::default()
    };
    f
}

/// The first XInput gamepad's motors (left, right), 0..65535.
pub(crate) fn rumble(speeds: [u16; 2]) {
    let mut v = XINPUT_VIBRATION { wLeftMotorSpeed: speeds[0], wRightMotorSpeed: speeds[1] };
    unsafe {
        let _ = XInputSetState(0, &mut v);
    }
}

/// First XInput gamepad, mapped like a pair of Touch controllers.
pub(crate) fn gamepad() -> InputFrame {
    let mut st = XINPUT_STATE::default();
    if unsafe { XInputGetState(0, &mut st) } != ERROR_SUCCESS.0 {
        return InputFrame::default();
    }
    let g = st.Gamepad;
    let stick = |x: i16, y: i16| {
        let v = [x as f32 / 32767.0, y as f32 / 32767.0];
        let m = v[0].hypot(v[1]);
        if m < 0.24 {
            [0.0, 0.0]
        } else {
            let s = ((m - 0.24) / 0.76).min(1.0) / m;
            [v[0] * s, v[1] * s]
        }
    };
    let b = |f: XINPUT_GAMEPAD_BUTTON_FLAGS| g.wButtons.0 & f.0 != 0;
    let trig = |t: u8| if t < 30 { 0.0 } else { t as f32 / 255.0 };
    let mut f = InputFrame::default();
    f.controllers[LEFT] = ControllerInput {
        thumbstick: stick(g.sThumbLX, g.sThumbLY),
        trigger: trig(g.bLeftTrigger),
        squeeze: if b(XINPUT_GAMEPAD_LEFT_SHOULDER) { 1.0 } else { 0.0 },
        button_lower: b(XINPUT_GAMEPAD_X),
        button_upper: b(XINPUT_GAMEPAD_Y),
        thumbstick_click: b(XINPUT_GAMEPAD_LEFT_THUMB),
        menu: b(XINPUT_GAMEPAD_START),
        ..Default::default()
    };
    f.controllers[RIGHT] = ControllerInput {
        thumbstick: stick(g.sThumbRX, g.sThumbRY),
        trigger: trig(g.bRightTrigger),
        squeeze: if b(XINPUT_GAMEPAD_RIGHT_SHOULDER) { 1.0 } else { 0.0 },
        button_lower: b(XINPUT_GAMEPAD_A),
        button_upper: b(XINPUT_GAMEPAD_B),
        thumbstick_click: b(XINPUT_GAMEPAD_RIGHT_THUMB),
        ..Default::default()
    };
    // D-pad walks and turns the head itself (the sticks belong to the app).
    f.move_forward = (b(XINPUT_GAMEPAD_DPAD_UP) as i32 - b(XINPUT_GAMEPAD_DPAD_DOWN) as i32) as f32;
    f.turn_rate = (b(XINPUT_GAMEPAD_DPAD_LEFT) as i32 - b(XINPUT_GAMEPAD_DPAD_RIGHT) as i32) as f32 * 1.5;
    f
}
