// XRMimic test page: a WebXR client with no dependencies. It requests the
// optional features ticked on the page, draws a small scene, and reports what
// the session got. window.testpage holds the same figures for scripts.

const FEATURES = ['local-floor', 'bounded-floor', 'unbounded', 'layers', 'hand-tracking', 'anchors',
  'hit-test', 'plane-detection', 'secondary-views'];

// Controller profile -> headset, the way pages tell headsets apart (WebXR
// never gives the headset's name).
const HEADSETS = {
  'meta-quest-touch-plus': 'Meta Quest 3 / 3S',
  'meta-quest-touch-pro': 'Meta Quest Pro',
  'oculus-touch-v3': 'Meta Quest 2',
  'oculus-touch-v2': 'Oculus Quest / Rift S',
  'oculus-touch': 'Oculus / Meta headset (Touch controllers)',
  'valve-index': 'Valve Index',
  'htc-vive': 'HTC Vive / Vive Pro',
  'htc-vive-cosmos': 'HTC Vive Cosmos',
  'hp-mixed-reality': 'HP Reverb G2',
  'samsung-odyssey': 'Samsung Odyssey',
  'microsoft-mixed-reality': 'Windows Mixed Reality headset',
  'generic-hand': 'hand tracking',
};

const $ = (id) => document.getElementById(id);
const state = window.testpage = { state: 'idle', granted: [], headset: null, layers: {}, controllers: [] };

// ---------------------------------------------------------------- page controls

for (const f of FEATURES) {
  const l = document.createElement('label');
  l.innerHTML = `<input type="checkbox" value="${f}" checked> ${f}`;
  $('features').append(l);
}
const requested = () => [...$('features').querySelectorAll('input:checked')].map((i) => i.value);

function kv(el, rows) {
  el.innerHTML = rows.map(([k, v]) => `<span>${k}</span><span>${v ?? ''}</span>`).join('');
}
const yes = (b) => (b ? '<span class="yes">yes</span>' : '<span class="no">no</span>');

if (!navigator.xr) {
  $('status').textContent = 'This browser has no WebXR (navigator.xr).';
} else {
  navigator.xr.isSessionSupported('immersive-vr').then((ok) => {
    $('status').textContent = ok ? 'immersive-vr is supported.' : 'immersive-vr is not supported by this browser or runtime.';
    $('enter').disabled = !ok;
    state.supported = ok;
  });
}
$('enter').addEventListener('click', () => (state.session ? state.session.end() : start()));

// ---------------------------------------------------------------- math

const deg = (r) => (r * 180) / Math.PI;
function fovOf(m) {
  const right = Math.atan((1 + m[8]) / m[0]), left = Math.atan((m[8] - 1) / m[0]);
  const up = Math.atan((1 + m[9]) / m[5]), down = Math.atan((m[9] - 1) / m[5]);
  return { left: deg(left), right: deg(right), up: deg(up), down: deg(down), h: deg(right - left), v: deg(up - down) };
}
function translate(x, y, z, s = 1) {
  return new Float32Array([s, 0, 0, 0, 0, s, 0, 0, 0, 0, s, 0, x, y, z, 1]);
}
function scaled(m, s) {
  const o = new Float32Array(m);
  for (let i = 0; i < 12; i++) if (i % 4 !== 3) o[i] *= s;
  return o;
}

// ---------------------------------------------------------------- GL scene

