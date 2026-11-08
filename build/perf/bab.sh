#!/bin/sh
# bab.sh qml secs val...: GRATE_BATCH_DRAWS A/B on a qml6 test app (extra env: BABENV="A=1 B=2")
q=$1; secs=$2; shift 2
hz=$(getconf CLK_TCK)
for v in "$@"; do
  log=/tmp/perf/bab-$v.log
  since=$(date '+%H:%M:%S')
  sh /tmp/runqml.sh "$q" "$log" QSG_DISTANCEFIELD_ANTIALIASING=gray GRATE_PERF=4 GRATE_BATCH_DRAWS=$v $BABENV
  sleep 4
  pid=; for p in $(pgrep -x qml6); do case "$(tr '\0' ' ' </proc/$p/cmdline)" in *"$q"*) pid=$p;; esac; done
  c0=$(awk '{print $14+$15}' /proc/$pid/stat); k0=$(awk '{print $14+$15}' /proc/$(pgrep -x kwin_wayland)/stat)
  sleep $((secs-4))
  c1=$(awk '{print $14+$15}' /proc/$pid/stat); k1=$(awk '{print $14+$15}' /proc/$(pgrep -x kwin_wayland)/stat)
  kill $pid; sleep 2
  echo "== GRATE_BATCH_DRAWS=$v $BABENV  qml6 cpu $(( (c1-c0)*100/hz/(secs-4) ))%  kwin cpu $(( (k1-k0)*100/hz/(secs-4) ))%"
  echo "app: $(journalctl --user --since "$since" --no-pager -o cat | grep -o 'ICONFPS [0-9.]*' | awk '{print $2}' | tail -n +2 | tr '\n' ' ')"
  grep 'GRATE PERF qml6' $log | tail -n +2 | sed -E 's/.*flushes \(([0-9.]+)\/s\).*draw cpu ([0-9.]+)ms submit ([0-9.]+)ms\(([0-9]+)\) drain ([0-9.]+)ms.*/flush\/s \1 draw \2 submit \3 ms(\4) drain \5 ms/'
  echo "kwin: $(journalctl --user --since "$since" --no-pager -o cat | grep -o '^FPS [0-9.]*' | awk '{print $2}' | tr '\n' ' ')"
  echo "errors: $(grep -c 'failed' $log)"
done
