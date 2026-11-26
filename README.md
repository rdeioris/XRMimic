# XRMimic

An OpenXR runtime for Windows that simulates a headset in a desktop window, so
WebXR pages in desktop Chrome (`immersive-vr`) can be tested without hardware.
It pretends to be a Quest 2: 1832×1920 per eye, its field of view, a 63 mm IPD,
90 Hz and two Touch controllers.

Chrome renders into XRMimic's swapchains as it would for a real headset; the
headset window shows both eyes side by side, and mouse, keyboard or a gamepad
move the head and drive the controllers.

## Build

Needs Rust (stable) and Windows 10/11 with a D3D11 GPU.

```powershell
cargo build --release
```

This builds `target\release\xrmimic_runtime.dll` (the runtime) and
`target\release\xrmimic-launch.exe` (a helper the launcher starts).

## Run

```powershell
.\xrmimic-chrome.ps1                                  # the XRMimic test page
.\xrmimic-chrome.ps1 -Url "http://localhost:8092/"    # any WebXR page
```

The launcher starts Chrome on its own profile (`%LOCALAPPDATA%\xrmimic\chrome-profile`)
with `XR_RUNTIME_JSON` pointing at a fresh copy of the runtime, so only that
Chrome uses XRMimic and nothing system-wide changes. Press the page's Enter VR
button and allow VR once; the **XRMimic** window opens.

### Test page

Without `-Url` the launcher opens XRMimic's own WebXR client (`tools/testpage`),
served on http://localhost:8771/ by `xrmimic-launch serve` until that Chrome
exits. It requests the optional features you tick (`local-floor`,
`bounded-floor`, `unbounded`, `layers`, `hand-tracking`, `anchors`, `hit-test`,
`plane-detection`, `secondary-views`), draws a small scene with the
controllers, and shows which features the session got, the headset as WebXR
sees it, live controller state, and a quad layer when `layers` is granted.
`window.testpage` holds the same for scripts.

Close the XRMimic Chrome before launching again: a second launch on the same
profile would be handed to the running one, which keeps its old runtime.

## Controls (headset window)

| Input | Does |
| --- | --- |
| Left drag | look around (the cursor stays visible) |
| Right drag | pan: the view follows the cursor, the head moves in its view plane |
| Wheel | zoom: the head moves forward / back along its gaze |
| W A S D | walk; Shift runs |
| Q / E | down / up |
| ← / → | turn |
| Tab | switch which controller the keys drive (shown in the title) |
| F | trigger of that controller |
| G | grip |
| I J K L | thumbstick |
| 1 / 2 | A/X, B/Y |
| 3 / M | thumbstick click, menu |
| Ctrl + left drag | move that controller in the view plane (it stays where you put it) |
| Ctrl + wheel | move it nearer / further along the gaze |
| Ctrl + right drag | turn it: left / right about the vertical, tilt up / down |
| R | that controller follows the head again |
| Gamepad | sticks = the two thumbsticks, triggers, bumpers = grips, A/B/X/Y, Start = menu, d-pad walks and turns |
| Close the window | ends the WebXR session |

The controllers sit at a fixed offset from the head and aim where you look,
until you move one by itself (Ctrl + drags and wheel, on the controller Tab
picked): from then on it stays where you put it in the room, pointing where
you turned it, while the head walks and looks around, so you can reach for
things, hold a controller still while you look at it from elsewhere, or walk
away from it. The Input tab shows "(put down; R)" by its hand; **R** gives
the active controller back to the head.

**Haptics.** What the app sends a controller (`xrApplyHapticFeedback`) shows
as an orange bar down that hand's side of the view while it plays (wider and
redder the stronger it is; a short pulse stays visible about 0.1 s), in the
Input tab under each hand ("Haptic: buzzing 0.80 at 160 Hz for 100 ms", then
the last one and how many were sent), in the Logs, and on the first XInput
gamepad's motors (left motor, left hand). `XR_MIN_HAPTIC_DURATION` is a 10 ms
pulse and `XR_INFINITE_DURATION` plays until `xrStopHapticFeedback` or the
next pulse. The calls check what a runtime must: a vibration action, its set
attached, a subaction path the action has, and a focused session (otherwise
`XR_SESSION_NOT_FOCUSED`, nothing played).