let gl, prog, uProj, uView, uModel, cube, grid, ray;
function initGL() {
  const canvas = document.createElement('canvas');
  gl = canvas.getContext('webgl2', { xrCompatible: true, antialias: true });
  const vs = `#version 300 es
  layout(location=0) in vec3 p; layout(location=1) in vec3 c;
  uniform mat4 proj, view, model; out vec3 col;
  void main() { col = c; gl_Position = proj * view * model * vec4(p, 1.0); }`;
  const fs = `#version 300 es
  precision mediump float; in vec3 col; out vec4 o; void main() { o = vec4(col, 1.0); }`;
  const sh = (t, src) => { const s = gl.createShader(t); gl.shaderSource(s, src); gl.compileShader(s); return s; };
  prog = gl.createProgram();
  gl.attachShader(prog, sh(gl.VERTEX_SHADER, vs));
  gl.attachShader(prog, sh(gl.FRAGMENT_SHADER, fs));
  gl.linkProgram(prog);
  uProj = gl.getUniformLocation(prog, 'proj');
  uView = gl.getUniformLocation(prog, 'view');
  uModel = gl.getUniformLocation(prog, 'model');
  const mesh = (data, mode) => {
    const vao = gl.createVertexArray();
    gl.bindVertexArray(vao);
    const b = gl.createBuffer();
    gl.bindBuffer(gl.ARRAY_BUFFER, b);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array(data), gl.STATIC_DRAW);
    gl.enableVertexAttribArray(0);
    gl.vertexAttribPointer(0, 3, gl.FLOAT, false, 24, 0);
    gl.enableVertexAttribArray(1);
    gl.vertexAttribPointer(1, 3, gl.FLOAT, false, 24, 12);
    return { vao, count: data.length / 6, mode };
  };
  // A unit cube, each face shaded.
  const c = [];
  const faces = [[0, 1], [0, -1], [1, 1], [1, -1], [2, 1], [2, -1]];
  for (const [axis, sign] of faces) {
    const shade = 0.55 + 0.45 * (axis === 1 ? (sign > 0 ? 1 : 0.3) : 0.6 + 0.2 * sign);
    const corner = (a, b) => {
      const v = [0, 0, 0];
      v[axis] = 0.5 * sign;
      v[(axis + 1) % 3] = a * 0.5;
      v[(axis + 2) % 3] = b * 0.5;
      return v;
    };
    for (const [a, b] of [[-1, -1], [1, -1], [1, 1], [-1, -1], [1, 1], [-1, 1]]) c.push(...corner(a, b), shade, shade, shade);
  }
  cube = mesh(c, gl.TRIANGLES);
  const g = [];
  for (let i = -5; i <= 5; i++) {
    const k = i === 0 ? 0.35 : 0.6;
    g.push(i, 0, -5, k, k, k, i, 0, 5, k, k, k, -5, 0, i, k, k, k, 5, 0, i, k, k, k);
  }
  grid = mesh(g, gl.LINES);
  ray = mesh([0, 0, 0, 1, 0.9, 0.2, 0, 0, -2, 1, 0.9, 0.2], gl.LINES);
}

// Coloured cubes around the start point, one straight ahead.
const SCENE = [
  [0, 1.5, -2, 0.3, [0.85, 0.25, 0.2]],
  [2, 1.0, -1, 0.3, [0.2, 0.6, 0.85]],
  [-2, 1.0, -1, 0.3, [0.3, 0.75, 0.35]],
  [0, 1.0, 2, 0.3, [0.9, 0.7, 0.2]],
  [1.5, 0.15, -3, 0.3, [0.6, 0.4, 0.85]],
];

