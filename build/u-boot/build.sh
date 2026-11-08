#!/bin/sh
set -e
export ARCH=arm CROSS_COMPILE=arm-linux-gnueabihf-
OUT=/work/build/u-boot/out
cd /work/sources/mainline-u-boot
make O="$OUT" surface-rt_defconfig
make O="$OUT" -j"$(nproc)"
ls -la "$OUT/u-boot-dtb-tegra.bin"
