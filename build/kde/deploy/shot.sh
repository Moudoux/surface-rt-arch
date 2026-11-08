#!/bin/sh
# Screenshot of the running KWin session on the Surface into a local PNG
# (KWin ScreenShot2 CaptureWorkspace via busctl, raw RGBX8888 converted here).
#   build/kde/deploy/shot.sh out.png
set -eu
HOST=${SURFACE:-alarm@surface-rt}
OUT=${1:-shot.png}
info=$(ssh "$HOST" 'cd /tmp && rm -f shotfifo shot.raw && mkfifo shotfifo && (cat shotfifo > shot.raw &) &&
  DBUS_SESSION_BUS_ADDRESS=unix:path=/run/user/1000/bus timeout 60 busctl --user call org.kde.KWin \
  /org/kde/KWin/ScreenShot2 org.kde.KWin.ScreenShot2 CaptureWorkspace "a{sv}h" 1 native-resolution b true 3 3>shotfifo;
  sleep 1')
echo "$info"
scp -q "$HOST":/tmp/shot.raw /tmp/shot.raw.$$
python3 -I - "$info" /tmp/shot.raw.$$ "$OUT" <<'PY'
import re, struct, sys, zlib
info, raw, out = sys.argv[1:]
get = lambda k: int(re.search(r'"%s" u (\d+)' % k, info).group(1))
w, h, stride, fmt = get("width"), get("height"), get("stride"), get("format")
assert fmt in (16, 17, 18), "QImage format %d not handled" % fmt  # RGBX/RGBA8888
d = open(raw, "rb").read()
rows = b"".join(b"\0" + b"".join(d[y*stride+i*4:y*stride+i*4+3] for i in range(w)) for y in range(h))
ch = lambda t, p: struct.pack(">I", len(p)) + t + p + struct.pack(">I", zlib.crc32(t + p) & 0xffffffff)
open(out, "wb").write(b"\x89PNG\r\n\x1a\n" + ch(b"IHDR", struct.pack(">IIBBBBB", w, h, 8, 2, 0, 0, 0)) +
                      ch(b"IDAT", zlib.compress(rows, 6)) + ch(b"IEND", b""))
print("wrote", out)
PY
rm -f /tmp/shot.raw.$$
