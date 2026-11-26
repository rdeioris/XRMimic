//! The control API: a small HTTP + JSON server for tests and tools, to drive
//! the simulated user and read what the app does while it runs.
//!
//! Off unless XRMIMIC_CONTROL is set to an address (`127.0.0.1:0` picks a
//! free port; the address taken is logged and, with XRMIMIC_CONTROL_FILE,
//! written to that file). XRMIMIC_CONTROL_TOKEN makes every request carry
//! `Authorization: Bearer <token>`. The server lives in the app's process, so
//! it cannot open inside Chrome's sandbox (an AppContainer cannot listen).
//!
//! The API (JSON in and out; the newest session):
//!
//! | Request | Does |
//! | --- | --- |
//! | `GET /v1/state` | device, session state and frame, app, interaction profile and bindings, the user (head, eyes, controllers with their inputs), haptics, the clock |
//! | `POST /v1/head` `{position?, yaw?, pitch?}` | move and turn the head (STAGE, metres and radians) |
//! | `POST /v1/controller/{left,right}` `{input?, replace?, put?}` | hold inputs on a controller (merged into what is held unless `replace`); `put`: an aim pose `{position, orientation?}` to put it down at, or `null` to give it back to the head |
//! | `POST /v1/hands` `{on}` | put the controllers down (tracked hands) or pick them up |
//! | `POST /v1/device` `{preset?, settings?}` | a device preset, or settings lines as the Device tab saves them |
//! | `POST /v1/views` `{depth?, centre?, reproject?}` | the window's views |
//! | `POST /v1/clock` `{paused?, step?, wait?}` | hold frames back, allow `step` more, `wait` until they have ended |
//! | `GET /v1/wait?frame=N&state=FOCUSED&timeout_ms=` | until the app has ended frame N and/or the session is in that state |
//! | `GET /v1/capture/eyes?format=png\|bmp` | the next frame's eyes side by side, as the app drew them |
//! | `GET /v1/capture/window?format=png\|bmp` | the headset window (Windows) |

use crate::headset::{self, FrameGate, HeadsetControl};
use crate::state::rt;
use crate::xlog;
use openxr_sys as xr;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use xrmimic_core::math::{Pose, Quat, Vec3};
use xrmimic_core::sim::{ControllerInput, LEFT, RIGHT};

static STARTED: std::sync::Once = std::sync::Once::new();

/// Start the server once per process, if XRMIMIC_CONTROL asks for one.
pub fn start_once() {
    STARTED.call_once(|| {
        let Ok(addr) = std::env::var("XRMIMIC_CONTROL") else { return };
        let server = match tiny_http::Server::http(&addr) {
            Ok(s) => s,
            Err(e) => {
                xlog!("control API: cannot listen on {addr}: {e} (inside Chrome's sandbox it cannot)");
                return;
            }
        };
        let local = server.server_addr().to_ip().map(|a| a.to_string()).unwrap_or(addr);
        xlog!("control API on http://{local}/v1/");
        if let Some(file) = std::env::var_os("XRMIMIC_CONTROL_FILE") {
            if let Err(e) = std::fs::write(&file, &local) {
                xlog!("control API: cannot write {}: {e}", std::path::Path::new(&file).display());
            }
        }
        let token = std::env::var("XRMIMIC_CONTROL_TOKEN").ok().filter(|t| !t.is_empty());
        let server = Arc::new(server);
        let _ = std::thread::Builder::new().name("xrmimic-control".into()).spawn(move || {
            for request in server.incoming_requests() {
                let token = token.clone();
                // One thread a request: waits and captures block.
                let _ = std::thread::Builder::new().name("xrmimic-control-request".into()).spawn(move || {
                    respond(request, token.as_deref());
                });
            }
        });
    });
}

struct Reply {
    status: u16,
    content_type: &'static str,
    body: Vec<u8>,
}

fn json_reply(status: u16, v: Value) -> Reply {
    Reply { status, content_type: "application/json", body: v.to_string().into_bytes() }
}

