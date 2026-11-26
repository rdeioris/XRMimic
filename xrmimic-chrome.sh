#!/bin/sh
# Start Chrome (157+) on Linux with xrmimic as its OpenXR runtime, on its own profile.
#
#   ./xrmimic-chrome.sh [url]
#
# Without a url it opens the XRMimic test page (tools/testpage), served on
# http://localhost:8771/ by xrmimic-launch until Chrome exits.
#
# Environment:
#   CHROME        Chrome binary (default: google-chrome, else ~/xrmimic-chrome/root/opt/google/chrome/chrome)
#   XRMIMIC_DEBUG_PORT  DevTools port to open (default 9333) so a script can drive the page
#
# Chrome needs a Wayland compositor for WebXR frames to reach the runtime (its
# GPU process cannot share them under --headless). Without a desktop session,
# start one first: tools/linux/headless-compositor.sh, then
#   export XDG_RUNTIME_DIR=/run/user/$(id -u) WAYLAND_DISPLAY=xrmimic-wl
set -eu
here=$(cd "$(dirname "$0")" && pwd)
url=${1:-}
so="$here/target/release/libxrmimic_runtime.so"
[ -f "$so" ] || { echo "not built: cargo build --release -p xrmimic-runtime" >&2; exit 1; }
chrome=${CHROME:-$(command -v google-chrome || echo "$HOME/xrmimic-chrome/root/opt/google/chrome/chrome")}

# Chrome keeps the runtime loaded while it runs: give it its own copy.
base=${XDG_DATA_HOME:-$HOME/.local/share}/xrmimic
rm -rf "$base/runtime"
dir="$base/runtime/$(date +%Y%m%d-%H%M%S)"
mkdir -p "$dir"
cp "$so" "$dir/"
cat > "$dir/xrmimic.json" <<'EOF'
{
    "file_format_version": "1.0.0",
    "runtime": { "name": "XRMimic", "library_path": "./libxrmimic_runtime.so" }
}
EOF

export XR_RUNTIME_JSON="$dir/xrmimic.json"
export XRMIMIC_LOG="$dir/xrmimic.log"
# Device settings (preset=..., name=..., see README), kept between launches.
export XRMIMIC_DEVICE_FILE="${XRMIMIC_DEVICE_FILE:-$base/device.ini}"
platform=--headless=new
[ -n "${WAYLAND_DISPLAY:-}" ] && platform=--ozone-platform=wayland
echo "runtime: $XR_RUNTIME_JSON"
echo "log:     $XRMIMIC_LOG"
server=
if [ -z "$url" ]; then
    url=http://localhost:8771/
    if ! curl -s -o /dev/null http://127.0.0.1:8771/ 2>/dev/null; then
        "$here/target/release/xrmimic-launch" serve "$here/tools/testpage" 8771 >/dev/null 2>&1 &
        server=$!
        sleep 0.3
    fi
fi
echo "page:    $url"
# --no-sandbox: a Chrome unpacked without root has no working sandbox helper.
"$chrome" --user-data-dir="$base/chrome-profile" --no-first-run --no-default-browser-check \
    --enable-features=OpenXR --use-angle=vulkan "$platform" --no-sandbox \
    --remote-debugging-port="${XRMIMIC_DEBUG_PORT:-9333}" "$url" || true
[ -n "$server" ] && kill "$server" 2>/dev/null
exit 0
