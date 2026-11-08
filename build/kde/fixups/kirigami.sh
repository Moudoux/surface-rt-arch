# Kirigami on grate: plain icon shader variant for icons at rest (no pow per
# pixel); local patch from sources/kirigami (git diff, uncommitted).
cp /pkg/fixups/kirigami-icon-plain.patch .
# The tarball download.kde.org serves (2026-11-07) has sha256 811a8a15... instead of
# Arch's pinned f21df727...: checked good signature (Nicolas Fella, 90A968AC..., in
# validpgpkeys, made 2026-10-08) and contents identical to KDE git tag v6.31.0.
sed -i -e "s/^pkgrel=\([0-9]*\)$/pkgrel=\1.${SRT_REL:-1}/" \
       -e 's/f21df727b0244925ea48df108de4d3d82f0a49f565b6e2a69e1b0ad01882fd15/811a8a150e636216269c0346e62470997fa282113a39e32b2d332bd5969926f0/' \
       -e "s|^source=(\(.*\))$|source=(\1\n        kirigami-icon-plain.patch)|" PKGBUILD
python3 -I - <<'PY'
import re
t=open('PKGBUILD').read()
m=re.search(r"^sha256sums=\((.*?)\)", t, re.S|re.M)
t=t[:m.end(1)]+"\n            'SKIP'"+t[m.end(1):]
t=t.replace("build() {", "prepare() {\n  patch -d $pkgname-$pkgver -p1 < kirigami-icon-plain.patch\n}\n\nbuild() {",1)
open('PKGBUILD','w').write(t)
PY
grep -n -A3 '^source=\|^sha256sums=\|^prepare\|^pkgrel' PKGBUILD
