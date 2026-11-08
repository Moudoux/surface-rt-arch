#!/bin/sh
# nrun.sh label [ENV=val...]: nested.sh run (MOVESECS=20) + per-second CPU of the nested kwin and its threads.
# Output: /tmp/perf/<label>.cpu (time, % of a core, per-thread ticks), <label>.log (KWin stderr), <label>.fps
l=$1; shift
sh /tmp/perf/nested.sh MOVESECS=20 "$@" &
sleep 2
K=$(pgrep -f "^/tmp/bin/kwin_wayland" | head -1)
hz=$(getconf CLK_TCK); prev=0
: > /tmp/perf/$l.cpu
while kill -0 $K 2>/dev/null; do
  t=$(awk '{print $14+$15}' /proc/$K/stat 2>/dev/null) || break
  thr=$(for d in /proc/$K/task/*; do awk -v c="$(cat $d/comm)" '{printf "%s=%d ", c, $14+$15}' $d/stat; done 2>/dev/null)
  echo "$(date +%s) $(( (t-prev)*100/hz ))% $thr" >> /tmp/perf/$l.cpu; prev=$t; sleep 1
done
wait
cp /tmp/kwx/log /tmp/perf/$l.log; cp /tmp/perf/n-fps.log /tmp/perf/$l.fps
