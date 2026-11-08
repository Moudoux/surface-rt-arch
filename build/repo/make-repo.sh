#!/bin/bash
# PC side: build linux-surface-rt (from build/kernel/out) and surface-rt-tools, then stage the [surface] repo
# in build/repo/surface/ from build/kde/pkg + build/mesa/pkg (hard links), using the same surface-repo-add the
# Surface runs (newest file per package in the db, one older file kept for rollback).
# Usage: build/repo/make-repo.sh            then copy to the Surface: see REBUILD.md "Deploy"
set -euo pipefail
cd "$(dirname "$0")"
here=$PWD; top=$(cd ../.. && pwd)
mkdir -p surface
# A package version is built once: an existing file is never rebuilt (so a later, untested build in
# build/kernel/out can't slip in under an old version). Bump pkgrel to ship changes.
for p in linux-surface-rt surface-rt-tools; do
  v=$(cd $p && makepkg --printsrcinfo | awk '/^\tpkgver =/{v=$3} /^\tpkgrel =/{r=$3} END{print v"-"r}')
  if compgen -G "surface/$p-$v-*.pkg.tar.*" >/dev/null; then echo "$p $v: already built"; continue; fi
  ( cd $p && rm -rf src pkg && CARCH=armv7h PKGDEST="$here/surface" makepkg -f --noconfirm >/dev/null && rm -rf src pkg )
  echo "$p $v: built"
done
ln -f "$top"/build/kde/pkg/*.pkg.tar.* "$top"/build/mesa/pkg/*.pkg.tar.* surface/
SRT_REPO="$here/surface" sh surface-rt-tools/surface-repo-add compat
