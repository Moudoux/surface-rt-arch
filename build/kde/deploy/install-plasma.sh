#!/bin/bash
# Install KDE Plasma 6.8.2 on the Surface RT (Arch Linux ARM armv7h + mesa-grate from [surface]).
# Run on the Surface after setup-surface-repo.sh:  sudo bash install-plasma.sh
# Installs Arch/ALARM repo packages plus our rebuilt/patched KDE packages; with [surface] listed first in
# pacman.conf, the names below resolve to our builds. Does NOT enable sddm (see docs/INSTALL.md).
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "Run with sudo: sudo bash $0"; exit 1; }

repo_pkgs=(pipewire-jack plasma-workspace konsole dolphin plasma-pa powerdevil kscreen breeze breeze-gtk
           systemsettings plasma-keyboard kde-gtk-config xdg-desktop-portal-kde bluedevil plasma-systemmonitor
           kinfocenter sddm noto-fonts-emoji sdl2-compat ttf-dejavu noto-fonts)
# ours (from [surface]); plasma-mobile is optional (tablet shell)
our_pkgs=(kwin kirigami plasma-desktop plasma-nm purpose spectacle plasma-welcome discover
          plasma-browser-integration flatpak-kcm)
# The repo's plasma-meta is 5.27 and would pull stale packages: never install it.

pacman-conf --repo-list | head -1 | grep -qx surface || { echo "[surface] is not the first repo: run setup-surface-repo.sh first"; exit 1; }
pacman -Q mesa-grate >/dev/null 2>&1 || { echo "mesa-grate is not installed: run 'pacman -S mesa-grate' first (stock mesa = no GPU)"; exit 1; }
for p in "${our_pkgs[@]}"; do
  pacman -Sl surface | awk '{print $2}' | grep -qx "$p" || { echo "$p is missing from [surface]: build it (build/kde/build.sh $p)"; exit 1; }
done
read -rp "Install ${#repo_pkgs[@]} repo packages (~370 with deps, ~330 MB download) and ${#our_pkgs[@]} [surface] packages? [y/N] " a
[[ $a == [yY]* ]] || exit 1

pacman -Syu --needed --noconfirm --disable-download-timeout "${repo_pkgs[@]}" "${our_pkgs[@]}"

echo; echo "== Check"
pacman -Q mesa-grate plasma-desktop plasma-nm plasma-workspace kwin kirigami
for p in kwin kirigami plasma-desktop; do pacman -Si "surface/$p" >/dev/null 2>&1 && echo "$p: from [surface]"; done
id alarm | grep -q '\bvideo\b' && id alarm | grep -q '\brender\b' && echo "alarm is in video+render" || echo "WARNING: alarm not in video/render"
systemctl is-enabled sddm 2>/dev/null | grep -q enabled && echo "note: sddm is enabled" || echo "sddm installed, not enabled yet"
echo; echo "Done. First session: see docs/INSTALL.md (start-plasma-test.sh from a text console, then enable sddm)."
