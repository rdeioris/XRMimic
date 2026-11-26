// Smoke check: start a WebXR immersive-vr session in Chrome against xrmimic.
//
//   node tools/smoke/smoke.mjs [--headless] [--url URL] [--keep]
//
// Starts Chrome with its own profile and XR_RUNTIME_JSON, serves page.html,
// calls startXR() through the DevTools protocol (a user gesture inside the
// page, no OS input), then checks the session ran, frames were submitted and
// the runtime's capture shows the left eye red and the right eye green.
// With --url it only checks that a session on that page starts and submits frames.
// Runs on Windows (D3D11, headset window) and Linux (Vulkan, headless headset;
// Chrome 157+ with --enable-features=OpenXR, here unpacked in ~/xrmimic-chrome).

import { spawn, execFileSync } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname(fileURLToPath(import.meta.url));
const root = path.resolve(here, '..', '..');
const args = process.argv.slice(2);
const flag = (n) => args.includes(n);
const opt = (n, d) => (args.includes(n) ? args[args.indexOf(n) + 1] : d);

const win = process.platform === 'win32';
const chrome = opt('--chrome', process.env.CHROME || (win
  ? 'C:/Program Files/Google/Chrome/Application/chrome.exe'
  : path.join(os.homedir(), 'xrmimic-chrome/root/opt/google/chrome/chrome')));
const lib = win ? 'xrmimic_runtime.dll' : 'libxrmimic_runtime.so';
const work = fs.mkdtempSync(path.join(os.tmpdir(), 'xrmimic-smoke-'));
if (win) {
  // Chrome's XR process runs in an AppContainer: let it read the runtime and
  // write the log and captures in this folder (and nowhere else).
  execFileSync('icacls', [work, '/grant', '*S-1-15-2-1:(OI)(CI)(M)'], { stdio: 'ignore' });
}
const log = path.join(work, 'xrmimic.log');
const bmp = path.join(work, 'capture.bmp');
const fail = (msg) => { console.error(`FAIL: ${msg}`); finish(1); };

let browser;
let server;
function finish(code) {
  if (browser && !flag('--keep') && !win) {
    // Chrome was started as its own process group: end the whole group.
    try { process.kill(-browser.pid, 'SIGKILL'); } catch {}
  }
  if (browser && !flag('--keep') && win) {
    try { execFileSync('taskkill', ['/PID', String(browser.pid), '/T', '/F'], { stdio: 'ignore' }); } catch {}
    // Sandboxed children can outlive the tree kill: stop any left on this profile, by PID.
    try {
      const out = execFileSync('powershell', ['-NoProfile', '-Command',
        `Get-CimInstance Win32_Process -Filter "Name='chrome.exe'" | Where-Object { $_.CommandLine -like '*${path.basename(work)}*' } | ForEach-Object { $_.ProcessId }`],
        { encoding: 'utf8' });
      for (const pid of out.split(/\s+/).filter(Boolean)) {
        try { execFileSync('taskkill', ['/PID', pid, '/F'], { stdio: 'ignore' }); } catch {}
      }
    } catch {}
  }
  server?.close();
  console.log(`log: ${log}`);
  process.exit(code);
}

// 1. Serve the test page on localhost (a secure context for WebXR).
server = http.createServer((req, res) => {
  res.writeHead(200, { 'content-type': 'text/html' });
  res.end(fs.readFileSync(path.join(here, 'page.html')));
});
await new Promise((r) => server.listen(0, '127.0.0.1', r));
const pageUrl = opt('--url', `http://localhost:${server.address().port}/`);