fn error(status: u16, message: impl Into<String>) -> Reply {
    json_reply(status, json!({ "error": message.into() }))
}

fn respond(mut request: tiny_http::Request, token: Option<&str>) {
    let reply = handle(&mut request, token);
    let header = tiny_http::Header::from_bytes(&b"Content-Type"[..], reply.content_type.as_bytes()).unwrap();
    let response = tiny_http::Response::from_data(reply.body).with_status_code(reply.status).with_header(header);
    let _ = request.respond(response);
}

fn handle(request: &mut tiny_http::Request, token: Option<&str>) -> Reply {
    if let Some(t) = token {
        let want = format!("Bearer {t}");
        let ok = request.headers().iter().any(|h| h.field.equiv("Authorization") && h.value.as_str() == want);
        if !ok {
            return error(401, "missing or wrong Authorization: Bearer <XRMIMIC_CONTROL_TOKEN>");
        }
    }
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let query: HashMap<String, String> = query
        .split('&')
        .filter_map(|kv| kv.split_once('='))
        .map(|(k, v)| (k.to_string(), v.to_string()))
        .collect();
    let mut text = String::new();
    if std::io::Read::read_to_string(request.as_reader(), &mut text).is_err() {
        return error(400, "unreadable body");
    }
    let body: Value = if text.trim().is_empty() {
        json!({})
    } else {
        match serde_json::from_str(&text) {
            Ok(v) => v,
            Err(e) => return error(400, format!("body is not JSON: {e}")),
        }
    };
    let method = request.method().as_str().to_string();
    match (method.as_str(), path) {
        ("GET", "/v1/state") => json_reply(200, state()),
        ("POST", "/v1/head") => with_headset(|c| {
            c.set_head(vec3(&body["position"]), f32_of(&body["yaw"]), f32_of(&body["pitch"]));
            Ok(json!({ "ok": true }))
        }),
        ("POST", p) if p.starts_with("/v1/controller/") => {
            let hand = match &p["/v1/controller/".len()..] {
                "left" => LEFT,
                "right" => RIGHT,
                other => return error(404, format!("no hand {other:?}: left or right")),
            };
            with_headset(|c| set_controller(c, hand, &body))
        }
        ("POST", "/v1/hands") => with_headset(|c| {
            let on = body["on"].as_bool().ok_or("hands: {\"on\": true|false}")?;
            c.set_hands(on);
            Ok(json!({ "ok": true, "hands": on }))
        }),
        ("POST", "/v1/device") => set_device(&body),
        ("POST", "/v1/views") => with_headset(|c| {
            c.set_views([body["depth"].as_bool(), body["centre"].as_bool(), body["reproject"].as_bool()]);
            Ok(json!({ "ok": true }))
        }),
        ("POST", "/v1/clock") => clock(&body),
        ("GET", "/v1/wait") => wait(&query),
        ("GET", "/v1/capture/eyes") => capture(&query, false),
        ("GET", "/v1/capture/window") => capture(&query, true),
        _ => error(404, format!("no {method} {path}; see GET /v1/state and the list in control.rs")),
    }
}

/// What the control API knows of a session, wherever its headset runs.
pub struct SessionInfo {
    pub handle: u64,
    pub state: String,
    pub running: bool,
    /// Frames the app has ended, and those with a projection layer.
    pub frame: u64,
    pub shown: u64,
    pub app: String,
    pub profile: Option<String>,
    /// The app's bindings in that profile: (binding path, action name).
    pub bound: Vec<(String, String)>,
}

/// A session the control API can drive: one in this process, or one the
/// server runs for an app elsewhere (crate::server).
pub trait ControlledSession: Send + Sync {
    /// None once the session is gone.
    fn info(&self) -> Option<SessionInfo>;
    fn control(&self) -> Option<Arc<dyn HeadsetControl>>;
    fn gate(&self) -> Option<Arc<FrameGate>>;
    /// (the frame-stepped clock is on, the frame period in ns).
    fn clock(&self) -> (bool, i64);
}

