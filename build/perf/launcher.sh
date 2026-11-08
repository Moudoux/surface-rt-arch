#!/bin/sh
# launcher.sh [cycles]: open/close the Kickoff launcher (D-Bus toggle) N times
# (3 s open, 3 s closed) and summarise plasmashell's rendering. plasmashell
# must run as the transient unit ps-perf with GRATE_PERF=1 QSG_RENDER_TIMING=1.
export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
N=${1:-3}
pid=$(systemctl --user show -p MainPID --value ps-perf)
[ "$pid" -gt 0 ] 2>/dev/null || { echo "ps-perf not running"; exit 1; }
ticks() { awk '{print $14 + $15}' /proc/$1/stat; }
since=$(date '+%Y-%m-%d %H:%M:%S')
t0=$(ticks "$pid"); s0=$(date +%s.%N)
i=0
while [ $i -lt "$N" ]; do
  qdbus6 org.kde.plasmashell /PlasmaShell org.kde.PlasmaShell.activateLauncherMenu >/dev/null
  sleep 3
  qdbus6 org.kde.plasmashell /PlasmaShell org.kde.PlasmaShell.activateLauncherMenu >/dev/null
  sleep 3
  i=$((i + 1))
done
t1=$(ticks "$pid"); s1=$(date +%s.%N)
hz=$(getconf CLK_TCK)
echo "plasmashell cpu: $(echo "$t0 $t1 $s0 $s1 $hz" | awk '{printf "%.0f%% of a core over %.0f s", 100*($2-$1)/$5/($4-$3), $4-$3}')"
J=$(journalctl --user -u ps-perf -q -o cat --since "$since")
echo "$J" | grep "time in renderer" | sed 's/.*total=\([0-9]*\)ms, preprocess=\([0-9]*\), updates=\([0-9]*\), rendering=\([0-9]*\).*/\1 \2 \3 \4/' |
  awk '{n++; t+=$1; p+=$2; u+=$3; r+=$4; if ($1>m) m=$1; if ($1>=17) slow++}
       END {if (n) printf "frames %d: mean total %.1f ms (preprocess %.1f updates %.1f render %.1f), max %d, >=17 ms: %d\n", n, t/n, p/n, u/n, r/n, m, slow}'
echo "$J" | grep -E "time in renderer|Frame rendered|Frame prepared" | awk '{print}' | tail -0
echo "$J" | grep "GRATE PERF plasmashell" | sed 's/.*per second: //' | head -20
echo "kwin FPS (fps.qml): $(journalctl --user -q -o cat --since "$since" | grep "FPS" | sed 's/.*FPS //' | tr '\n' ' ')"