**Play area** (bounded floor): the device has a play area, 2.5 x 2.5 m for
the Quest and Reverb presets and 3.5 x 3 m for the room-scale Index and Vive
(Device tab, "Play area"; `play_area_m=3x2.5`, `0` for none), centred on the
STAGE origin (the floor where the session started). `xrGetReferenceSpaceBoundsRect`
reports it for STAGE (`XR_SPACE_BOUNDS_UNAVAILABLE` with none, and for other
spaces); changing it while a session runs sends the app
`XR_TYPE_EVENT_DATA_REFERENCE_SPACE_CHANGE_PENDING` for STAGE and the new
bounds after that. When the head comes within 1.2 m of the edge the view shows
the boundary as a blue grid wall, 2.5 m tall, fading in as you get closer.

**Depth** (toolbar checkbox): shows each eye's depth buffer instead of its
picture, when the app sends one with its projection layer
(`XR_KHR_composition_layer_depth`): greys by distance, 1 / (1 + d/2), so white
at the eye, mid grey at 2 m, darker further away. Ordinary, reversed and
infinite depth ranges are read as the app declares them. Neither Chrome 157
(Windows) nor Chrome 158 (Linux) enables the extension, so only native OpenXR
apps fill it.

**Centre** (toolbar checkbox): one undistorted picture from a camera halfway
between the eyes instead of the two eye images, for screenshots, recordings
and screen sharing. Nothing is rendered again: the eyes (with their layers, or
the Depth view) are composed off-screen and the centre camera reads them, each
direction from the left eye on the left and the right eye on the right, with
a soft seam between. Without depth that treats everything as far away, so
near things are off by half the IPD (about 1.8 degrees at 1 m, 3.6 at 0.5 m);
when both eyes send depth (native apps, not Chrome) each centre ray is marched
through the eyes' depth buffers to the surface it hits, and near things land
where a centre camera would see them.

**Reproject** (toolbar checkbox): each eye is drawn again from where it is
now rather than where the app drew it, the way a headset's compositor hides a
late frame (positional timewarp). The same pass as the centre view reads the
frame: with both eyes' depth each ray is marched through it, so near and far
things shift by their own parallax; without depth the frame is only turned
(rotation, everything treated as far away). Where the newer pose sees what
the frame never drew, the gap is black. It shows what the app's frames would
look like on a headset that reprojects; the app sees no difference.

**Depth-tested layers** (`XR_FB_composition_layer_depth_test`, offered by
every preset): a quad, cylinder, equirect or cube layer that chains
`XrCompositionLayerDepthTestFB` is compared per pixel with the projection
layer's depth for that eye (its distance along the view axis against the
depth's), with the layer's compare op (less: the layer shows where it is
nearer), so a panel behind a wall the app drew is hidden by it. Without depth
from that eye the layer is drawn over as usual. `depthMask` is accepted; layers
write no depth of their own.

A native 3D scene to look at it with, depth included (a room with a table,
pillars, far boxes, a cube 35 cm and a rod 50 cm from the eyes; WASD and
the mouse move you; it runs until the window is closed):

```powershell
cargo run --release -p xrmimic-runtime --example scene -- centre
```