static SESSIONS: std::sync::Mutex<Vec<(u64, Arc<dyn ControlledSession>)>> = std::sync::Mutex::new(Vec::new());

/// Make a session the control API's newest.
pub fn register(key: u64, session: Arc<dyn ControlledSession>) {
    let mut s = SESSIONS.lock().unwrap();
    s.retain(|(k, _)| *k != key);
    s.push((key, session));
}

pub fn unregister(key: u64) {
    SESSIONS.lock().unwrap().retain(|(k, _)| *k != key);
}

/// The newest session.
fn current() -> Option<Arc<dyn ControlledSession>> {
    SESSIONS.lock().unwrap().last().map(|(_, s)| s.clone())
}

/// A session of this process's runtime (its OpenXR side and headset here).
pub struct InProcessSession(pub u64);

impl ControlledSession for InProcessSession {
    fn info(&self) -> Option<SessionInfo> {
        let rt = rt();
        let s = rt.sessions.get(&self.0)?;
        let app = rt.instances.get(&s.instance).map(|i| i.app_name.clone()).unwrap_or_default();
        let bound = rt
            .instances
            .get(&s.instance)
            .and_then(|i| i.suggested.get(&s.profile))
            .map(|list| {
                list.iter()
                    .filter_map(|(action, path)| Some((rt.path_str(*path)?.to_string(), rt.actions.get(action)?.name.clone())))
                    .collect()
            })
            .unwrap_or_default();
        Some(SessionInfo {
            handle: self.0,
            state: state_name(s.state).to_string(),
            running: s.running,
            frame: s.frames_ended,
            shown: s.frames_shown,
            app,
            profile: rt.path_str(s.profile).map(str::to_string),
            bound,
        })
    }

    fn control(&self) -> Option<Arc<dyn HeadsetControl>> {
        rt().sessions.get(&self.0).map(|s| s.headset.control())
    }

    fn gate(&self) -> Option<Arc<FrameGate>> {
        rt().sessions.get(&self.0).map(|s| s.headset.frame_gate())
    }

    fn clock(&self) -> (bool, i64) {
        rt().sessions.get(&self.0).map(|s| (s.headset.stepped(), s.headset.period_ns())).unwrap_or((false, 0))
    }
}

fn current_control() -> Option<Arc<dyn HeadsetControl>> {
    current()?.control()
}

fn with_headset(f: impl FnOnce(&dyn HeadsetControl) -> Result<Value, String>) -> Reply {
    let Some(control) = current_control() else {
        return error(409, "no session");
    };
    match f(control.as_ref()) {
        Ok(v) => json_reply(200, v),
        Err(e) => error(400, e),
    }
}

fn f32_of(v: &Value) -> Option<f32> {
    v.as_f64().map(|x| x as f32)
}

fn vec3(v: &Value) -> Option<[f32; 3]> {
    let a = v.as_array()?;
    if a.len() != 3 {
        return None;
    }
    Some([f32_of(&a[0])?, f32_of(&a[1])?, f32_of(&a[2])?])
}

fn pose_json(p: &Pose) -> Value {
    let (o, q) = (p.position, p.orientation);
    json!({ "position": [o.x, o.y, o.z], "orientation": [q.x, q.y, q.z, q.w] })
}

/// `{position: [x, y, z], orientation?: [x, y, z, w]}`.
fn pose_of(v: &Value) -> Result<Pose, String> {
    let p = vec3(&v["position"]).ok_or("a pose needs \"position\": [x, y, z]")?;
    let q = match v["orientation"].as_array() {
        Some(a) if a.len() == 4 => {
            let f: Vec<f32> = a.iter().filter_map(f32_of).collect();
            if f.len() != 4 {
                return Err("\"orientation\": [x, y, z, w]".into());
            }
            Quat { x: f[0], y: f[1], z: f[2], w: f[3] }.normalized()
        }
        None => Quat::IDENTITY,
        _ => return Err("\"orientation\": [x, y, z, w]".into()),
    };
    Ok(Pose::new(q, Vec3::new(p[0], p[1], p[2])))
}

