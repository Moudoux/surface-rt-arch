#!/bin/bash
# Host side: builds mesa-grate inside the emulated armv7h container.
set -e
cd "$(dirname "$0")"
docker build --platform linux/arm/v7 -t srt-mesa-build . >/dev/null
mkdir -p pkg
# Build tree kept in work/ (mounted at /tmp/b, the path meson recorded), so rebuilds are incremental.
mkdir -p work
docker run --rm --platform linux/arm/v7 \
  -v "$(cd ../.. && pwd)/sources/libre-tegra_mesa":/src/mesa:ro \
  -v "$PWD/work":/tmp/b \
  -v "$PWD":/pkg srt-mesa-build \
  bash -c 'cp /pkg/PKGBUILD /tmp/b/ && cd /tmp/b && MAKEFLAGS=-j24 PKGDEST=/pkg/pkg makepkg -f --nocheck --skipinteg'
ls -la pkg
