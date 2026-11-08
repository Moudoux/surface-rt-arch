#!/bin/sh
# One-time setup of the local [surface] pacman repo on a freshly flashed Surface RT. Run as root:
#   sudo sh /tmp/pkgs/srt/setup-surface-repo.sh
# Expects the staged repo in /tmp/pkgs/srt/surface (build/repo/make-repo.sh on the PC, copied over; see
# docs/INSTALL.md). Changes, each with a backup:
#   1. /srv/surface-repo: our packages + surface.db + compat
#   2. /etc/pacman.conf: [surface] above [core]          (backup /etc/pacman.conf.pre-surface)
#   3. the kernel that is running now is kept as the extlinux "fallback" entry: /boot/zImage.old and
#      /boot/dtbs/tegra30-microsoft-surface-rt.dtb.fallback      (backup /boot/extlinux/extlinux.conf.pre-surface)
#   4. pacman -Sy; install surface-rt-tools (hooks) and linux-surface-rt (takes ownership of /boot/zImage,
#      /boot/dtbs/*.dtb and the kernel modules that the SD image shipped as plain files).
# Undo: cp /etc/pacman.conf.pre-surface /etc/pacman.conf; cp /boot/extlinux/extlinux.conf.pre-surface
#   /boot/extlinux/extlinux.conf; pacman -R surface-rt-tools (never remove linux-surface-rt: it owns zImage).
set -eu
SRC=${1:-/tmp/pkgs/srt/surface}
R=/srv/surface-repo
C=/etc/pacman.conf
X=/boot/extlinux/extlinux.conf
DTB=/boot/dtbs/tegra30-microsoft-surface-rt.dtb
ROOT=PARTUUID=5352543a-0000-4000-8000-000000000001
[ "$(id -u)" = 0 ] || { echo "run with sudo"; exit 1; }
[ -f "$SRC/surface.db.tar.gz" ] && [ -f "$SRC/compat" ] || { echo "missing $SRC (copy build/repo/surface first)"; exit 1; }
[ -f /boot/zImage ] && [ -f $DTB ] || { echo "/boot/zImage or $DTB missing: is this the Surface RT image?"; exit 1; }
pacman -Q linux-surface-rt >/dev/null 2>&1 && { echo "linux-surface-rt already installed: nothing to do"; exit 1; }

echo "== 1. $R"
mkdir -p $R
cp -a "$SRC/." $R/
chown -R root:root $R
chmod 755 $R; find $R -type f -exec chmod 644 {} +
ls -l $R

echo "== 2. $C"
[ -e $C.pre-surface ] || cp -p $C $C.pre-surface
if ! grep -q '^\[surface\]' $C; then
  sed -i '/^\[core\]$/i # Surface RT patched packages (kwin, kirigami, mesa-grate, kernel, ...); first, so it shadows Arch/ALARM.\n# See build/repo/REBUILD.md in the surface-rt-arch repository.\n[surface]\nSigLevel = Optional TrustAll\nServer = file:///srv/surface-repo\n' $C
fi
pacman-conf --repo-list

echo "== 3. extlinux fallback entry (= the kernel running now)"
[ -e $X.pre-surface ] || cp -p $X $X.pre-surface
cp -p /boot/zImage /boot/zImage.old
cp -p $DTB $DTB.fallback
if grep -q '^LABEL fallback' $X; then
  awk -v d="FDT $DTB.fallback" '
    /^LABEL / { inf = ($2 == "fallback") }
    inf && $1 == "FDT" { sub(/FDT .*/, d) }
    { print }' $X > $X.new
  mv $X.new $X
else
  cat >> $X <<EOT

LABEL fallback
    MENU LABEL Previous kernel (zImage.old)
    LINUX /boot/zImage.old
    FDT $DTB.fallback
    APPEND root=$ROOT rootwait rw console=ttyS0,115200n8 console=tty0 loglevel=4
EOT
fi
sed -n '/^LABEL fallback/,/^$/p' $X

echo "== 4. pacman"
pacman -Sy
pacman -S --noconfirm \
  --overwrite '/boot/zImage' --overwrite '/boot/dtbs/*' --overwrite '/usr/lib/modules/*' \
  surface-rt-tools linux-surface-rt
sync

echo "== check"
pacman -Qo /boot/zImage $DTB
md5sum /boot/zImage /boot/zImage.old $DTB $DTB.fallback
echo "pending upgrades (pacman -Qu):"; pacman -Qu || true
echo "Done. If /boot/zImage changed, the new kernel runs after the next APX boot (boot-apx.sh boot)."
