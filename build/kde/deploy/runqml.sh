#!/bin/sh
# runqml.sh file.qml log [ENV=val ...]: start qml6 in the running Plasma session
f=$1; log=$2; shift 2
export XDG_RUNTIME_DIR=/run/user/1000 WAYLAND_DISPLAY=wayland-1 QT_QPA_PLATFORM=wayland DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
env "$@" nohup qml6 "$f" > "$log" 2>&1 &
