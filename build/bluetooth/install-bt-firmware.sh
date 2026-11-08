#!/bin/sh
# Install the Marvell 88W8797 combo firmware 14.62.17.p215 (Wi-Fi + Bluetooth over UART) on the Surface RT.
# Run as root on the Surface:  sudo sh install-bt-firmware.sh /path/to/sd8797_uapsta.bin
# The firmware is NOT in this repository (it is Microsoft/Marvell code, not redistributable). Extract it
# yourself from your own Windows RT 8.1 driver with
# sources/surface-rt-patches/tools/bluetooth/extract-windows-uart-firmware.py (see docs/BLUETOOTH.md).
# Changes:
#   /usr/lib/firmware/updates/mrvl/sd8797_uapsta.bin    (takes precedence over linux-firmware-marvell's p59)
#   /etc/modprobe.d/surface-rt-bluetooth.conf           (mwifiex_sdio force_legacy_scan=1, needed with p215)
#   enables surface-rt-bluetooth.service (from surface-rt-tools) if it is installed
# Undo: rm both files, systemctl disable surface-rt-bluetooth, reboot.
set -eu
FW=${1:?usage: $0 /path/to/sd8797_uapsta.bin}
WANT=97cc4df3d978db1485c3531c13749cc91d12eae7570c38edeabfd2dd9e6d8b74
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }
have=$(sha256sum "$FW" | cut -d' ' -f1)
[ "$have" = "$WANT" ] || { echo "$FW: sha256 $have, expected $WANT (14.62.17.p215 BT_UART). Refusing."; exit 1; }
install -D -m 644 "$FW" /usr/lib/firmware/updates/mrvl/sd8797_uapsta.bin
cat > /etc/modprobe.d/surface-rt-bluetooth.conf <<'EOT'
# Surface RT: the 14.62.17.p215 combo firmware needs legacy scan (mwifiex patch from surface-rt-patches 0027)
options mwifiex_sdio force_legacy_scan=1
EOT
if systemctl cat surface-rt-bluetooth.service >/dev/null 2>&1; then
  systemctl enable surface-rt-bluetooth.service
else
  echo "note: surface-rt-tools is not installed yet; enable surface-rt-bluetooth.service after installing it"
fi
sync
echo "Installed. Reboot; then check: journalctl -k | grep -i 'p215\|FW download'; systemctl status surface-rt-bluetooth"
