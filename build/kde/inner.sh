#!/bin/bash
# Runs inside the armv7h container as (container) root; makepkg itself runs as alarm.
# sudo cannot work under qemu-user (setuid ignored), so deps are installed here with pacman directly.
set -euo pipefail
n=$1
rm -rf /tmp/b && cp -r /pkgbuilds/$n /tmp/b && cd /tmp/b
# armv7h + ALARM naming fixes (ALARM's builds lack these provides)
sed -i -e 's/^arch=(x86_64)/arch=(armv7h)/' \
       -e 's/\bemoji-font\b/noto-fonts-emoji/' \
       -e 's/^\(\s*\)sdl2\(\s*\(#.*\)\?\)$/\1sdl2-compat\2/' PKGBUILD
# kaccounts-integration is uninstallable on ALARM armv7h (signon-ui needs qt5-webengine): drop it; KAccounts is optional in KDE CMake
sed -i -e "/^\s*'kaccounts-integration:/d" -e '/^\s*kaccounts-integration\s*\(#.*\)\?$/d' -e 's/\bkaccounts-integration\b//' PKGBUILD
[ -f /pkg/fixups/$n.sh ] && . /pkg/fixups/$n.sh
chown -R alarm:alarm /tmp/b
deps=$(runuser -u alarm -- makepkg --printsrcinfo | sed -n 's/^\s*\(make\)\?depends = //p' | sed 's/[<>=].*//' | sort -u)
# Already-built local packages first (e.g. purpose), then the rest from the repos
local=()
for f in /pkg/pkg/*.pkg.tar.*; do
  [ -e "$f" ] || continue
  pn=$(bsdtar -xOf "$f" .PKGINFO | sed -n 's/^pkgname = //p')
  grep -qx "$pn" <<<"$deps" && { local+=("$f"); deps=$(grep -vx "$pn" <<<"$deps"); }
done
[ ${#local[@]} -gt 0 ] && pacman -U --noconfirm --needed "${local[@]}"
pacman -S --noconfirm --needed --asdeps $deps
# PGP: import the release keys Arch ships with the PKGBUILD
[ -d keys/pgp ] && runuser -u alarm -- gpg -q --import keys/pgp/*.asc || true
grep -n '^arch=' PKGBUILD
# ccache: same /tmp/b path every run, so hashes match without CCACHE_BASEDIR. Stats at the end of the log.
runuser -u alarm -- env MAKEFLAGS=-j24 CMAKE_BUILD_PARALLEL_LEVEL=24 PKGDEST=/pkg/pkg SRCDEST=/srcdest \
  CCACHE_DIR=/ccache CCACHE_MAXSIZE=20G makepkg -f --noconfirm --nocheck
runuser -u alarm -- env CCACHE_DIR=/ccache ccache -s || true
