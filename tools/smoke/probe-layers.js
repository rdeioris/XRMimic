// Probe: does this Chrome offer WebXR Layers against the runtime?
// Evaluated in the smoke page by `node tools/smoke/smoke.mjs --eval-file tools/smoke/probe-layers.js`,
// with the layer extensions ticked for the run, e.g.
// XRMIMIC_SMOKE_DEVICE="extensions=XR_EXT_local_floor,XR_KHR_composition_layer_cylinder,XR_KHR_composition_layer_equirect2,XR_KHR_composition_layer_cube".
// Returns what the page could do; the runtime's trace shows what reached xrEndFrame.
(async () => {
  const out = window.__probe = {
    stage: 'start',
    ua: navigator.userAgent.match(/Chrome\/[\d.]+/)?.[0],
    createQuadLayer: typeof XRWebGLBinding === 'function' && 'createQuadLayer' in XRWebGLBinding.prototype,
    createProjectionLayer: typeof XRWebGLBinding === 'function' && 'createProjectionLayer' in XRWebGLBinding.prototype,
  };
  let s;
  out.stage = 'requestSession';
  try {
    s = await navigator.xr.requestSession('immersive-vr', { optionalFeatures: ['layers', 'local-floor'] });
  } catch (e) {
    out.requestError = String(e);
    return out;
  }
  out.enabledFeatures = s.enabledFeatures ? [...s.enabledFeatures] : 'n/a';
  s.addEventListener('end', () => { out.endedByBrowserAt = out.endedByBrowserAt || out.stage; });
  out.stage = 'makeXRCompatible';
  const gl = document.createElement('canvas').getContext('webgl2', { xrCompatible: true });
  await gl.makeXRCompatible();
  out.stage = 'requestReferenceSpace';
  const space = await s.requestReferenceSpace('local');
  out.stage = 'layers';
  let binding, proj;
  const layers = [];
  try {
    binding = new XRWebGLBinding(s, gl);
    proj = binding.createProjectionLayer({ textureType: 'texture' });
    layers.push(['projection', proj, [0, 0, 1]]);
    out.projection = 'ok';
  } catch (e) {
    out.projection = String(e);
  }
  const make = (name, f, color) => {
    try {
      layers.push([name, f(), color]);
      out[name] = 'ok';
    } catch (e) {
      out[name] = String(e);
    }
  };
  if (binding) {
    make('quad', () => binding.createQuadLayer({
      space, viewPixelWidth: 256, viewPixelHeight: 256, width: 0.5, height: 0.5,
      transform: new XRRigidTransform({ x: 0, y: 0, z: -1.5 }),
    }), [1, 1, 0]);
    make('cylinder', () => binding.createCylinderLayer({
      space, viewPixelWidth: 512, viewPixelHeight: 128, radius: 2, centralAngle: 1, aspectRatio: 4,
    }), [1, 0, 1]);
    make('equirect', () => binding.createEquirectLayer({ space, viewPixelWidth: 256, viewPixelHeight: 128 }), [0, 1, 1]);
  }
  try {
    s.updateRenderState(proj ? { layers: layers.map((l) => l[1]) } : { baseLayer: new XRWebGLLayer(s, gl) });
    out.updateRenderState = 'ok';
  } catch (e) {
    out.updateRenderState = String(e);
  }
  const fb = gl.createFramebuffer();
  let frames = 0;
  out.stage = 'frames';
  await new Promise((res) => {
    setTimeout(() => { out.frameTimeout = true; res(); }, 5000);
    const frame = (t, xf) => {
      try {
        const pose = xf.getViewerPose(space);
        gl.bindFramebuffer(gl.FRAMEBUFFER, fb);
        for (const [name, layer, c] of layers) {
          const subs = name === 'projection' ? (pose ? pose.views.map((v) => binding.getViewSubImage(layer, v)) : [])
            : [binding.getSubImage(layer, xf)];
          for (const sub of subs) {
            gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, sub.colorTexture, 0);
            const vp = sub.viewport;
            gl.viewport(vp.x, vp.y, vp.width, vp.height);
            gl.clearColor(c[0], c[1], c[2], 1);
            gl.clear(gl.COLOR_BUFFER_BIT);
          }
        }
      } catch (e) {
        out.frameError = String(e);
      }
      if (++frames >= 60) res();
      else s.requestAnimationFrame(frame);
    };
    s.requestAnimationFrame(frame);
  });
  out.frames = frames;
  out.stage = 'end';
  if (!out.endedByBrowserAt) {
    try {
      await s.end();
    } catch (e) {
      out.endError = String(e);
    }
  }
  out.stage = 'done';
  return out;
})()
