#!/bin/sh
# Runs on the Surface. Start a compositor from a text VT (Ctrl+Alt+F2, log in as alarm):
#   ~/rtshell/run.sh [-t SECONDS] [-p] [-d] [binary [args...]]    default: rtshell (or anvil --tty-udev)
#   -t N  kill it after N seconds (guard for new GPU paths; 0 = no limit, the default)
#   -p    stop the Plasma session first (frees RAM and the GPU)
#   -d    debug log for Smithay's DRM compositor (plane/cursor decisions)
# Recovery: Ctrl+Alt+F<n> back to this VT; rtshell: Ctrl+Alt+Backspace quits; over SSH: pkill -x rtshell
# (SIGTERM = clean exit) or pkill -x anvil.
cd "$(dirname "$0")"
LIMIT=0
while getopts t:pd o; do case $o in t) LIMIT=$OPTARG;; p) STOP_PLASMA=1;; d) RUST_LOG=info,smithay::backend::drm=debug;; *) exit 2;; esac; done
shift $((OPTIND - 1))
if [ $# -eq 0 ]; then
  if [ -x ./rtshell ]; then set -- rtshell; else set -- anvil --tty-udev; fi
fi
BIN=$1; shift

if [ -n "$STOP_PLASMA" ] && pgrep -u "$USER" -x kwin_wayland >/dev/null; then
  echo "logging out of Plasma..."
  # Clean logout via ksmserver (apps get to save). Note: pkill -x startplasma-wayland does NOT work,
  # comm is cut to 15 chars ("startplasma-way"); and pkill -f would match this script (notes/03 Test 5).
  busctl --user call org.kde.Shutdown /Shutdown org.kde.Shutdown logout || true
  for i in $(seq 30); do pgrep -u "$USER" -x kwin_wayland >/dev/null || break; sleep 0.5; done
  if pgrep -u "$USER" -x kwin_wayland >/dev/null; then
    echo "logout timed out; terminating the Plasma login session"
    for s in $(loginctl list-sessions --no-legend | awk -v u="$USER" '$3 == u {print $1}'); do
      [ "$(loginctl show-session "$s" -p Desktop --value)" = KDE ] && loginctl terminate-session "$s"
    done
    for i in $(seq 20); do pgrep -u "$USER" -x kwin_wayland >/dev/null || break; sleep 0.5; done
  fi
  pgrep -u "$USER" -x kwin_wayland >/dev/null && echo "warning: kwin_wayland still running" || echo "Plasma stopped"
  # After a logout SDDM starts its greeter and jumps to VT 1, which takes the display (DRM) away from us
  # (2026-11-09: 13 s of EACCES page flips). Wait for the greeter, then make this session active again.
  for i in $(seq 20); do
    loginctl list-sessions --no-legend | awk '$3 == "sddm"' | grep -q . && break; sleep 0.5
  done
  sleep 2
  loginctl activate "$XDG_SESSION_ID" && echo "re-activated session $XDG_SESSION_ID"
  sleep 1
fi

n=1; while [ -e "$HOME/rtshell-$n.log" ]; do n=$((n + 1)); done
LOG="$HOME/rtshell-$n.log"
echo "log: $LOG"
{
  echo "== $(date -Is) $BIN $* (limit ${LIMIT}s) on $(tty) XDG_SESSION_ID=$XDG_SESSION_ID"
  uname -r; free -m | sed -n 2p
} > "$LOG"
export RUST_BACKTRACE=1 RUST_LOG=${RUST_LOG:-info}
unset WAYLAND_DISPLAY DISPLAY
if [ "$LIMIT" -gt 0 ]; then
  timeout -k 3 "$LIMIT" "./$BIN" "$@" >> "$LOG" 2>&1
else
  "./$BIN" "$@" >> "$LOG" 2>&1
fi
rc=$?
echo "== exit $rc at $(date -Is)" >> "$LOG"
echo "exit $rc, log: $LOG"
