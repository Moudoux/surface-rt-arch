#!/bin/sh
# Host side: screenshot of what rtshell is compositing on the Surface (SIGUSR1, render/screenshot.rs),
# copied to the host.   build/shell/shot.sh [OUT.png]     (default: $OUT/rtshell-shot.png, OUT=/tmp)
# The shot is an offscreen re-render of the current elements with full damage (same imports and shaders as
# the real frame; it does not see KMS planes). It is written on the Surface as /tmp/rtshell-shot-<n>.png.
set -eu
HOST=${SURFACE:-alarm@surface-rt}
DEST=${1:-${OUT:-/tmp}/rtshell-shot.png}
P=$(ssh "$HOST" 'L=$(ls -t ~/rtshell-*.log | head -1)
  n=$(grep "screenshot path=" "$L" | grep -vc -- "-buf[0-9]*\.png" || true)
  pkill -USR1 -x rtshell || { echo "rtshell not running" >&2; exit 1; }
  for i in $(seq 50); do
    m=$(grep -e "screenshot path=" -e "screenshot failed" -e "screenshot write failed" -e "screenshot: no active" "$L" | grep -vc -- "-buf[0-9]*\.png" || true)
    [ "$m" -gt "$n" ] && break; sleep 0.1
  done
  # Only the composite line ("screenshot path=... w= h="), not the per-buffer dumps (-buf<k>.png).
  line=$(grep -e "screenshot path=" -e "screenshot failed" -e "screenshot write failed" -e "screenshot: no active" "$L" | grep -v -- "-buf[0-9]*\.png" | tail -1); echo "$line" >&2
  echo "$line" | sed -n "s/.*path=\([^ ]*\).*/\1/p"')
[ -n "$P" ] || exit 1
scp -q "$HOST:$P" "$DEST"
echo "$DEST"