/// A controller's inputs as JSON (the names of xrmimic_core::sim::ControllerInput).
pub fn input_json(c: &ControllerInput) -> Value {
    json!({
        "thumbstick": c.thumbstick, "trigger": c.trigger, "squeeze": c.squeeze,
        "button_lower": c.button_lower, "button_upper": c.button_upper,
        "thumbstick_click": c.thumbstick_click, "menu": c.menu,
        "trigger_touch": c.trigger_touch, "thumbstick_touch": c.thumbstick_touch,
        "button_lower_touch": c.button_lower_touch, "button_upper_touch": c.button_upper_touch,
        "thumbrest_touch": c.thumbrest_touch, "trackpad": c.trackpad, "trackpad_click": c.trackpad_click,
        "trackpad_touch": c.trackpad_touch, "curls": c.curls, "hand_hidden": c.hand_hidden,
    })
}

/// Set the inputs `v` names on `c`; unknown names are an error.
pub fn apply_input_json(c: &mut ControllerInput, v: &Value) -> Result<(), String> {
    let Some(map) = v.as_object() else { return Err("\"input\" must be an object".into()) };
    for (k, x) in map {
        let num = || f32_of(x).ok_or_else(|| format!("{k}: a number"));
        let flag = || x.as_bool().ok_or_else(|| format!("{k}: true or false"));
        let pair = || -> Result<[f32; 2], String> {
            let a = x.as_array().filter(|a| a.len() == 2).ok_or_else(|| format!("{k}: [x, y]"))?;
            Ok([f32_of(&a[0]).ok_or(format!("{k}: numbers"))?, f32_of(&a[1]).ok_or(format!("{k}: numbers"))?])
        };
        match k.as_str() {
            "thumbstick" => c.thumbstick = pair()?,
            "trackpad" => c.trackpad = pair()?,
            "trigger" => c.trigger = num()?.clamp(0.0, 1.0),
            "squeeze" => c.squeeze = num()?.clamp(0.0, 1.0),
            "button_lower" => c.button_lower = flag()?,
            "button_upper" => c.button_upper = flag()?,
            "thumbstick_click" => c.thumbstick_click = flag()?,
            "menu" => c.menu = flag()?,
            "trigger_touch" => c.trigger_touch = flag()?,
            "thumbstick_touch" => c.thumbstick_touch = flag()?,
            "button_lower_touch" => c.button_lower_touch = flag()?,
            "button_upper_touch" => c.button_upper_touch = flag()?,
            "thumbrest_touch" => c.thumbrest_touch = flag()?,
            "trackpad_click" => c.trackpad_click = flag()?,
            "trackpad_touch" => c.trackpad_touch = flag()?,
            "hand_hidden" => c.hand_hidden = flag()?,
            "curls" => {
                let a = x.as_array().filter(|a| a.len() == 5).ok_or("curls: five numbers, thumb to little finger")?;
                for (i, v) in a.iter().enumerate() {
                    c.curls[i] = f32_of(v).ok_or("curls: numbers")?.clamp(0.0, 1.0);
                }
            }
            other => return Err(format!("no input {other:?}")),
        }
    }
    Ok(())
}

fn set_controller(c: &dyn HeadsetControl, hand: usize, body: &Value) -> Result<Value, String> {
    if !body["input"].is_null() {
        // What the control API holds now, with this request's changes.
        let mut held = if body["replace"].as_bool().unwrap_or(false) {
            ControllerInput::default()
        } else {
            c.held_by_control(hand)
        };
        apply_input_json(&mut held, &body["input"])?;
        c.hold_input(hand, held);
    }
    if let Some(obj) = body.as_object() {
        if let Some(put) = obj.get("put") {
            c.place_controller(hand, if put.is_null() { None } else { Some(pose_of(put)?) });
        }
    }
    let user = c.user();
    Ok(json!({ "ok": true, "aim": pose_json(&user.aim_pose(hand)), "input": input_json(&user.controllers[hand]) }))
}

