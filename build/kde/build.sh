#!/bin/bash
# Host side: build KDE packages for armv7h in the emulated container.
# Usage: build/kde/build.sh purpose plasma-desktop plasma-nm ...   (log: build/kde/logs/<name>.log)
# Our pkgrel = Arch pkgrel + .N; N defaults per fixup (fixups/<pkg>.sh), override: SRT_REL=2 build.sh kwin
set -e
cd "$(dirname "$0")"
docker build --platform linux/arm/v7 -t srt-kde-build . >/dev/null
mkdir -p pkg logs ccache srcdest
# ccache/ (compiler cache, shared by all packages) and srcdest/ (downloaded source tarballs) persist
# across runs; the build tree itself is still thrown away (container /tmp/b).
for n in "$@"; do
  echo "== $n ($(date +%T))"
  docker run --rm --platform linux/arm/v7 \
    -v "$(cd ../.. && pwd)/sources/arch-pkgbuilds":/pkgbuilds:ro \
    -v "$PWD/ccache":/ccache -v "$PWD/srcdest":/srcdest \
    ${SRT_REL:+-e SRT_REL=$SRT_REL} -v "$PWD":/pkg -u root srt-kde-build bash /pkg/inner.sh "$n" > logs/$n.log 2>&1 \
    && echo "   ok ($(date +%T))" || { echo "   FAILED, see logs/$n.log"; tail -20 logs/$n.log; exit 1; }
done
ls -la pkg
