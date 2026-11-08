# KWin on grate (Tegra 3, GLES 2.0): local patch (uncommitted edits in sources/kwin, exported with git diff).
cp /pkg/fixups/kwin-surface-rt-gles2.patch .
sed -i -e 's|^  https://invent.kde.org/plasma/kwin/-/commit/c9d720793bbcef9483ab290673032d39441c0e6f.patch$|&\n  kwin-surface-rt-gles2.patch|' \
       -e 's|^\(  patch -d $pkgname-$pkgver -p1 < c9d720793bbcef9483ab290673032d39441c0e6f.patch.*\)$|\1\n  patch -d $pkgname-$pkgver -p1 < kwin-surface-rt-gles2.patch|' \
       -e "s/^pkgrel=\([0-9]*\)$/pkgrel=\1.${SRT_REL:-3}/" PKGBUILD
# one more checksum entry for the new source
python3 -I - <<'PY'
import re
t=open('PKGBUILD').read()
m=re.search(r"^sha256sums=\((.*?)\)", t, re.S|re.M)
t=t[:m.end(1)]+"\n            'SKIP'"+t[m.end(1):]
open('PKGBUILD','w').write(t)
PY