// 2. Chrome on its own profile with the runtime selected for it alone,
// loading its own copy of the DLL so cargo can rebuild meanwhile.
fs.copyFileSync(path.join(root, 'target', 'release', lib), path.join(work, lib));
fs.writeFileSync(path.join(work, 'xrmimic.json'), JSON.stringify({
  file_format_version: '1.0.0',
  runtime: { name: 'xrmimic', library_path: `./${lib}` },
}));
// XRMIMIC_SMOKE_DEVICE: device settings (key=value lines) for this run, written
// where the sandboxed XR process can read them.
if (process.env.XRMIMIC_SMOKE_DEVICE) {
  fs.writeFileSync(path.join(work, 'device.ini'), process.env.XRMIMIC_SMOKE_DEVICE.split(';').join('\n'));
}
const turnRate = 0.5; // rad/s of scripted head yaw (positive turns left)
const env = {
  ...process.env,
  XR_RUNTIME_JSON: path.join(work, 'xrmimic.json'),
  // The smoke's own page: a scripted turn and walk (other pages: standing still).
  ...(opt('--url') || opt('--eval-file') ? {} : {
    XRMIMIC_TEST_TURN: String(turnRate),
    XRMIMIC_TEST_MOVE: '1', // walk forward at the default 1.4 m/s
  }),
  XRMIMIC_TEST_CONTROLLERS: '1',
  // The second session is closed from the headset side after 120 frames (only
  // in the smoke's own sequence: probes and other pages run undisturbed).
  ...(opt('--url') || opt('--eval-file') ? {} : { XRMIMIC_TEST_CLOSE_AFTER: '120' }),
  XRMIMIC_LOG: log,
  XRMIMIC_TRACE_FILE: path.join(work, 'trace.log'),
  ...(process.env.XRMIMIC_SMOKE_DEVICE ? { XRMIMIC_DEVICE_FILE: path.join(work, 'device.ini') } : {}),
  ...(win ? {
    XRMIMIC_SWITCH_DIR: work,
    XRMIMIC_TEST_SWITCH_TO: 'C:\\nowhere\\test-runtime.json',
    // The window is shown off-screen and photographs itself (PrintWindow).
    XRMIMIC_TEST_SNAPSHOT: path.join(work, 'window.bmp'),
  } : {}),
  XRMIMIC_CAPTURE: bmp,
  XRMIMIC_CAPTURE_FRAME: opt('--capture-frame', '60'),
};
const chromeArgs = [
  `--user-data-dir=${path.join(work, 'profile')}`,
  '--remote-debugging-port=0',
  '--no-first-run',
  '--no-default-browser-check',
  // An off-screen window must still count as visible for requestSession.
  '--disable-features=Translate,CalculateNativeWinOcclusion',
  // Windows: WebXR through WebGPU (XRGPUBinding), Blink's experimental binding
  // and the browser feature that gives WebGPU sessions their OpenXR swapchains
  // (untried on Linux; Chrome keeps only the last --enable-features).
  ...(win ? ['--enable-blink-features=WebXRGPUBinding', '--enable-features=WebXRWebGPUBinding'] : []),
  ...(flag('--no-sandbox') ? ['--no-sandbox'] : []),
  // Linux: OpenXR is behind a feature flag (Chrome 157+); the box has no display.
  // Under a Wayland compositor (WAYLAND_DISPLAY set, e.g. headless GNOME Shell)
  // Chrome can share its frames with the XR process; plain --headless cannot.
  ...(win ? [] : ['--enable-features=OpenXR', `--use-angle=${process.env.XRMIMIC_SMOKE_ANGLE || 'vulkan'}`,
    ...(process.env.WAYLAND_DISPLAY ? ['--ozone-platform=wayland'] : ['--headless=new'])]),
  '--disable-backgrounding-occluded-windows',
  '--disable-renderer-backgrounding',
  '--window-position=-3000,-3000',
  '--window-size=800,600',
  ...(flag('--headless') ? ['--headless=new'] : []),
  ...(opt('--chrome-args') ? opt('--chrome-args').split(' ') : []),
  pageUrl,
];
// --chrome-log: Chrome's own log (stderr) into the work folder.
const chromeLog = flag('--chrome-log') ? fs.openSync(path.join(work, 'chrome.log'), 'w') : 'ignore';
if (flag('--chrome-log')) chromeArgs.splice(1, 0, '--enable-logging=stderr', '--v=0', '--vmodule=*openxr*=2,*xr*=1');
browser = spawn(chrome, chromeArgs, { env, stdio: ['ignore', chromeLog, chromeLog], detached: !win });
if (win) {
  // The switch helper, outside the sandbox, in dry-run mode (no UAC, no registry).
  spawn(path.join(root, 'target', 'release', 'xrmimic-launch.exe'), ['watch', work, String(browser.pid)], {
    env: { ...process.env, XRMIMIC_SWITCH_DRYRUN: '1' },
    stdio: 'ignore',
  });
}

