# plasma-desktop on Tegra 3: drop CompactApplet's `enabled: dialog.visible` (QTBUG-146992 workaround,
# KDE bug 515280). It made every applet popup re-sync the theme colours of its whole item tree on each
# open/close: ~0.5 s plasmashell CPU per Kickoff open+close. pkgrel 1 -> 1.1.
cp /pkg/fixups/plasma-desktop-no-popup-disable.patch .
sed -i -e "s/^pkgrel=\([0-9]*\)$/pkgrel=\1.${SRT_REL:-1}/" \
       -e "s|^source=(\(.*\))$|source=(\1\n        plasma-desktop-no-popup-disable.patch)|" \
       -e "s|^\(            'SKIP')\)$|            'SKIP'\n            'SKIP')|" PKGBUILD
python3 -I - <<'PY'
t=open('PKGBUILD').read()
assert t.count("build() {")==1 and "prepare()" not in t
t=t.replace("build() {", "prepare() {\n  patch -d $pkgname-$pkgver -p1 < plasma-desktop-no-popup-disable.patch\n}\n\nbuild() {",1)
open('PKGBUILD','w').write(t)
PY
grep -n -A3 '^source=\|^sha256sums=\|^prepare\|^pkgrel' PKGBUILD
