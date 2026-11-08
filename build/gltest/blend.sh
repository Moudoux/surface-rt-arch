#!/bin/sh
# Blend mode sweep: runs tb/blend-src.frag through run.sh with each blend
# spec (GLTEST_BLEND syntax, see gltest.c), grate vs softpipe.
#   build/gltest/blend.sh [-p mesa-pkg.tar.xz] [spec ...]
HERE=$(cd "$(dirname "$0")" && pwd)
PKGOPT=
if [ "${1:-}" = -p ]; then PKGOPT="-p $2"; shift 2; fi
[ $# -gt 0 ] || set -- \
  "one isa" "cc isc" "one one" "ca one" "ca ica" "sa isa" "zero sc" \
  "dc zero" "one ida" "sa isa one isa" "icc cc" "one one one one sub" \
  "one one one one rsub" "one one one one min" "one one one one max" \
  "sat one" "da idc zero one"
for spec in "$@"; do
  printf '%-22s ' "[$spec]"
  "$HERE"/run.sh $PKGOPT -B "$spec" "$HERE"/tb/blend-src.frag | grep -v '^== \|^unpacked'
  PKGOPT=
done
