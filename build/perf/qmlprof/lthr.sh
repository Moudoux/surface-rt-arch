#!/bin/sh
# lthr.sh [unit]: per-thread plasmashell CPU ms for one launcher open and one close
export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
U=${1:-ps-perf}; pid=$(systemctl --user show -p MainPID --value $U)
snap() { for d in /proc/$pid/task/*; do printf "%s %s %s\n" "${d##*/}" "$(awk '{print $14+$15}' $d/stat)" "$(tr ' ' _ < $d/comm)"; done; }
diffs() { join "$1" "$2" | awk -v hz=$(getconf CLK_TCK) -v l="$3" '{d=($4-$2)*1000/hz; if (d>0) printf "  %s %-22s %5d ms\n", l, $3, d}'; }
snap | sort > /tmp/perf/s0
qdbus6 org.kde.plasmashell /PlasmaShell org.kde.PlasmaShell.activateLauncherMenu >/dev/null; sleep 3
snap | sort > /tmp/perf/s1
qdbus6 org.kde.plasmashell /PlasmaShell org.kde.PlasmaShell.activateLauncherMenu >/dev/null; sleep 3
snap | sort > /tmp/perf/s2
diffs /tmp/perf/s0 /tmp/perf/s1 open; diffs /tmp/perf/s1 /tmp/perf/s2 close
