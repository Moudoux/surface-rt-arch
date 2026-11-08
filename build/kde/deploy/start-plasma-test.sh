#!/bin/bash
# First KWin Wayland test on grate. Run as alarm (no sudo) from a text console on the Surface (Ctrl+Alt+F2..F6), Weston stopped.
# Log: ~/plasma-<n>.log (also check: journalctl --user -b). If it hangs: Ctrl+Alt+F2 for another console.
[ "$(id -u)" != 0 ] || { echo "Run as alarm, not root"; exit 1; }
[ -n "${SSH_CONNECTION:-}" ] && { echo "Run this on the Surface's own console, not over SSH"; exit 1; }
pgrep -x weston >/dev/null && { echo "Weston is running: stop it first"; exit 1; }
# A previous session may still be shutting down (systemd refuses with "TransactionIsDestructive"): wait for it.
for i in $(seq 60); do [ -z "$(systemctl --user list-jobs --no-legend 2>/dev/null)" ] && break; [ $i = 1 ] && echo "Waiting for the previous session to stop..."; sleep 1; done
systemctl --user reset-failed 2>/dev/null
n=1; while [ -e ~/plasma-$n.log ]; do n=$((n+1)); done
log=~/plasma-$n.log
echo "KWin output goes to the user journal: journalctl --user -b"
# KWin 6.8 requires GL_OES_texture_3D (only used for ICC profiles); grate lacks it. Mesa debug override until KWin is patched.
# Run with NO_GL_OVERRIDE=1 to test without it.
[ -z "${NO_GL_OVERRIDE:-}" ] && export MESA_EXTENSION_OVERRIDE="${MESA_EXTENSION_OVERRIDE:-+GL_OES_texture_3D}"
export QSG_INFO=1
export QT_LOGGING_RULES="kwin_*.debug=true;qt.scenegraph.general=true;qt.rhi.*=true"
{ echo "== $(date) $(uname -r)"; pacman -Q mesa-grate kwin plasma-workspace plasma-desktop qt6-base
  env | grep -E "^(MESA_|GRATE_|KWIN_|QSG_|QT_)" ; } > "$log"
echo "Logging to $log"
run=/usr/lib/plasma-dbus-run-session-if-needed; [ -x $run ] || run=dbus-run-session
$run startplasma-wayland >> "$log" 2>&1
echo "Plasma exited ($?). Log: $log"
