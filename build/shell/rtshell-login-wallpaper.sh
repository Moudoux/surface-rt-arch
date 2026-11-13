#!/bin/sh
# Run as root, once: sudo sh rtshell-login-wallpaper.sh [user]
# Makes /var/lib/rtshell, owned by the desktop user (default alarm) and readable by everyone, so the user's
# rtshell session can leave its lock-screen image there for the login screen (greetd's user cannot read
# /home). Undo: rm -r /var/lib/rtshell
set -e
U=${1:-alarm}
install -d -o "$U" -g "$U" -m 755 /var/lib/rtshell
ls -ld /var/lib/rtshell
