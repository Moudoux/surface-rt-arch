#!/bin/sh
# lcost.sh [cycles] [unit]: plasmashell CPU ms per launcher open and per close (3 s each), plus 3 s idle baseline
export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
N=${1:-5}; U=${2:-ps-perf}
pid=$(systemctl --user show -p MainPID --value $U)
hz=$(getconf CLK_TCK)
t() { awk '{print $14 + $15}' /proc/$pid/stat; }
ms() { echo $(( ($2 - $1) * 1000 / hz )); }
a=$(t); sleep 3; b=$(t); echo "idle 3s: $(ms $a $b) ms"
i=0
while [ $i -lt $N ]; do
  a=$(t); qdbus6 org.kde.plasmashell /PlasmaShell org.kde.PlasmaShell.activateLauncherMenu >/dev/null; sleep 3; b=$(t)
  qdbus6 org.kde.plasmashell /PlasmaShell org.kde.PlasmaShell.activateLauncherMenu >/dev/null; sleep 3; c=$(t)
  echo "cycle $i: open $(ms $a $b) ms, close $(ms $b $c) ms"
  i=$((i+1))
done
