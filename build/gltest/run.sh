#!/bin/sh
# Run every shader test on the Surface with grate and with Mesa's software
# renderer (same package, LIBGL_ALWAYS_SOFTWARE=1), compare the pixels.
# Everything goes to the Surface's /tmp/gt (tmpfs, RAM). No install, no sudo.
#
#   build/gltest/run.sh [-p mesa-pkg.tar.xz] [-e "VAR=val ..."] [-b] [test.frag ...]
#
# -p  unpack this mesa-grate package into /tmp/gt/m first (default: keep what
#     is there; on first use the newest build/mesa/pkg/mesa-grate-*.pkg.tar.xz)
# -b  premultiplied source-over blending onto a coloured background (both runs)
# -B  blend spec for GLTEST_BLEND (e.g. "cc isc" or "ca one sa isa add"),
#     see gltest.c; GLTEST_BLEND_COLOR / GLTEST_COLORMASK pass through too
# -v  vertex shader file (GLTEST_VS) for every test, both runs
# GLTEST_SCISSOR="x y w h" passes through as well; GLTEST_PTS="x y x y ..." sets the sampled pixels
# -e  extra environment for the grate run (e.g. "GRATE_FP_ROWS=2")
# The grate run always has GRATE_DEBUG=tgsi (dump in /tmp/gt/t/<test>.err),
# which also reports the chosen pixel rows and EXEC count.
# Tests default to build/gltest/t/*.frag. Output: one line per test,
# PASS/FAIL/REJECT with the largest channel difference (tolerance TOL=0.03).
set -eu
HOST=${SURFACE:-alarm@surface-rt}
HERE=$(cd "$(dirname "$0")" && pwd)
PKG= ENV= VS= BLEND=${GLTEST_BLEND:-}
while getopts p:e:bB:v: o; do
  case $o in p) PKG=$OPTARG;; e) ENV=$OPTARG;; b) BLEND=1;; B) BLEND=$OPTARG;; v) VS=$OPTARG;; *) exit 1;; esac
done
shift $((OPTIND - 1))
[ $# -gt 0 ] || set -- "$HERE"/t/*.frag

ssh "$HOST" 'mkdir -p /tmp/gt/t /tmp/gt/m'
if [ -z "$PKG" ] && ! ssh "$HOST" 'test -e /tmp/gt/m/usr/lib/libEGL_mesa.so.0'; then
  PKG=$(ls -t "$HERE"/../mesa/pkg/mesa-grate-*.pkg.tar.xz | head -1)
fi
if [ -n "$PKG" ]; then
  scp -q "$PKG" "$HOST":/tmp/gt/mesa.pkg.tar.xz
  ssh "$HOST" 'rm -rf /tmp/gt/m && mkdir /tmp/gt/m && bsdtar -xf /tmp/gt/mesa.pkg.tar.xz -C /tmp/gt/m 2>/dev/null'
  echo "unpacked $(basename "$PKG")"
fi
VSF=${VS:+$(basename "$VS")}
scp -q "$HERE"/gltest "$@" ${VS:+"$VS"} "$HOST":/tmp/gt/t/

names=
for f in "$@"; do names="$names $(basename "$f")"; done
ssh "$HOST" "cd /tmp/gt/t && TOL=\${TOL:-0.03} GENV='$ENV' BLEND='$BLEND' BCOL='${GLTEST_BLEND_COLOR:-}' CMASK='${GLTEST_COLORMASK:-}' SCIS='${GLTEST_SCISSOR:-}' VSF='$VSF' RGBA='${GLTEST_RGBA:-}' XPTS='${GLTEST_PTS:-}' sh -s $names" <<'EOF'
[ -n "$BLEND" ] && export GLTEST_BLEND="$BLEND"
[ -n "$BCOL" ] && export GLTEST_BLEND_COLOR="$BCOL"
[ -n "$CMASK" ] && export GLTEST_COLORMASK="$CMASK"
[ -n "$SCIS" ] && export GLTEST_SCISSOR="$SCIS"
[ -n "$VSF" ] && export GLTEST_VS="/tmp/gt/t/$VSF"
[ -n "$RGBA" ] && export GLTEST_RGBA=1
export LD_LIBRARY_PATH=/tmp/gt/m/usr/lib
export __EGL_VENDOR_LIBRARY_FILENAMES=/tmp/gt/m/usr/share/glvnd/egl_vendor.d/50_mesa.json
PTS=${XPTS:-"8 8 56 8 8 56 32 32 20 44 44 20 3 60 60 3"}
pass=0 fail=0
for t in "$@"; do
  env GRATE_DEBUG=tgsi $GENV ./gltest "$t" $PTS > "$t.grate" 2> "$t.err"; rc=$?
  env LIBGL_ALWAYS_SOFTWARE=1 GALLIUM_DRIVER=softpipe ./gltest "$t" $PTS > "$t.ref" 2>/dev/null
  rej=$(grep -m1 -E 'rejected|exhausted|UNIMPLEMENTED' "$t.err" || true)
  if [ $rc -ne 0 ] || [ -n "$rej" ]; then
    printf '%-24s REJECT %s\n' "$t" "${rej:-exit $rc}"; fail=$((fail+1)); continue
  fi
  res=$(paste -d' ' "$t.grate" "$t.ref" | awk -v tol="$TOL" '
    /^px/ { gsub(/[()=,]/, " ");
            # fields: px x y uv u v rgba r g b a (1-11) | same (12-22)
            for (i = 0; i < 4; i++) { d = $(8+i) - $(19+i); if (d < 0) d = -d; if (d > m) { m = d; at = $2 "," $3 } }
            g = g sprintf(" (%.2f %.2f %.2f %.2f)", $8, $9, $10, $11)
            r = r sprintf(" (%.2f %.2f %.2f %.2f)", $19, $20, $21, $22) }
    END { printf "%s %.3f %s\n", (m > tol ? "FAIL" : "PASS"), m, at;
          if (m > tol) { print "    grate:" g; print "    ref:  " r } }')
  info=$(grep -o 'GRATE FRAG: [0-9] rows, [0-9]* EXECs' "$t.err" | tail -1 | sed 's/GRATE FRAG: //')
  printf '%-24s %s\n' "$t" "$res${info:+   [$info]}"
  case $res in PASS*) pass=$((pass+1));; *) fail=$((fail+1));; esac
done
echo "== $pass passed, $fail failed/rejected"
EOF
