#!/bin/sh
# Copy the performance tools to the Surface's /tmp/perf (RAM) and make the move-script variants.
set -e
HOST=${SURFACE:-alarm@surface-rt}
HERE=$(cd "$(dirname "$0")" && pwd)
ssh "$HOST" 'mkdir -p /tmp/perf'
scp -q "$HERE"/*.qml "$HERE"/*.sh "$HOST":/tmp/perf/
ssh "$HOST" 'cd /tmp/perf && chmod +x *.sh &&
  mk() { mkdir -p $1 && sed "s/CAPTION/$2/; s/X0/$3/; s/RX/$4/; s/Y0/$5/; s/RY/$6/; s/W, H/$7, $8/" move.qml > $1/Main.qml; }
  mk big Dolphin 180 150 40 50 1000 560
  mk small Dolphin 300 250 150 100 360 240
  mk bigs staticwin 180 150 40 50 1000 560'