function drawScene(proj, view, frame, space) {
  gl.useProgram(prog);
  gl.uniformMatrix4fv(uProj, false, proj);
  gl.uniformMatrix4fv(uView, false, view);
  gl.uniformMatrix4fv(uModel, false, translate(0, state.floorOffset, 0));
  gl.bindVertexArray(grid.vao);
  gl.drawArrays(grid.mode, 0, grid.count);
  gl.bindVertexArray(cube.vao);
  for (const [x, y, z, s] of SCENE) {
    gl.uniformMatrix4fv(uModel, false, translate(x, y + state.floorOffset, z, s));
    gl.drawArrays(cube.mode, 0, cube.count);
  }
  for (const src of state.session.inputSources) {
    // Tracked hands: a small cube per joint, sized by its radius; and the
    // thumb tip to index tip distance (the pinch) for the panel.
    if (src.hand && frame.getJointPose) {
      gl.bindVertexArray(cube.vao);
      for (const joint of src.hand.values()) {
        const jp = frame.getJointPose(joint, space);
        if (jp) {
          gl.uniformMatrix4fv(uModel, false, scaled(jp.transform.matrix, Math.max(jp.radius * 2, 0.008)));
          gl.drawArrays(cube.mode, 0, cube.count);
        }
      }
      const a = frame.getJointPose(src.hand.get('thumb-tip'), space);
      const b = frame.getJointPose(src.hand.get('index-finger-tip'), space);
      if (a && b) {
        const p = a.transform.position, q = b.transform.position;
        state.pinch = state.pinch || {};
        state.pinch[src.handedness] = Math.round(Math.hypot(p.x - q.x, p.y - q.y, p.z - q.z) * 1000);
      }
      continue;
    }
    const grip = src.gripSpace && frame.getPose(src.gripSpace, space);
    if (grip) {
      gl.bindVertexArray(cube.vao);
      gl.uniformMatrix4fv(uModel, false, scaled(grip.transform.matrix, 0.08));
      gl.drawArrays(cube.mode, 0, cube.count);
    }
    const aim = frame.getPose(src.targetRaySpace, space);
    if (aim) {
      gl.bindVertexArray(ray.vao);
      gl.uniformMatrix4fv(uModel, false, aim.transform.matrix);
      gl.drawArrays(ray.mode, 0, ray.count);
    }
  }
}

// ---------------------------------------------------------------- session