fn set_device(body: &Value) -> Reply {
    let mut d = headset::device();
    let mut problems = Vec::new();
    if let Some(p) = body["preset"].as_str() {
        if let Err(e) = d.set("preset", p) {
            return error(400, e);
        }
    }
    if let Some(text) = body["settings"].as_str() {
        problems = d.apply_ini(text);
    }
    if !problems.is_empty() {
        return error(400, problems.join("; "));
    }
    let name = d.name.clone();
    match crate::devstore::set(d) {
        Ok(_) => json_reply(200, json!({ "ok": true, "device": name })),
        Err(e) => error(500, e),
    }
}

fn clock(body: &Value) -> Reply {
    let Some(session) = current() else { return error(409, "no session") };
    let (Some(gate), Some(info)) = (session.gate(), session.info()) else { return error(409, "no session") };
    let frame_before = info.frame;
    if let Some(p) = body["paused"].as_bool() {
        gate.set_paused(p);
    }
    let step = body["step"].as_u64().unwrap_or(0);
    if step > 0 {
        gate.step(step);
    }
    let mut frame = frame_before;
    if step > 0 && body["wait"].as_bool().unwrap_or(false) {
        // Until the allowed frames have started, then until the app ended them.
        let deadline = Instant::now() + Duration::from_secs(10);
        gate.wait_drained(Duration::from_secs(10));
        loop {
            frame = session.info().map(|i| i.frame).unwrap_or(0);
            if frame >= frame_before + step || Instant::now() > deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    let (paused, allowed) = gate.status();
    json_reply(200, json!({ "ok": true, "paused": paused, "allowed": allowed, "frame": frame }))
}

pub fn state_name(s: xr::SessionState) -> &'static str {
    match s {
        xr::SessionState::IDLE => "IDLE",
        xr::SessionState::READY => "READY",
        xr::SessionState::SYNCHRONIZED => "SYNCHRONIZED",
        xr::SessionState::VISIBLE => "VISIBLE",
        xr::SessionState::FOCUSED => "FOCUSED",
        xr::SessionState::STOPPING => "STOPPING",
        xr::SessionState::LOSS_PENDING => "LOSS_PENDING",
        xr::SessionState::EXITING => "EXITING",
        _ => "UNKNOWN",
    }
}

fn state() -> Value {
    let d = headset::device();
    let device = json!({
        "name": d.name, "preset": d.preset, "refresh_hz": d.default_refresh, "ipd": d.ipd,
        "eye_size": [d.eye_width, d.eye_height], "play_area": d.play_area, "controllers": d.controllers,
    });
    let none = json!({ "runtime": "XRMimic", "device": device, "session": null });
    let Some(session) = current() else { return none };
    let (Some(info), Some(control), Some(gate)) = (session.info(), session.control(), session.gate()) else {
        return none;
    };
    let (stepped, period) = session.clock();
    let bound: Vec<Value> = info.bound.iter().map(|(p, a)| json!([p, a])).collect();
    let user = control.user();
    let held = control.held_inputs();
    let now = crate::time::now_ns();
    let haptics = control.haptics(now);
    let controllers: Vec<Value> = [LEFT, RIGHT]
        .iter()
        .map(|&i| {
            json!({
                "hand": if i == LEFT { "left" } else { "right" },
                "aim": pose_json(&user.aim_pose(i)),
                "grip": pose_json(&user.grip_pose(i)),
                "put_down": user.placed[i].is_some(),
                "input": input_json(&user.controllers[i]),
                "held": input_json(&held[i]),
            })
        })
        .collect();
    let haptics: Vec<Value> = [LEFT, RIGHT]
        .iter()
        .map(|&i| {
            let hs = &haptics[i];
            json!({
                "hand": if i == LEFT { "left" } else { "right" },
                "amplitude": hs.amplitude(now),
                "last": hs.last.map(|(v, _)| json!({
                    "amplitude": v.amplitude, "frequency": v.frequency, "duration_ns": v.duration_ns,
                })),
                "count": hs.count,
            })
        })
        .collect();
    let (paused, allowed) = gate.status();
    json!({
        "runtime": "XRMimic",
        "device": device,
        "session": {
            "handle": info.handle, "state": info.state, "running": info.running, "frame": info.frame,
            "frames_shown": info.shown, "app": info.app, "profile": info.profile, "bound": bound,
            "hands": control.hands(),
            "clock": { "stepped": stepped, "period_ns": period, "paused": paused, "allowed": allowed },
            "user": {
                "head": pose_json(&user.head_pose()),
                "yaw": user.yaw, "pitch": user.pitch,
                "eyes": [pose_json(&user.eye_pose(0)), pose_json(&user.eye_pose(1))],
                "controllers": controllers,
            },
            "haptics": haptics,
        },
    })
}

fn wait(query: &HashMap<String, String>) -> Reply {
    let frame: Option<u64> = query.get("frame").and_then(|v| v.parse().ok());
    let want_state = query.get("state").map(|s| s.to_ascii_uppercase());
    let timeout = Duration::from_millis(query.get("timeout_ms").and_then(|v| v.parse().ok()).unwrap_or(10_000));
    let deadline = Instant::now() + timeout;
    loop {
        let (now_frame, now_state) = match current().and_then(|s| s.info()) {
            Some(i) => (i.frame, i.state),
            None => (0, "NONE".to_string()),
        };
        let ok = frame.is_none_or(|f| now_frame >= f) && want_state.as_deref().is_none_or(|w| w == now_state.as_str());
        if ok {
            return json_reply(200, json!({ "ok": true, "frame": now_frame, "state": now_state }));
        }
        if Instant::now() > deadline {
            return json_reply(408, json!({ "ok": false, "frame": now_frame, "state": now_state }));
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn capture(query: &HashMap<String, String>, window: bool) -> Reply {
    let timeout = Duration::from_millis(query.get("timeout_ms").and_then(|v| v.parse().ok()).unwrap_or(5_000));
    let format = query.get("format").map(String::as_str).unwrap_or("png");
    if format != "png" && format != "bmp" {
        return error(400, "format: png or bmp");
    }
    let Some(control) = current_control() else { return error(409, "no session") };
    let image = if window { control.capture_window(timeout) } else { control.capture_eyes(timeout) };
    let image = match image {
        Ok(i) => i,
        Err(e) => return error(504, e),
    };
    if format == "bmp" {
        return Reply { status: 200, content_type: "image/bmp", body: bmp_bytes(&image) };
    }
    match png_bytes(&image) {
        Ok(b) => Reply { status: 200, content_type: "image/png", body: b },
        Err(e) => error(500, e),
    }
}

fn bmp_bytes(i: &crate::shared::Image) -> Vec<u8> {
    let mut out = Vec::with_capacity(54 + i.bgra.len());
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + i.bgra.len() as u32).to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(i.width as i32).to_le_bytes());
    out.extend_from_slice(&(-(i.height as i32)).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&32u16.to_le_bytes());
    out.extend_from_slice(&[0u8; 24]);
    out.extend_from_slice(&i.bgra);
    out
}

fn png_bytes(i: &crate::shared::Image) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    {
        let mut e = png::Encoder::new(&mut out, i.width, i.height);
        e.set_color(png::ColorType::Rgb);
        e.set_depth(png::BitDepth::Eight);
        let mut w = e.write_header().map_err(|e| e.to_string())?;
        let rgb: Vec<u8> = i.bgra.chunks(4).flat_map(|p| [p[2], p[1], p[0]]).collect();
        w.write_image_data(&rgb).map_err(|e| e.to_string())?;
    }
    Ok(out)
}
