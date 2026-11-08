#!/bin/sh
# Runs inside the nested KWin session (nested.sh): wallpaper window, big static window, fps counter,
# then moves the static window for MOVESECS (default 10) s. FPS lines land in /tmp/perf/n-fps.log.
export QT_QPA_PLATFORM=wayland QSG_DISTANCEFIELD_ANTIALIASING=gray QT_FORCE_STDERR_LOGGING=1
qml6 /tmp/perf/bg.qml > /tmp/perf/n-bg.log 2>&1 &
sleep 3
qml6 /tmp/perf/static.qml > /tmp/perf/n-static.log 2>&1 &
sleep 3
qml6 /tmp/perf/fps.qml > /tmp/perf/n-fps.log 2>&1 &
sleep 4
echo "== idle"; sleep 6
id=$(qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.loadDeclarativeScript ${MOVE:-/tmp/perf/bigs/Main.qml} nmove)
qdbus6 org.kde.KWin /Scripting/Script$id org.kde.kwin.Script.run
echo "== moving"; sleep ${MOVESECS:-10}
qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.unloadScript nmove
echo "== done"; sleep 1
