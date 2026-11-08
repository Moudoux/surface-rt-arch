#!/bin/bash
# Install the test packages from /tmp/pkgs (if not installed yet) and start the
# Plasma test session with shader dumping. Run as alarm on the Surface's own
# console (not over SSH). Asks for the sudo password only for pacman.
#   bash /tmp/pkgs/test-grate.sh
set -e
[ "$(id -u)" != 0 ] || { echo "Run as alarm, not root"; exit 1; }
KWIN=/tmp/pkgs/kwin-6.8.2-2.2-armv7h.pkg.tar.xz
MESA=/tmp/pkgs/mesa-grate-26.2.3-25-armv7h.pkg.tar.xz

want=()
[ "$(pacman -Q kwin 2>/dev/null)" = "kwin 6.8.2-2.2" ] || want+=("$KWIN")
[ "$(pacman -Q mesa-grate 2>/dev/null)" = "mesa-grate 26.2.3-25" ] || want+=("$MESA")
if [ ${#want[@]} -gt 0 ]; then
  for f in "${want[@]}"; do [ -e "$f" ] || { echo "Missing $f (reboot cleared /tmp?)"; exit 1; }; done
  echo "Installing: ${want[*]##*/}"
  sudo pacman -U --noconfirm "${want[@]}"
fi
pacman -Q kwin mesa-grate

mkdir -p /tmp/shaders
export MESA_SHADER_DUMP_PATH=/tmp/shaders NO_GL_OVERRIDE=1
# Qt's subpixel (LCD) distance-field text shader is too long for grate
# (260 EXECs > 63): grayscale antialiasing uses the short one.
export QSG_DISTANCEFIELD_ANTIALIASING=gray
exec ~/kde/start-plasma-test.sh