// 3. DevTools: find the page and call startXR() with a user gesture.
const portFile = path.join(work, 'profile', 'DevToolsActivePort');
let port;
for (let i = 0; i < 100 && !port; i++) {
  await new Promise((r) => setTimeout(r, 200));
  if (fs.existsSync(portFile)) port = Number(fs.readFileSync(portFile, 'utf8').split('\n')[0]);
}
if (!port) fail('Chrome did not open a DevTools port');
let target;
for (let i = 0; i < 50 && !target; i++) {
  const list = await (await fetch(`http://127.0.0.1:${port}/json/list`)).json();
  target = list.find((t) => t.type === 'page' && t.url.startsWith(pageUrl.split('?')[0]));
  if (!target) await new Promise((r) => setTimeout(r, 200));
}
if (!target) fail('page target not found');

async function connect(url) {
  const ws = new WebSocket(url);
  await new Promise((r, j) => { ws.onopen = r; ws.onerror = j; });
  let id = 0;
  const pending = new Map();
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (pending.has(msg.id)) { pending.get(msg.id)(msg); pending.delete(msg.id); }
  };
  return (method, params = {}) => new Promise((r) => {
    const n = ++id;
    pending.set(n, r);
    ws.send(JSON.stringify({ id: n, method, params }));
  });
}
const cdp = await connect(target.webSocketDebuggerUrl);

// Grant the VR permission up front so Chrome never shows its prompt. Features
// with a permission of their own (hand-tracking: handTracking) are asked for
// separately and denied unanswered; --grant adds them (comma-separated).
const version = await (await fetch(`http://127.0.0.1:${port}/json/version`)).json();
const browserCdp = await connect(version.webSocketDebuggerUrl);
const granted = await browserCdp('Browser.grantPermissions', {
  permissions: ['vr', ...(opt('--grant') ? opt('--grant').split(',') : [])],
  origin: new URL(pageUrl).origin,
});
if (granted.error) fail(`could not grant the VR permission: ${JSON.stringify(granted.error)}`);
const evaluate = async (expression, timeout = 60000) => {
  const res = await Promise.race([
    cdp('Runtime.evaluate', { expression, awaitPromise: true, userGesture: true, returnByValue: true }),
    new Promise((r) => setTimeout(() => r({ timeout: true }), timeout)),
  ]);
  if (res.timeout) {
    const progress = await Promise.race([
      cdp('Runtime.evaluate', { expression: 'JSON.stringify(window.__probe ?? null)', returnByValue: true }),
      new Promise((r) => setTimeout(() => r(null), 3000)),
    ]);
    fail(`timed out: ${expression.split(/\r?\n/)[0].slice(0, 80)}; probe state ${progress?.result?.result?.value}`);
  }
  if (res.result?.exceptionDetails) fail(JSON.stringify(res.result.exceptionDetails));
  return res.result?.result?.value;
};

// The window is off-screen and never activated: emulate focus instead.
await cdp('Emulation.setFocusEmulationEnabled', { enabled: true });

// Wait for the page to load.
let loaded = false;
for (let i = 0; i < 300 && !loaded; i++) {
  const ready = opt('--url') ? "document.readyState === 'complete'" : "typeof startXR === 'function'";
  loaded = await evaluate(ready);
  if (!loaded) await new Promise((r) => setTimeout(r, 100));
}
if (!loaded) fail(`page did not load: ${await evaluate('location.href + " " + document.readyState')}`);

