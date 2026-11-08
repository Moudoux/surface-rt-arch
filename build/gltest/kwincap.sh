#!/bin/sh
# Capture KWin's own GLSL on the Surface. The installed kwin_wayland has
# cap_sys_nice, so Mesa ignores MESA_SHADER_DUMP_PATH for it (secure_getenv).
# A copy in /tmp/bin (same basename, or it aborts) has no capabilities; it runs
# nested with the virtual backend, a private D-Bus and all XDG dirs in /tmp
# (nothing is written to the SD card), alongside the real session.
#   build/gltest/kwincap.sh [-p]      (-p: use the unpacked Mesa in /tmp/gt/m)
# Shaders land in build/gltest/kwin/, the log (GRATE messages) is printed;
# KWin's own messages go to the journal (journalctl --user --since -2min).
set -eu
HOST=${SURFACE:-alarm@surface-rt}
HERE=$(cd "$(dirname "$0")" && pwd)
UNPACKED=0; [ "${1:-}" = -p ] && UNPACKED=1
ssh "$HOST" "UNPACKED=$UNPACKED sh -s" <<'REMOTE'
set -e
mkdir -p /tmp/bin /tmp/kwx/cfg /tmp/kwx/data /tmp/kwx/cache /tmp/kwx/state
cp /usr/bin/kwin_wayland /tmp/bin/kwin_wayland
rm -rf /tmp/kwshaders; mkdir -p /tmp/kwshaders
extra=
[ "$UNPACKED" = 1 ] && extra="LD_LIBRARY_PATH=/tmp/gt/m/usr/lib __EGL_VENDOR_LIBRARY_FILENAMES=/tmp/gt/m/usr/share/glvnd/egl_vendor.d/50_mesa.json"
env -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR=/run/user/1000 XDG_CONFIG_HOME=/tmp/kwx/cfg \
  XDG_DATA_HOME=/tmp/kwx/data XDG_CACHE_HOME=/tmp/kwx/cache XDG_STATE_HOME=/tmp/kwx/state \
  MESA_SHADER_DUMP_PATH=/tmp/kwshaders $extra \
  timeout 60 dbus-run-session -- /tmp/bin/kwin_wayland --virtual --socket wayland-kwt --no-lockscreen \
  --width 1366 --height 768 --exit-with-session "sleep 15" > /tmp/kwx/log 2>&1 || echo "kwin exit $?"
grep -E "GRATE" /tmp/kwx/log | sort | uniq -c || true
REMOTE
mkdir -p "$HERE/kwin"
scp -q "$HOST:/tmp/kwshaders/*" "$HERE/kwin/"
ls "$HERE/kwin"
