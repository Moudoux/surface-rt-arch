#!/bin/sh
# realrun.sh label: in the real session, move Dolphin big then small (8 s each), print FPS + CPU
export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
for s in big small; do
  n=r$s$$
  id=$(qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.loadDeclarativeScript /tmp/perf/$s/Main.qml $n)
  qdbus6 org.kde.KWin /Scripting/Script$id org.kde.kwin.Script.run >/dev/null; sleep 3
  m=$(/tmp/perf/measure.sh $1-$s 8)
  echo "$m"; echo "   FPS: $(journalctl --user -b -q --since -8s -o cat | grep "FPS" | sed "s/.*FPS //" | tr "\n" " ")"
  qdbus6 org.kde.KWin /Scripting org.kde.kwin.Scripting.unloadScript $n >/dev/null; sleep 2
done
echo "   idle FPS: $(journalctl --user -b -q --since -2s -o cat | grep "FPS" | sed "s/.*FPS //" | tr "\n" " ")"
