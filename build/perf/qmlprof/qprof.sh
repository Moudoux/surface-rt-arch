#!/bin/sh
# qprof.sh out.qtd: qmlprofiler trace of plasmashell (debug server on :3768) for one launcher toggle (3 s)
export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
{ sleep 2; echo r; sleep 1
  qdbus6 org.kde.plasmashell /PlasmaShell org.kde.PlasmaShell.activateLauncherMenu >/dev/null
  sleep 3; echo r; sleep 5; echo f; sleep 4; echo q; echo y; sleep 1; } |
  /usr/lib/qt6/bin/qmlprofiler --interactive --record off -a 127.0.0.1 -p 3768 -o "$1" 2>&1 | tail -5
ls -la "$1"