async function start() {
  const want = requested();
  state.state = 'requesting';
  state.requested = want;
  let session;
  try {
    session = await navigator.xr.requestSession('immersive-vr', { optionalFeatures: want });
  } catch (e) {
    state.state = 'error';
    state.error = String(e);
    $('status').textContent = `requestSession failed: ${e}`;
    return;
  }
  state.session = session;
  state.state = 'running';
  state.granted = session.enabledFeatures ? [...session.enabledFeatures] : [];
  $('enter').textContent = 'Exit VR';
  if (!gl) initGL();
  await gl.makeXRCompatible();

  let space, kind;
  for (kind of ['local-floor', 'local']) {
    try {
      space = await session.requestReferenceSpace(kind);
      break;
    } catch {}
  }
  state.space = kind;
  state.floorOffset = kind === 'local-floor' ? 0 : -1.6;
  state.bounds = null;
  if (state.granted.includes('bounded-floor')) {
    try {
      const b = await session.requestReferenceSpace('bounded-floor');
      state.bounds = b.boundsGeometry.map((p) => [+p.x.toFixed(2), +p.z.toFixed(2)]);
    } catch (e) {
      state.bounds = String(e);
    }
  }

  // Layers if granted: a projection layer plus a quad layer with text.
  let binding = null, proj = null, quad = null, quadCanvas = null, depth = null, cyl = null, cylCanvas = null;
  let sky = null, skyCanvas = null, cube = null, cubeCanvas = null;
  state.layers = { granted: state.granted.includes('layers') };
  if (state.layers.granted) {
    try {
      binding = new XRWebGLBinding(session, gl);
      proj = binding.createProjectionLayer({ depthFormat: gl.DEPTH_COMPONENT24 });
      state.layers.projection = 'ok';
      try {
        quad = binding.createQuadLayer({
          space, viewPixelWidth: 512, viewPixelHeight: 256, width: 1.0, height: 0.5,
          transform: new XRRigidTransform({ x: 0, y: 1.6 + state.floorOffset, z: -1.5 }),
        });
        quadCanvas = document.createElement('canvas');
        quadCanvas.width = 512;
        quadCanvas.height = 256;
        state.layers.quad = 'ok';
      } catch (e) {
        state.layers.quad = String(e);
      }
      // A cylinder layer around the viewer, behind the quad (needs the
      // runtime's XR_KHR_composition_layer_cylinder).
      try {
        cyl = binding.createCylinderLayer({
          space, viewPixelWidth: 1024, viewPixelHeight: 128, radius: 2.5, centralAngle: 1.6, aspectRatio: 8,
          transform: new XRRigidTransform({ x: 0, y: 2.1 + state.floorOffset, z: 0 }),
        });
        cylCanvas = document.createElement('canvas');
        cylCanvas.width = 1024;
        cylCanvas.height = 128;
        state.layers.cylinder = 'ok';
      } catch (e) {
        state.layers.cylinder = String(e);
      }
      // A sky band all around the viewer, at infinity (needs the runtime's
      // XR_KHR_composition_layer_equirect2).
      try {
        sky = binding.createEquirectLayer({
          space, viewPixelWidth: 1024, viewPixelHeight: 256, radius: 0,
          centralHorizontalAngle: 2 * Math.PI, upperVerticalAngle: 1.2, lowerVerticalAngle: 0.5,
        });
        skyCanvas = document.createElement('canvas');
        skyCanvas.width = 1024;
        skyCanvas.height = 256;
        state.layers.equirect = 'ok';
      } catch (e) {
        state.layers.equirect = String(e);
      }
      // A cube layer at infinity, transparent but for a label on each face
      // (needs the runtime's XR_KHR_composition_layer_cube).
      try {
        cube = binding.createCubeLayer({ space, viewPixelWidth: 256, viewPixelHeight: 256 });
        cubeCanvas = document.createElement('canvas');
        cubeCanvas.width = 256;
        cubeCanvas.height = 256;
        state.layers.cube = 'ok';
      } catch (e) {
        state.layers.cube = String(e);
      }
      session.updateRenderState({ layers: [proj, cube, sky, cyl, quad].filter(Boolean) });
    } catch (e) {
      state.layers.projection = String(e);
      binding = proj = quad = cyl = sky = cube = null;
    }
  }
  if (!proj) {
    session.updateRenderState({ baseLayer: new XRWebGLLayer(session, gl) });
    depth = null;
  }

  const fb = gl.createFramebuffer();
  const gaps = [];
  let last = 0, n = 0;
  gl.enable(gl.DEPTH_TEST);

  const frame = (t, xf) => {
    if (last) {
      gaps.push(t - last);
      if (gaps.length > 90) gaps.shift();
    }
    last = t;
    const pose = xf.getViewerPose(space);
    if (pose) {
      if (proj) {
        gl.bindFramebuffer(gl.FRAMEBUFFER, fb);
        for (const view of pose.views) {
          const sub = binding.getViewSubImage(proj, view);
          gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, sub.colorTexture, 0);
          gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.DEPTH_ATTACHMENT, gl.TEXTURE_2D, sub.depthStencilTexture, 0);
          const vp = sub.viewport;
          gl.viewport(vp.x, vp.y, vp.width, vp.height);
          gl.clearColor(0.93, 0.92, 0.9, 1);
          gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
          drawScene(view.projectionMatrix, view.transform.inverse.matrix, xf, space);
        }
        if (quad && (quad.needsRedraw || n % 45 === 0)) {
          const c = quadCanvas.getContext('2d');
          c.fillStyle = '#1f5fbf';
          c.fillRect(0, 0, 512, 256);
          c.fillStyle = '#fff';
          c.font = 'bold 34px system-ui, sans-serif';
          c.fillText('WebXR quad layer', 24, 70);
          c.font = '24px system-ui, sans-serif';
          c.fillText(state.headset?.name ?? '', 24, 130);
          c.fillText(`${(state.fps ?? 0).toFixed(0)} fps`, 24, 180);
          const sub = binding.getSubImage(quad, xf);
          gl.bindTexture(gl.TEXTURE_2D, sub.colorTexture);
          gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, gl.RGBA, gl.UNSIGNED_BYTE, quadCanvas);
          state.layers.quadDrawn = (state.layers.quadDrawn || 0) + 1;
        }
        if (cyl && (cyl.needsRedraw || n % 45 === 0)) {
          const c = cylCanvas.getContext('2d');
          const g = c.createLinearGradient(0, 0, 1024, 0);
          g.addColorStop(0, '#7b2fbf');
          g.addColorStop(1, '#1f8fbf');
          c.fillStyle = g;
          c.fillRect(0, 0, 1024, 128);
          c.fillStyle = '#fff';
          c.font = 'bold 40px system-ui, sans-serif';
          c.fillText('WebXR cylinder layer: it curves around you', 40, 80);
          const sub = binding.getSubImage(cyl, xf);
          gl.bindTexture(gl.TEXTURE_2D, sub.colorTexture);
          gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, gl.RGBA, gl.UNSIGNED_BYTE, cylCanvas);
          state.layers.cylinderDrawn = (state.layers.cylinderDrawn || 0) + 1;
        }
        if (sky && (sky.needsRedraw || n % 90 === 0)) {
          const c = skyCanvas.getContext('2d');
          const g = c.createLinearGradient(0, 0, 0, 256);
          g.addColorStop(0, '#0b1d4a');
          g.addColorStop(1, '#5b9be0');
          c.fillStyle = g;
          c.fillRect(0, 0, 1024, 256);
          c.fillStyle = '#fff';
          c.font = 'bold 28px system-ui, sans-serif';
          for (const [x, label] of [[0, 'S'], [256, 'W'], [512, 'N (ahead)'], [768, 'E']]) {
            c.fillText(`equirect sky: ${label}`, x + 20, 150);
          }
          const sub = binding.getSubImage(sky, xf);
          gl.bindTexture(gl.TEXTURE_2D, sub.colorTexture);
          gl.texSubImage2D(gl.TEXTURE_2D, 0, 0, 0, gl.RGBA, gl.UNSIGNED_BYTE, skyCanvas);
          state.layers.equirectDrawn = (state.layers.equirectDrawn || 0) + 1;
        }
        if (cube && (cube.needsRedraw || n % 90 === 0)) {
          // Faces in GL order +X -X +Y -Y +Z -Z; each a transparent square with a label.
          const sub = binding.getSubImage(cube, xf);
          gl.bindTexture(gl.TEXTURE_CUBE_MAP, sub.colorTexture);
          gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, true);
          const c = cubeCanvas.getContext('2d');
          ['+X', '-X', '+Y', '-Y', '+Z', '-Z'].forEach((label, i) => {
            c.clearRect(0, 0, 256, 256);
            c.fillStyle = 'rgba(20, 20, 20, 0.75)';
            c.fillRect(78, 98, 100, 60);
            c.fillStyle = '#ffd54a';
            c.font = 'bold 40px system-ui, sans-serif';
            c.textAlign = 'center';
            c.fillText(`cube ${label}`, 128, 142);
            gl.texSubImage2D(gl.TEXTURE_CUBE_MAP_POSITIVE_X + i, 0, 0, 0, gl.RGBA, gl.UNSIGNED_BYTE, cubeCanvas);
          });
          gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false);
          gl.bindTexture(gl.TEXTURE_CUBE_MAP, null);
          state.layers.cubeDrawn = (state.layers.cubeDrawn || 0) + 1;
        }
      } else {
        const layer = session.renderState.baseLayer;
        gl.bindFramebuffer(gl.FRAMEBUFFER, layer.framebuffer);
        gl.clearColor(0.93, 0.92, 0.9, 1);
        gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT);
        for (const view of pose.views) {
          const vp = layer.getViewport(view);
          gl.viewport(vp.x, vp.y, vp.width, vp.height);
          drawScene(view.projectionMatrix, view.transform.inverse.matrix, xf, space);
        }
      }
      measure(pose, session, proj, binding);
    }
    state.fps = gaps.length ? 1000 / [...gaps].sort((a, b) => a - b)[gaps.length >> 1] : 0;
    state.frames = ++n;
    if (n % 15 === 1) render();
    session.requestAnimationFrame(frame);
  };
  session.requestAnimationFrame(frame);
  session.addEventListener('end', () => {
    state.state = 'ended';
    state.session = null;
    $('enter').textContent = 'Enter VR';
    render();
  });
  render();
}

