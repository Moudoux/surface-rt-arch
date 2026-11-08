#!/bin/bash
# measure.sh label seconds: CPU (total, kwin, plasmashell), journal growth and cpu0 frequency while something animates
K=$(pgrep -x kwin_wayland); P=$(pgrep -x plasmashell)
j0=$(journalctl --user -b -q | wc -l); s0=$(journalctl -b -q | wc -l)
read -r _ a b c d e f g h _ < /proc/stat; t0=$((a+b+c+d+e+f+g+h)); i0=$((d+e))
kt0=$(awk '{print $14+$15}' /proc/$K/stat); pt0=$(awk '{print $14+$15}' /proc/$P/stat)
fr=""; for i in $(seq $2); do fr="$fr $(cat /sys/devices/system/cpu/cpu0/cpufreq/scaling_cur_freq)"; sleep 1; done
read -r _ a b c d e f g h _ < /proc/stat; t1=$((a+b+c+d+e+f+g+h)); i1=$((d+e))
kt1=$(awk '{print $14+$15}' /proc/$K/stat); pt1=$(awk '{print $14+$15}' /proc/$P/stat)
j1=$(journalctl --user -b -q | wc -l); s1=$(journalctl -b -q | wc -l)
hz=$(getconf CLK_TCK)
echo "== $1 ($2 s): total busy $(( 100*((t1-t0)-(i1-i0))/(t1-t0) ))% of 4 cores; kwin $(( 100*(kt1-kt0)/hz/$2 ))% plasmashell $(( 100*(pt1-pt0)/hz/$2 ))% (of one core); journal user +$((j1-j0)) system +$((s1-s0)); cpu0 kHz:$fr"