`-- nodepth` sends no depth, to compare. With `XRMIMIC_TEST_SNAPSHOT` it also
renders the scene from a real camera between the eyes and counts the pixels
where the window's centre view differs: about 0.5% with depth (object
outlines), 2.5% without (the near objects and the table's edges shift).

### The window

The headset view is always on the left, with the runtime list and the
**Reproject**, **Depth** and **Centre** checkboxes above it. On the right a panel holds three
tabs, **Logs**, **Device** and **Input** (Input first); untick **Panel** to give
the view the whole window. A click in the panel hands the keys back to the
view, so WASD keeps working.

### Logs tab

The **Logs** tab lists every OpenXR call the runtime has
received, newest last: sequence number, time, thread, the call, its result
and duration, its arguments as the app passed them (structs read from the
app's pointers, paths and action names spelled out) and, after a successful
call, what the runtime wrote back. It keeps the last 50,000 calls (Chrome makes
about 5,000 a second while a session runs).

- **Filter** shows only rows containing the text (call name, arguments, outputs or result).
- **Live** follows new calls; untick it to freeze the list while you read it.
- **Hide per-frame calls** leaves out what an app calls on every frame (the
  frame loop, view and space locations, action sync and state, swapchain image
  calls, and event polls that found nothing), so setup, teardown and session
  events stand out. Polls that delivered an event stay.
- **Clear** empties the record. Double-click a row for its full text (Ctrl+C copies it).

`XRMIMIC_TRACE_FILE=<path>` also writes every call to a file (the only way on
Linux, which has no window); `XRMIMIC_TRACE=0` turns the record off.

### Input tab

Every input of the controllers' interaction profile, one row per input with
the left and the right hand's controls side by side (`x/click | a/click` when
the hands name it differently): the profile the app's session attached with
(before an app attaches, the Device tab's controllers). Buttons and touches
are checkboxes ("held"), trigger and squeeze sliders (0..1), sticks and
trackpads a round pad: drag the dot, it stays where you let go; right-click or
double-click centres it. "Release" lets go of a hand. The paths are the OpenXR
specification's for that profile (Touch: X/Y and menu on the left, A/B on the
right, thumbrest; Index: trackpad beside the stick, squeeze force, system;
Vive: trackpad, no stick; WMR; HP; simple controller: select and menu). Each
row shows the app's actions bound to that input (under its name) and what the
app reads now (beside each control).
What the tab holds stays until changed and combines with the keyboard and
gamepad (the larger value or any press wins); a press counts as a touch.

`cargo run --release -p xrmimic-runtime --example scene` has controllers to try it
on: each is drawn at its grip, red as its trigger is pulled, bigger as it is
squeezed, green while A/X is held, with a marker its stick moves. Its test
(`XRMIMIC_TEST_SNAPSHOT=<out.bmp> ... --example scene -- input`) sets inputs
through the tab (`XRMIMIC_TEST_INPUT`) and checks what its actions read.

### Hands

**Hands** at the top of the Input tab (or the **H** key) puts the controllers
down. The session then reports the first hand interaction profile the app
suggested bindings for (`XR_EXT_hand_interaction`'s
`/interaction_profiles/ext/hand_interaction_ext`, then
`XR_MSFT_hand_interaction`'s `/interaction_profiles/microsoft/hand_interaction`;
none if the app suggested neither) and tells the app
(`XR_TYPE_EVENT_DATA_INTERACTION_PROFILE_CHANGED`); unticking brings the
controllers back the same way. `XR_EXT_hand_tracking` hand trackers report 26
joints per hand (OpenXR's joint frames: -Z along the bone, +Y out of the back
of the hand) where the controllers were, palms turned in. The Input tab's rows
become the profile's pinch and grasp (the trigger and squeeze), a curl per
finger, and "out of view" (the hand stops being tracked). A pinch brings the
thumb tip to the index tip; a grasp closes the hand. `XRMIMIC_HANDS=1` starts with
hands. The Quest presets offer `XR_EXT_hand_tracking` and
`XR_EXT_hand_interaction` (the Device tab ticks them; the Microsoft profile can
be ticked too).

What Chrome does with them (157 on Windows, 158 on Linux): it enables both
extensions, suggests bindings for the EXT hand profile, and its input sources
become hands (`generic-hand-select-grasp`; pinch is `select`, grasp the last
button). `hand-tracking` is a permission of its own ("hand-tracking" in the
site settings, default Ask): Chrome prompts for it, and an unanswered prompt
(automation, an off-screen window) denies the feature, so the page gets
pointers with no `hand`. Granted (answer the prompt, or the smoke's
`--grant handTracking`, DevTools' `Browser.grantPermissions`), every input
source has a `hand` with 25 joints (WebXR drops the palm). By default Chrome
then replaces the joints with a standard-sized hand (its "fallback
anonymization", because XRMimic offers no `XR_FB_hand_tracking_mesh` to scale
with): the pose and pinch follow XRMimic, the bone lengths and radii are
Chrome's. `--webxr-hand-anonymization-strategy=none` passes XRMimic's joints
through unchanged (checked: every distance and radius equal, pinched tips 7 mm
apart). Native apps get the joints too: the scene draws them, and its test
(`XRMIMIC_TEST_SNAPSHOT=<out.bmp> ... --example scene -- hands`) pinches the right
hand and closes the left through the Input tab and checks the profile, the
actions (pinch -> trigger 1.00, grasp -> squeeze 1.00) and the joints (thumb and
index tips 7 mm apart, the fist's middle fingertip 3 cm from the palm).

### Device tab

Sets what the runtime reports about the headset: start from a preset (Quest 2,
Quest 3, Quest Pro, Valve Index, Vive Pro 2, Reverb G2; figures approximate) or
edit the system name, vendor ID, per-eye size, field of view, IPD, eye height,
refresh rate, play area and the controllers' interaction profile, then **Apply**. The
settings are saved (`%LOCALAPPDATA%\xrmimic\runtime\device.ini` from the
launcher; `XRMIMIC_DEVICE_FILE` elsewhere) and also work on Linux as that file or
`XRMIMIC_DEVICE=<preset>`. Size, refresh rate, name and controllers apply at the
next Enter VR; IPD, field of view and the play area at once.

**Advertised extensions.** The right-hand column ticks which optional OpenXR
extensions the runtime offers (the graphics binding and sandbox ones are always
on): `XR_EXT_local_floor`, `XR_MSFT_unbounded_reference_space` (an
`UNBOUNDED_MSFT` space with its origin where the head started, like LOCAL,
and no bounds), the Quest 3 / Quest Pro / Reverb G2 controller
extensions, the cylinder, equirect and cube layer extensions (drawn), the
depth layer extension (shown by the Depth checkbox, used by Centre and
Reproject) and depth-tested layers. Each preset ticks the
floor and unbounded spaces, the three layer shapes, depth and its own controllers' extension. Chrome reads the list once per OpenXR instance, which on Windows
lives as long as Chrome, so extension changes apply after restarting Chrome.
Chrome enables what it knows of the list, and WebXR features follow from that
and Chrome's version: unticking `XR_EXT_local_floor` does not take `local-floor`
from a page (Chrome then derives it), while quad layers need no extension at all.
Settings key: `extensions=<comma list>`.

What a WebXR page can tell from these:

| Preset | `inputSources[].profiles[0]`, Chrome 157 Windows | Chrome 158 Linux |
| --- | --- | --- |
| Quest 2 (name "Oculus Quest2") | `oculus-touch-v3` | `oculus-touch-v3` |
| Quest 3 | `oculus-touch` (Chrome 157 does not enable the Touch Plus extension) | `meta-quest-touch-plus` |
| Quest Pro | `oculus-touch` | `meta-quest-touch-pro` (not measured) |
| Valve Index | `valve-index` | |
| Vive Pro 2 | `htc-vive` | |
| Reverb G2 | `hp-mixed-reality` | |

Layers: Chrome 157 on Windows never grants the `layers` feature; Chrome 158 on
Linux does, and projection, quad, cylinder and equirect layers reach XRMimic
(`node tools/smoke/smoke.mjs --eval-file tools/smoke/probe-layers.js` probes it).
Chrome 158 also enables the cube extension and `createCubeLayer` succeeds, but
Chrome never creates a cube swapchain nor sends the layer: cube layers are
only exercised by the native client. The headset window draws quad, cylinder,
equirect and cube layers (see Limits).

Chrome derives `oculus-touch-v3` from the exact system name "Oculus Quest2"
(what Meta's runtime reports); other names give `oculus-touch`. Pages also see
the per-eye size, field of view, IPD and frame rate, never the name itself.

### Switching the system runtime

The list at the top of the window shows the OpenXR runtimes installed on this
machine (from `HKLM\SOFTWARE\Khronos\OpenXR\1\AvailableRuntimes`, SteamVR, and
this checkout's `xrmimic.json`), with the system's active one marked ●. Picking
another one makes it the system's `ActiveRuntime` for every OpenXR app; Windows
asks for administrator approval each time. To undo, pick the previous one.

The window runs inside Chrome's sandbox, so the switch is done by the launcher's
helper (`xrmimic-launch watch`), which runs outside it and exits with Chrome.

## How Chrome uses it

What Chrome 157 asks of the runtime (logged by XRMimic):

- OpenXR 1.0 with `XR_KHR_D3D11_enable`, `XR_KHR_win32_convert_performance_counter_time`,
  `XR_EXT_local_floor` and, when sandboxed, `XR_EXT_win32_appcontainer_compatible`
  (Chrome drops runtimes without it).
- One 3664×1920 `DXGI_FORMAT_R8G8B8A8_UNORM_SRGB` swapchain, both eyes side by side.
- LOCAL, VIEW and LOCAL_FLOOR reference spaces.
- Bindings for the Microsoft, simple, Touch, Index and Vive controller profiles;
  XRMimic answers as Touch.
- Chrome's XR process runs in an AppContainer: the runtime DLL, manifest and log
  must be in a folder granted to `ALL APPLICATION PACKAGES`. The launcher uses
  `%LOCALAPPDATA%\xrmimic\runtime` for that.
- Chrome asks the user for permission before a page may use VR.
- `bounded-floor` works: Chrome 157 reads STAGE's bounds and the page's
  `boundsGeometry` is the play area's corners (empty when read before the
  first frame: Chrome passes the bounds with the frame data).
- No haptics from WebXR: Chrome 157 gives the controllers' gamepads neither
  `hapticActuators` nor a `vibrationActuator`, so pages cannot send any.
  Native apps can (the scene pulses on each trigger pull).
- Chrome's swapchain is single-sampled (WebXR antialiasing happens in the
  page's own framebuffer), so MSAA swapchains are for native apps.

**WebXR through WebGPU** (`XRGPUBinding`, a WebGPU device drawing straight
into the headset's layer). Chrome 157 has it on Windows, behind two switches
the launcher and the smoke check pass:

- `--enable-blink-features=WebXRGPUBinding` makes `XRGPUBinding` exist in the
  page (an experimental Blink feature);
- `--enable-features=WebXRWebGPUBinding` lets the browser give a session that
  asks for `requiredFeatures: ['webgpu']` its OpenXR swapchain as a shared
  image WebGPU can write (off by default; Chrome keeps only the last
  `--enable-features`, so add others to the same list).

The page asks for an adapter with `xrCompatible: true`, makes
`new XRGPUBinding(session, device)`, a projection layer
(`createProjectionLayer`, `bgra8unorm` preferred), and draws each view into
`getViewSubImage(layer, view).colorTexture` (a 2-layer array, one layer per
eye; `getViewDescriptor()` picks it). Chrome asks for an sRGB swapchain either
way; XRMimic makes Chromium's sRGB images plain RGBA8 underneath, the one
format both of Chrome's halves accept: its shared-image code refuses a typed
sRGB texture (the XR process dies after the first `xrBeginFrame`) and Dawn a
typeless one (the page's device is lost: "Unsupported DXGI format 1b"). The
bytes are the same as before for WebGL sessions, which write sRGB-encoded
values through linear views. `node tools/smoke/smoke.mjs --webgpu` checks the
WebGPU path (red | green eyes, through `XRGPUBinding`).

The runtime logs every entry point it is given and the first calls of each to
`XRMIMIC_LOG` (the launcher prints where).

## Linux

Chrome 157 and newer can do WebXR through OpenXR on Linux, behind
`--enable-features=OpenXR`, with a Vulkan binding (`XR_KHR_vulkan_enable2`).
XRMimic builds there as `libxrmimic_runtime.so`: it creates Chrome's Vulkan instance
and device, hands out swapchain images on that device, and simulates the user
exactly as on Windows. There is no headset window on Linux yet: the user is
driven by the `XRMIMIC_TEST_*` hooks only, and frames can be checked with
`XRMIMIC_CAPTURE`.

```sh
cargo build --release -p xrmimic-runtime
tools/linux/headless-compositor.sh        # only without a desktop session
export XDG_RUNTIME_DIR=/run/user/$(id -u) WAYLAND_DISPLAY=xrmimic-wl
./xrmimic-chrome.sh http://localhost:8092/
node tools/smoke/smoke.mjs --no-sandbox   # the same smoke check
```

Linux notes:

- Chrome needs a Wayland compositor (or an X server with DRI3) to pass WebXR
  frames to the runtime; under plain `--headless` its GPU process cannot import
  them (`Could not find SharedImageBackingFactory ... OpenXrVulkanBinding`) and
  every frame arrives empty. `tools/linux/headless-compositor.sh` runs GNOME Shell
  headless for that, without root.
- A Chrome unpacked without root has no sandbox helper, so it runs with
  `--no-sandbox`; Chrome's Linux XR sandbox with XRMimic is untested.
- XRMimic's frame loop runs at 90 Hz on Linux too, but Chrome 158 under headless
  GNOME Shell delivers about 42 new page frames per second, resubmitting the
  previous one in between. The 120 Hz virtual monitor and both ANGLE backends
  gave the same figure.

## Tests

```powershell
cargo test --workspace
python tests\xrmimic_test.py
node tools\smoke\smoke.mjs
```

`tests/xrmimic_test.py` checks every feature from Python with nothing but the
standard library: `ctypes` loads `xrmimic_runtime.dll` the way the OpenXR
loader does, makes a D3D11 device through `d3d11.dll`, and renders with plain
clears and texture uploads; `unittest` runs 19 tests (about a minute), each
in its own process because XRMimic reads some settings once per process. They
cover the API (extensions, properties, strings and paths, the session's
states, reference spaces and STAGE bounds, swapchain formats and the errors a
runtime must give, frame pacing, time conversion, actions, bound sources,
haptics), the simulated head motion, a controller put down by itself, the
Input tab, hands, a device preset and a play-area change, and in the window's
snapshots: projection, quad (both eyes, one eye), cylinder, equirect and cube
layers, MSAA colour and depth, the Depth and Centre views, reprojection with
and without depth, depth-tested layers, the boundary and the haptics bar.
`-v -k <name>` runs one; `--scenario <name>` runs one scenario in this process
and prints its checks; `XRMIMIC_TEST_DLL` points it at another build (a build
with six features broken on purpose fails exactly the tests for them).

The unit tests cover the pose and projection math, frame pacing, binding paths
and the runtime list. The smoke check starts its own off-screen Chrome against
the runtime, grants the VR permission through DevTools (no prompt, no input on
your desktop), and checks that:

- a session starts and the submitted eyes are captured red | green as drawn;
- a scripted head turn and walk reach the page's viewer pose;
- both Touch controllers appear with scripted trigger, A, grip and stick values;
- closing the headset ends the session, and a new session starts after it;
- a runtime-switch request from the sandboxed window reaches the helper
  (dry run: the registry is not touched).

`--webgpu` draws the first session through WebGPU (`XRGPUBinding`) instead of
WebGL. `--url <page> --click <selector>` drives someone else's page through
its own Enter VR control instead (standing still: the scripted walk and the
headset close are only for the smoke's page); `--read <expression>` prints
what the page shows afterwards, `--run-ms` sets how long the session runs and
`--capture-frame <n>` which frame `XRMIMIC_CAPTURE` keeps (`capture.bmp` in
the work folder):

```powershell
node tools\smoke\smoke.mjs --url "https://rdeioris.github.io/renderia-furnish/?seed=5&furnish" --click "#vr" --run-ms 12000 --capture-frame 600 --read "document.getElementById('vr').title"
```

Quad layers are checked without a browser (Chrome 157 on Windows cannot send
them) by a native OpenXR client that drives the runtime directly:

```powershell
$env:XRMIMIC_TEST_SNAPSHOT = "$env:TEMP\quad.bmp"; cargo run --release -p xrmimic-runtime --example quad_client
```

It submits a blue projection layer, a mostly transparent cube layer turned
90 degrees (two patches of its +X face below the horizon), an equirect band at infinity above the
horizon (orange / white over purple / black), a cylinder layer around the start point
(radius 2 m, 1 rad wide, magenta left part, cyan right part), an opaque quad
1.5 m ahead (yellow top, red bottom) and a green left-eye-only quad, lets the
headset window photograph itself, then projects the layers with the eye poses
it was given and checks the colours where they must be: position, size,
curvature, orientation, layer order and eye visibility. Without
`XRMIMIC_TEST_SNAPSHOT` it just shows them for 60 s, on top of other windows.
Each eye also sends a depth buffer (left: 1 m, ordinary Z; right: 4 m,
reversed infinite Z); with the argument `depth` (`... --example quad_client
-- depth`) the window starts in its depth view and the client checks the
greys instead. `-- centre` and `-- centre-nodepth` paint two stripes on a
wall 0.5 m ahead where each eye sees them and check that the centre view puts
them where a centre camera sees them (with depth) or where the nearer eye
does (without). `-- reproject` / `-- reproject-nodepth` send frames that claim
eye poses 6 cm left of the real ones, with the stripes where those stale eyes
see them, and check that the reprojected eyes move them to where the eyes see
them now (with depth) or leave them (rotation only). `-- depthtest` depth-tests
the yellow and red quad 1.5 m ahead: hidden behind the left eye's 1 m, shown in
front of the right eye's 4 m. `msaa` added to any mode makes every swapchain
but the cube map 4x MSAA, colour and depth, and the same checks must pass.

The scene's `-- features` test (`XRMIMIC_TEST_SNAPSHOT=<out.bmp> ... --example
scene -- features`) checks haptics (a pulse, a stop, a pulse on an input
action refused), STAGE's bounds, and that a controller put down by itself
(`XRMIMIC_TEST_HAND_PUT=x,y,z`) stays put while the head walks 3 m
(`XRMIMIC_TEST_MOVE`), past the boundary.

`tools\build-runtime.sh` rebuilds the DLL even while a Chrome holds it loaded.

## Limits

- Windows (D3D11, headset window) and Linux (Vulkan, headless only).
- The headset runs at 90 Hz (`XRMIMIC_REFRESH=72` for 72) independently of the
  desktop monitor; the window shows the newest frame at the monitor's rate.
- Layers drawn in the headset window: one projection layer and up to eight
  quad, cylinder, equirect and cube layers in submission order (opaque,
  premultiplied or straight alpha, per-eye visibility), drawn from the eye
  poses of the frame they came with. An equirect layer at infinity (radius 0
  or +inf) and every cube layer are drawn 500 m away, centred on each eye;
  cube maps use the standard Vulkan / D3D face lookup. Linux has no window,
  so no layers are drawn there (cube swapchains are accepted).
- Depth buffers (`XR_KHR_composition_layer_depth`) are validated, traced and
  used by the Depth, Centre and Reproject views and depth-tested layers (D16,
  D24S8, D32, D32S8 formats; MSAA ones through their first sample).
- Hands are posed from curls and a pinch, not tracked motion; no hand meshes
  (`XR_FB_hand_tracking_mesh`).
- MSAA swapchains: 2x and 4x where the device supports the format (resolved
  for the window; no MSAA cube maps or mip chains). On Linux they are accepted
  but `XRMIMIC_CAPTURE` skips MSAA eyes.
- A controller moved by itself keeps its pose until R; the gamepad does not move it.
- `XRMIMIC_TEST_*` variables exist for the smoke check only.

## Layout

- `crates/xrmimic-core`: platform-neutral math, device profile, frame pacing,
  simulated user and binding paths.
- `crates/xrmimic-runtime`: the runtime DLL (OpenXR entry points, D3D11, window).
- `crates/xrmimic-launch`: the out-of-sandbox helper.
- `crates/xrmimic-minimal`: the smallest runtime that only advertises itself.

## License

MIT OR Apache-2.0, at your option.
