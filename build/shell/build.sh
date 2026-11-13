#!/bin/sh
# Cross-build a crate under build/shell for the Surface: ./build.sh <crate-dir> [cargo args...]
set -e
HERE="$(cd "$(dirname "$0")" && pwd)"
CRATE="$1"; shift
export CARGO_TARGET_ARMV7_UNKNOWN_LINUX_GNUEABIHF_LINKER="$HERE/armv7-cc"
export CC_armv7_unknown_linux_gnueabihf="$HERE/armv7-cc"
export PKG_CONFIG_SYSROOT_DIR="$HERE/sysroot"
export PKG_CONFIG_LIBDIR="$HERE/sysroot/usr/lib/pkgconfig:$HERE/sysroot/usr/share/pkgconfig"
export PKG_CONFIG_PATH=
cd "$CRATE"
exec cargo build --target armv7-unknown-linux-gnueabihf "$@"
