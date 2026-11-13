#!/bin/sh
# Back to SDDM (run as root: sudo sh /tmp/pkgs/rtshell-back-to-sddm.sh [now]).
# Re-enables SDDM at boot and disables greetd. With "now" it also stops greetd and starts SDDM at once
# (this ends a session started from the rtshell login screen). The installed files stay (harmless); the
# stock greetd config is restored.
systemctl disable greetd
systemctl enable sddm
[ -f /etc/greetd/config.toml.orig ] && cp -p /etc/greetd/config.toml.orig /etc/greetd/config.toml
if [ "$1" = now ]; then
  systemctl stop greetd
  systemctl start sddm
fi
echo "--- at next boot: sddm=$(systemctl is-enabled sddm) greetd=$(systemctl is-enabled greetd)"
echo "running: sddm=$(systemctl is-active sddm) greetd=$(systemctl is-active greetd)"
