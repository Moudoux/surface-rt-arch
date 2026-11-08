#!/bin/sh
# Runs inside the srt-cross container. /work = project root.
# SRT_KBUILD=<n> (docker run -e SRT_KBUILD=5 ...): kernel release 7.0.1-surface-rt-<n>, so each build has its
# own /usr/lib/modules dir and the fallback kernel keeps its modules (build/repo/REBUILD.md). Unset: old
# behaviour, 7.0.1-surface-rt+ (builds 1-4).
set -e
SRC=/work/sources/libre-tegra_linux
OUT=/work/build/kernel/out
mkdir -p "$OUT"
export ARCH=arm CROSS_COMPILE=arm-linux-gnueabihf-
LV=${SRT_KBUILD:+LOCALVERSION=-$SRT_KBUILD}
cd "$SRC"
if [ ! -f "$OUT/.config" ] || [ "$1" = "config" ]; then
  make O="$OUT" grate_defconfig
  scripts/kconfig/merge_config.sh -m -O "$OUT" "$OUT/.config" kernel/configs/pmos.config /work/build/kernel/archlinux-surface-rt.config
  make O="$OUT" olddefconfig
fi
make O="$OUT" $LV -j"$(nproc)" zImage modules dtbs
rm -rf "$OUT/staging"
make O="$OUT" $LV INSTALL_MOD_PATH="$OUT/staging" INSTALL_MOD_STRIP=1 modules_install >/dev/null
echo "KERNELRELEASE=$(make -s O="$OUT" $LV kernelrelease)"
