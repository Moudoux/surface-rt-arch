#!/bin/bash
# Incremental KWin rebuild for testing patch iterations (instead of a full ~55 min build.sh kwin).
# Uses the makepkg tree saved in build/kde/work/kwin (mounted at /tmp/b, the path CMake recorded),
# copies the files changed in sources/kwin (uncommitted edits, `git diff`) into it, and runs `makepkg -e`
# (no extract, no prepare: patches are not re-applied, the edited files are copied instead).
# Usage: build/kde/rebuild-kwin.sh <pkgrel>     e.g. 2.2  (each test build gets a new pkgrel)
set -euo pipefail
rel=${1:?usage: $0 <pkgrel, e.g. 2.2>}
cd "$(dirname "$0")"
here=$PWD; src=$(cd ../.. && pwd)/sources/kwin
[ -d work/kwin/src ] || { echo "no saved tree in work/kwin: run build.sh kwin first"; exit 1; }
tree=$(ls -d work/kwin/src/kwin-*/ | head -1)
# keep the patch file in sync for full builds
git -C $src diff > fixups/kwin-surface-rt-gles2.patch
# files touched now or in earlier iterations (so reverted edits are copied back too)
touch work/kwin/.touched
{ cat work/kwin/.touched; git -C $src diff --name-only; } | sort -u > work/kwin/.touched.new && mv work/kwin/.touched.new work/kwin/.touched
while read -r f; do
  if [ -f "$src/$f" ]; then cmp -s "$src/$f" "$tree/$f" || { cp "$src/$f" "$tree/$f"; echo "updated $f"; }; fi
done < work/kwin/.touched
sed -i "s/^pkgrel=.*/pkgrel=$rel/" work/kwin/PKGBUILD
docker build --platform linux/arm/v7 -t srt-kde-build . >/dev/null
echo "== kwin incremental ($(date +%T))"
docker run --rm --platform linux/arm/v7 -u root \
  -v "$here/work/kwin":/tmp/b -v "$here":/pkg srt-kde-build bash -c '
    set -e; cd /tmp/b
    deps=$(runuser -u alarm -- makepkg --printsrcinfo | sed -n "s/^\s*\(make\)\?depends = //p" | sed "s/[<>=].*//" | sort -u)
    pacman -S --noconfirm --needed --asdeps $deps >/dev/null
    runuser -u alarm -- env MAKEFLAGS=-j24 CMAKE_BUILD_PARALLEL_LEVEL=24 PKGDEST=/pkg/pkg makepkg -ef --noconfirm --nocheck' \
  > logs/kwin-incremental.log 2>&1 && echo "   ok ($(date +%T))" || { echo "   FAILED, see logs/kwin-incremental.log"; tail -20 logs/kwin-incremental.log; exit 1; }
ls -la pkg/kwin-*
