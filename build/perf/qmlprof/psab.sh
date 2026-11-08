#!/bin/sh
# psab.sh label [systemd-run -E args...]: restart ps-perf plasmashell with the given env, wait, run lcost.sh 4
export XDG_RUNTIME_DIR=/run/user/1000 DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus
l=$1; shift
systemctl --user stop ps-perf 2>/dev/null; sleep 2
systemd-run -q --user --unit=ps-perf "$@" /usr/bin/plasmashell; sleep 25
echo "== $l"; sh /tmp/perf/lcost.sh 4