// What the views and inputs say about the headset.
function measure(pose, session, proj, binding) {
  const v = pose.views;
  const f = fovOf(v[0].projectionMatrix);
  let ipd = 0;
  if (v.length > 1) {
    const a = v[0].transform.position, b = v[1].transform.position;
    ipd = Math.hypot(a.x - b.x, a.y - b.y, a.z - b.z) * 1000;
  }
  let eye = null;
  if (proj) eye = { width: proj.textureWidth, height: proj.textureHeight };
  else {
    const l = session.renderState.baseLayer;
    eye = { width: Math.round(l.framebufferWidth / v.length), height: l.framebufferHeight };
  }
  state.controllers = [...session.inputSources].map((s) => ({
    hand: s.handedness,
    mode: s.targetRayMode,
    profiles: [...(s.profiles || [])],
    hand_joints: s.hand ? s.hand.size : 0,
    pinch_mm: state.pinch && s.hand ? state.pinch[s.handedness] : undefined,
    buttons: s.gamepad ? [...s.gamepad.buttons].map((b) => +b.value.toFixed(2)) : [],
    axes: s.gamepad ? [...s.gamepad.axes].map((a) => +a.toFixed(2)) : [],
  }));
  const profiles = state.controllers.flatMap((c) => c.profiles);
  const known = profiles.find((p) => HEADSETS[p]);
  state.headset = {
    name: known ? HEADSETS[known] : profiles[0] ? `VR headset (${profiles[0]})` : 'VR headset',
    eye, fov: f, ipdMm: ipd, views: v.length,
    blend: session.environmentBlendMode, interaction: session.interactionMode,
    frameRate: session.frameRate ?? null,
  };
}

