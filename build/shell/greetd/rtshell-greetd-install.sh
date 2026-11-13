#!/bin/sh
# Step 1 of 2 (run as root: sudo sh /tmp/pkgs/rtshell-greetd-install.sh). Installs rtshell for greetd:
#   /usr/local/bin/rtshell, /usr/local/bin/rtshell-session  (the greeter cannot read /home/alarm)
#   /etc/rtshell/config.toml                                (the greeter's config)
#   /etc/greetd/config.toml                                 (stock file kept as config.toml.orig)
# Changes NO services: SDDM stays the display manager. Try greetd by hand afterwards:
#   sudo systemctl stop sddm; sudo systemctl start greetd
# (a reboot, or `sudo systemctl stop greetd; sudo systemctl start sddm`, goes back to SDDM).
set -e
D=/tmp/pkgs
for f in rtshell rtshell-session rtshell-greeter.toml greetd-config.toml; do
  [ -f "$D/$f" ] || { echo "missing $D/$f"; exit 1; }
done
install -m 0755 -o root -g root "$D/rtshell" /usr/local/bin/rtshell
install -m 0755 -o root -g root "$D/rtshell-session" /usr/local/bin/rtshell-session
install -d -m 0755 -o root -g root /etc/rtshell
install -m 0644 -o root -g root "$D/rtshell-greeter.toml" /etc/rtshell/config.toml
[ -f /etc/greetd/config.toml.orig ] || cp -p /etc/greetd/config.toml /etc/greetd/config.toml.orig
install -m 0644 -o root -g root "$D/greetd-config.toml" /etc/greetd/config.toml
echo "--- installed:"
ls -l /usr/local/bin/rtshell /usr/local/bin/rtshell-session /etc/rtshell/config.toml /etc/greetd/config.toml /etc/greetd/config.toml.orig
sha256sum /usr/local/bin/rtshell "$D/rtshell"
echo "--- services (unchanged): sddm=$(systemctl is-enabled sddm) greetd=$(systemctl is-enabled greetd)"
