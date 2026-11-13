#!/bin/sh
# Step 2 of 2 (run as root: sudo sh /tmp/pkgs/rtshell-greetd-enable.sh), after the by-hand test worked:
# greetd + rtshell become the login screen at boot instead of SDDM. SDDM stays installed.
# Back: sudo sh /tmp/pkgs/rtshell-back-to-sddm.sh
set -e
[ -x /usr/local/bin/rtshell ] || { echo "run rtshell-greetd-install.sh first"; exit 1; }
systemctl disable sddm
systemctl enable greetd
echo "--- at next boot: sddm=$(systemctl is-enabled sddm) greetd=$(systemctl is-enabled greetd)"
echo "display-manager.service -> $(readlink /etc/systemd/system/display-manager.service)"