// --eval-file: run a probe script in the page (with a user gesture), print what
// it returns and what the runtime saw, and stop there.
if (opt('--eval-file')) {
  const probe = await evaluate(fs.readFileSync(opt('--eval-file'), 'utf8'), 90000);
  console.log('probe:', JSON.stringify(probe, null, 1));
  await new Promise((r) => setTimeout(r, 500));
  const tf = path.join(work, 'trace.log');
  const lines = (fs.existsSync(tf) ? fs.readFileSync(tf, 'utf8') : '').split(/\r?\n/);
  const create = lines.find((l) => / xrCreateInstance\(/.test(l)) || '';
  console.log('extensions Chrome enabled:', (create.match(/extensions=\[[^\]]*\]/) || ['none'])[0]);
  const kinds = new Map();
  for (const l of lines.filter((l) => / xrEndFrame\(/.test(l))) {
    const k = [...l.matchAll(/(Projection|COMPOSITION_LAYER_[A-Z0-9_]+)\{/g)].map((m) => m[1]).join(' + ') || '(no layers)';
    kinds.set(k, (kinds.get(k) || 0) + 1);
  }
  console.log('layers per xrEndFrame:', JSON.stringify(Object.fromEntries(kinds)));
  finish(0);
}

let result;
if (opt('--url') && opt('--click')) {
  // Someone else's page, through its own Enter VR control: click it from
  // inside the page (a DevTools user gesture), let the session run, and report
  // what the page shows about the headset (renderia's #xrdevice panel).
  const sel = JSON.stringify(opt('--click'));
  let ready = false;
  for (let i = 0; i < 600 && !ready; i++) {
    ready = await evaluate(`(() => { const b = document.querySelector(${sel}); return !!b && !b.hidden && !b.disabled; })()`);
    if (!ready) await new Promise((r) => setTimeout(r, 100));
  }
  if (!ready) fail(`${opt('--click')} never became clickable`);
  await evaluate(`document.querySelector(${sel}).click()`);
  await new Promise((r) => setTimeout(r, Number(opt('--run-ms', '6000'))));
  if (opt('--read')) {
    // Any page: print what the given expression returns (e.g. the test page's state).
    console.log('page state:', await evaluate(opt('--read')));
  } else {
    const device = await evaluate('JSON.stringify(window.xrDevice ?? null)');
    const panel = await evaluate(`document.getElementById('xrdevice')?.innerText ?? null`);
    console.log('page xrDevice:', device);
    console.log('page panel:\n' + panel);
    if (!panel) fail('the page shows no headset panel');
  }
  if (opt('--page-shot')) {
    const shot = await cdp('Page.captureScreenshot', { format: 'png' });
    fs.writeFileSync(opt('--page-shot'), Buffer.from(shot.result.data, 'base64'));
    console.log(`page screenshot: ${opt('--page-shot')}`);
  }
  result = { state: 'done', frames: 0 };
} else if (opt('--url')) {
  // Someone else's page: start a session directly and count frames.
  result = await evaluate(`(async () => {
    const s = await navigator.xr.requestSession('immersive-vr', { optionalFeatures: ['local-floor'] });
    const gl = document.createElement('canvas').getContext('webgl2', { xrCompatible: true });
    s.updateRenderState({ baseLayer: new XRWebGLLayer(s, gl) });
    const space = await s.requestReferenceSpace('local');
    let frames = 0;
    await new Promise((res) => { const f = (t, fr) => { fr.getViewerPose(space); if (++frames >= 90) res(); else s.requestAnimationFrame(f); }; s.requestAnimationFrame(f); });
    await s.end();
    return { state: 'done', frames };
  })()`);
} else {
  // 1: a normal session (ends itself at 100 frames, before the close hook);
  // --webgpu: drawn through XRGPUBinding instead of WebGL.
  const webgpu = flag('--webgpu');
  result = await evaluate(`startXR(100, ${webgpu})`);
  if (webgpu && result?.errors?.length) fail(`WebGPU session: ${result.errors.join('; ')}`);
  // 2: the headset window "closes" mid-session; the page must see the end.
  const closed = await evaluate('startXR(100000)');
  console.log('closed session:', JSON.stringify({ state: closed?.state, frames: closed?.frames }));
  if (closed?.state !== 'ended-by-runtime') fail('closing the headset did not end the session');
  // 3: a fresh session after that must start and run (stale events once killed it).
  const again = await evaluate('startXR(60)');
  console.log('session after close:', JSON.stringify({ state: again?.state, frames: again?.frames }));
  if (again?.state !== 'done') fail('a session after closing the headset did not run');
}
console.log('page result:', JSON.stringify(result));
if (!result || result.state !== 'done') fail(`session did not complete: ${JSON.stringify(result)}`);

// 4. What the runtime saw.
await new Promise((r) => setTimeout(r, 500));
const text = fs.existsSync(log) ? fs.readFileSync(log, 'utf8') : '';
const submitted = [...text.matchAll(/frames submitted: (\d+)/g)].map((m) => Number(m[1])).pop() || 0;
console.log(`runtime frames submitted (last milestone): ${submitted}`);
if (!text.includes('session') || submitted < 10) fail('runtime did not see frames');

if (!opt('--url')) {
  if (!fs.existsSync(bmp)) fail('no capture written');
  const b = fs.readFileSync(bmp);
  const w = b.readInt32LE(18), h = -b.readInt32LE(22);
  const px = (x, y) => { const o = 54 + (y * w + x) * 4; return [b[o + 2], b[o + 1], b[o]]; };
  const left = px(Math.floor(w / 4), Math.floor(h / 2));
  const right = px(Math.floor(3 * w / 4), Math.floor(h / 2));
  console.log(`capture ${w}x${h}: left eye rgb ${left}, right eye rgb ${right}`);
  const near = (c, want) => c.every((v, i) => Math.abs(v - want[i]) < 8);
  if (!near(left, [255, 0, 0]) || !near(right, [0, 255, 0])) fail('captured eyes are not red | green');
  // The scripted turn must reach the page's viewer pose, turning left.
  const expected = turnRate * (result.lastFrameAt - result.measureFrom) / 1000;
  console.log(`head yaw turned ${result.yawTurned.toFixed(3)} rad, expected about ${expected.toFixed(3)}`);
  if (Math.abs(result.yawTurned - expected) > 0.25 * expected) fail('viewer pose did not follow the head');
  // WASD path: the scripted forward walk must move the viewer at walking speed.
  const walkExpected = 1.4 * (result.lastFrameAt - result.measureFrom) / 1000;
  console.log(`walked ${result.walked.toFixed(3)} m, expected about ${walkExpected.toFixed(3)}`);
  if (Math.abs(result.walked - walkExpected) > 0.25 * walkExpected) fail('viewer did not walk');
  // Controllers: both hands present with the scripted buttons and stick.
  console.log('inputs:', JSON.stringify(result.inputs));
  const hand = (h) => (result.inputs || []).find((i) => i.hand === h);
  const L = hand('left'), R = hand('right');
  if (!L || !R) fail('two controllers expected');
  if (!R.ray || !R.grip || !L.ray || !L.grip) fail('controller poses missing');
  if (R.buttons[0] !== 1 || R.buttons[4] !== 1) fail('right trigger / A not pressed');
  if (L.buttons[1] !== 1) fail('left squeeze not pressed');
  if (L.axes[2] !== 0.5 || Math.abs(L.axes[3]) !== 1) fail('left thumbstick not pushed');
  console.log(`fps ${result.fps.toFixed(1)}, first frame ${(result.firstFrameAt - result.requestedAt).toFixed(0)} ms after requestSession`);
  // The headset runs at 90 Hz whatever the desktop monitor does; warm-up frames
  // pull the average a little below. On Linux Chrome itself delivers about 42
  // (xrmimic's loop still runs at 90; see README), so only a collapse fails there.
  const floor = win ? 80 : 30;
  if (result.fps < floor) fail(`frame rate ${result.fps.toFixed(1)} fps, expected at least ${floor}`);
}
// 5. The call trace: every entry point with its arguments and outputs.
{
  const trace = fs.existsSync(path.join(work, 'trace.log')) ? fs.readFileSync(path.join(work, 'trace.log'), 'utf8') : '';
  const lines = trace.split(/\r?\n/).filter(Boolean);
  const find = (re, what) => {
    const l = lines.find((x) => re.test(x));
    if (!l) fail(`trace has no ${what}`);
    return l;
  };
  find(/ xrCreateInstance\(info=\{app="Google Chrome[^"]*".*extensions=\[/, 'xrCreateInstance with app name and extensions');
  find(/ xrEndFrame\(.*layers=\[Projection\{.*rect=/, 'xrEndFrame with a projection layer');
  find(/ xrGetActionStateBoolean\(.*-> SUCCESS.*out=ActionStateBoolean/, 'xrGetActionStateBoolean with its output');
  find(/ xrLocateViews\(.*=> .*views=View/, 'xrLocateViews with its output');
  find(/ xrStringToPath\(.*s="\/user\/hand\/left".*=> .*"\/user\/hand\/left"/, 'xrStringToPath resolved');
  const names = new Set(lines.map((l) => (l.match(/\] (xr\w+)\(/) || [])[1]).filter(Boolean));
  console.log(`trace: ${lines.length} calls, ${names.size} distinct entry points`);
}

if (win) {
  // The sandboxed window asked the helper to switch, and got its answer.
  if (!text.includes('runtime switch: ok')) fail('runtime switch request was not answered');
  console.log('runtime switch request answered by the helper');
  const shot = path.join(work, 'window.bmp');
  console.log(fs.existsSync(shot) ? `window snapshot: ${shot}` : 'no window snapshot');
}
console.log('PASS');
finish(0);
