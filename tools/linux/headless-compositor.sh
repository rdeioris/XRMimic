#!/bin/sh
# A headless Wayland compositor (GNOME Shell, no root) for running Chrome's
# WebXR on a machine without a desktop session. Prints GNOME Shell's PID.
#
#   tools/linux/headless-compositor.sh            start (display "xrmimic-wl")
#   kill <pid>                                    stop (GNOME Shell itself; its
#                                                 dbus-run-session wrapper then exits)
#
# Then: export XDG_RUNTIME_DIR=/run/user/$(id -u) WAYLAND_DISPLAY=xrmimic-wl
set -eu
export XDG_RUNTIME_DIR=${XDG_RUNTIME_DIR:-/run/user/$(id -u)}
if [ -S "$XDG_RUNTIME_DIR/xrmimic-wl" ]; then
    echo "already running: $XDG_RUNTIME_DIR/xrmimic-wl" >&2
    exit 0
fi
nohup dbus-run-session -- gnome-shell --headless --wayland --no-x11 \
    --virtual-monitor 1920x1080 --wayland-display=xrmimic-wl \
    > "${TMPDIR:-/tmp}/xrmimic-compositor.log" 2>&1 &
for _ in 1 2 3 4 5 6 7 8 9 10; do
    [ -S "$XDG_RUNTIME_DIR/xrmimic-wl" ] && break
    sleep 1
done
[ -S "$XDG_RUNTIME_DIR/xrmimic-wl" ] || { echo "compositor did not start; see ${TMPDIR:-/tmp}/xrmimic-compositor.log" >&2; exit 1; }
pgrep -n -u "$(id -u)" -f "^gnome-shell --headless .*--wayland-display=xrmimic-wl"
