#!/bin/sh
# nested.sh [ENV=val ...]: nested KWin (copy without file caps, --virtual 1366x768, private D-Bus,
# config/state in /tmp/kwx: nothing on the SD card) running scenario.sh. KWin stderr: /tmp/kwx/log.
# Unpacked Mesa: LD_LIBRARY_PATH=/tmp/gt/m/usr/lib __EGL_VENDOR_LIBRARY_FILENAMES=/tmp/gt/m/usr/share/glvnd/egl_vendor.d/50_mesa.json
mkdir -p /tmp/bin /tmp/kwx/cfg /tmp/kwx/data /tmp/kwx/cache /tmp/kwx/state
[ -x /tmp/bin/kwin_wayland ] || cp /usr/bin/kwin_wayland /tmp/bin/kwin_wayland
env -i HOME="$HOME" PATH="$PATH" XDG_RUNTIME_DIR=/run/user/1000 XDG_CONFIG_HOME=/tmp/kwx/cfg \
  XDG_DATA_HOME=/tmp/kwx/data XDG_CACHE_HOME=/tmp/kwx/cache XDG_STATE_HOME=/tmp/kwx/state "$@" \
  timeout 90 dbus-run-session -- /tmp/bin/kwin_wayland --virtual --socket wayland-kwt --no-lockscreen \
  --width 1366 --height 768 --exit-with-session /tmp/perf/scenario.sh > /tmp/kwx/log 2>&1 || echo "kwin exit $?"
