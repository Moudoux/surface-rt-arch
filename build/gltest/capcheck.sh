#!/bin/sh
# Compile-and-draw the captured desktop GLSL shaders (build/gltest/captured,
# or the given files) with shadercheck on the Surface, on grate and on
# softpipe, and compare the centre pixel. Uses the package already unpacked
# in /tmp/gt/m (see run.sh -p). Rejections are listed under each shader.
#   build/gltest/capcheck.sh [file.glsl ...]
set -eu
HOST=${SURFACE:-alarm@surface-rt}
HERE=$(cd "$(dirname "$0")" && pwd)
[ $# -gt 0 ] || set -- "$HERE"/captured/*.glsl
ssh "$HOST" 'mkdir -p /tmp/gt/cap'
scp -q "$HERE"/shadercheck "$@" "$HOST":/tmp/gt/cap/
names=
for f in "$@"; do names="$names $(basename "$f")"; done
ssh "$HOST" "cd /tmp/gt/cap && sh -s $names" <<'REMOTE'
export LD_LIBRARY_PATH=/tmp/gt/m/usr/lib
export __EGL_VENDOR_LIBRARY_FILENAMES=/tmp/gt/m/usr/share/glvnd/egl_vendor.d/50_mesa.json
for f in "$@"; do
  g=$(./shadercheck "$f" 2> "$f.err" | tail -1)
  r=$(LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=softpipe ./shadercheck "$f" 2>/dev/null | tail -1)
  gp=$(echo "$g" | grep -o 'rgba=([^)]*)' || echo "$g" | cut -c1-60)
  rp=$(echo "$r" | grep -o 'rgba=([^)]*)' || echo "$r" | cut -c1-60)
  same=DIFF; [ "$gp" = "$rp" ] && same=same
  printf '%-14s grate %-30s softpipe %-30s %s\n' "$(echo $f | cut -c1-11)" "$gp" "$rp" "$same"
  grep -h -E 'rejected|exhausted|UNIMPLEMENTED|unsupported' "$f.err" | sort | uniq -c | sed 's/^/      /' || true
done
REMOTE
