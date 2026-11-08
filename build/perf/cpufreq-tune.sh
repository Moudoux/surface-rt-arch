#!/bin/sh
# cpufreq-tune.sh [up_threshold] [sampling_down_factor]: run as root (sudo sh cpufreq-tune.sh).
# ondemand with a low up_threshold, so KWin's frame work runs at full clock; idle stays low.
# Runtime only: a reboot restores the defaults (ondemand, 95, 1).
set -e
UP=${1:-25}
DOWN=${2:-10}
echo ondemand > /sys/devices/system/cpu/cpufreq/policy0/scaling_governor
echo "$UP" > /sys/devices/system/cpu/cpufreq/ondemand/up_threshold
echo "$DOWN" > /sys/devices/system/cpu/cpufreq/ondemand/sampling_down_factor
sysctl -q kernel.sched_util_clamp_min_rt_default=1024
echo "governor $(cat /sys/devices/system/cpu/cpufreq/policy0/scaling_governor)," \
     "up_threshold $(cat /sys/devices/system/cpu/cpufreq/ondemand/up_threshold)," \
     "sampling_down_factor $(cat /sys/devices/system/cpu/cpufreq/ondemand/sampling_down_factor)," \
     "rt uclamp default $(cat /proc/sys/kernel/sched_util_clamp_min_rt_default)"