function render() {
  const s = state;
  kv($('session'), [
    ['state', s.state],
    ['requested', s.requested?.join(', ')],
    ['granted', s.granted.map((f) => `<code>${f}</code>`).join(' ')],
    ['reference space', s.space],
    ['stage bounds', s.bounds ? JSON.stringify(s.bounds) : s.granted.includes('bounded-floor') ? '…' : 'not granted'],
    ['frames', s.frames],
  ]);
  const h = s.headset;
  if (h) {
    kv($('headset'), [
      ['headset', `<b>${h.name}</b>`],
      ['per eye', h.eye ? `${h.eye.width} × ${h.eye.height} px` : ''],
      ['field of view', `${h.fov.h.toFixed(0)}° × ${h.fov.v.toFixed(0)}° (left eye ${h.fov.left.toFixed(0)}/${h.fov.right.toFixed(0)}/${h.fov.up.toFixed(0)}/${h.fov.down.toFixed(0)}°)`],
      ['IPD', `${h.ipdMm.toFixed(1)} mm`],
      ['frame rate', `${(s.fps ?? 0).toFixed(0)} fps${h.frameRate ? ` (headset ${h.frameRate} Hz)` : ''}`],
      ['display', `${h.blend}, ${h.interaction}`],
    ]);
  }
  const L = s.layers || {};
  kv($('layers'), [
    ['layers feature', yes(L.granted)],
    ['projection layer', L.projection ?? (L.granted ? '' : 'not used (XRWebGLLayer)')],
    ['quad layer', L.quad ?? '—'],
    ['cylinder layer', L.cylinder ?? '—'],
    ['equirect layer', L.equirect ?? '—'],
    ['cube layer', L.cube ?? '—'],
    ['quad redraws', L.quadDrawn ?? 0],
  ]);
  $('controllers').innerHTML = s.controllers.length
    ? `<table><tr><th>hand</th><th>mode</th><th>profiles</th><th>buttons</th><th>axes</th><th>hand joints</th></tr>${
      s.controllers.map((c) => `<tr><td>${c.hand}</td><td>${c.mode}</td><td>${c.profiles.join('<br>')}</td><td>${c.buttons.join(' ')}</td><td>${c.axes.join(' ')}</td><td>${c.hand_joints || '—'}</td></tr>`).join('')
    }</table>`
    : 'none reported';
}
