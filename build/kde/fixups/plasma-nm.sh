# ALARM's qt6-webengine is a stub without Qt6WebEngineWidgets (no Chromium on armv7h): build without the OpenConnect VPN plugin.
sed -i -e "s/^optdepends=('openconnect: Cisco AnyConnect VPN plugin'/optdepends=(/" -e '/^\s*openconnect\s*$/d' -e '/^\s*qt6-webengine)\s*$/s/.*/)/' -e 's/^\(makedepends=(extra-cmake-modules\)$/\1/' \
       -e 's/-DBUILD_TESTING=OFF/-DBUILD_TESTING=OFF -DBUILD_OPENCONNECT=OFF/' PKGBUILD
