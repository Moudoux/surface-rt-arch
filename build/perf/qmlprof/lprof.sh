#!/bin/sh
# lprof.sh [unit] [extra perf record args]: perf profiles of plasmashell for one launcher open (open.data) and close (close.data)
export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
export LD_LIBRARY_PATH=/tmp/pf/root/usr/lib
P=/tmp/pf/root/usr/bin/perf
U=${1:-ps-perf}; shift; pid=$(systemctl --user show -p MainPID --value $U)
cd /tmp/pf
for w in open close; do
  $P record -q -e cpu-clock:u -F ${F:-2000} "$@" -p $pid -o $w.data -- sleep 3 &
  sleep 0.3
  qdbus6 org.kde.plasmashell /PlasmaShell org.kde.PlasmaShell.activateLauncherMenu >/dev/null
  wait
done
