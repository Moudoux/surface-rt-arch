#!/bin/bash
# One-time host setup for boot-apx.sh (no root):
#   1. builds the Fusée Gelée iROM patch payload T30_patch_irom.bin from sources/Open-Surface-RT_fusee-tools
#      in the srt-cross image (arm-none-eabi) into build/fusee/payloads/
#   2. creates build/fusee/venv with pyusb for fusee-launcher.py
#   3. copies the U-Boot binary from build/u-boot/out (if built)
# Usage (from the repository root): build/fusee/setup.sh
set -euo pipefail
cd "$(dirname "$0")"
top=$(cd ../.. && pwd)
T=$top/sources/Open-Surface-RT_fusee-tools
[ -d "$T/payloads" ] || { echo "missing $T: run ./fetch-sources.sh first"; exit 1; }
mkdir -p payloads
cp "$T"/payloads/*.c "$T"/payloads/*.h "$T"/payloads/*.S "$T"/payloads/*.lds "$T"/payloads/Makefile payloads/
docker run --rm -v "$top":/work srt-cross sh -c '
  cd /work/build/fusee/payloads &&
  make T30_patch_irom.bin LDFLAGS="-nostdlib -L $(dirname $(arm-none-eabi-gcc -print-libgcc-file-name)) -lgcc" >/dev/null'
ls -la payloads/T30_patch_irom.bin
[ -x venv/bin/python ] || python3 -m venv venv
venv/bin/pip install -q -r "$T/fusee-launcher-new/requirements.txt"
venv/bin/python -c 'import usb; print("pyusb", usb.__version__)'
if [ -f "$top/build/u-boot/out/u-boot-dtb-tegra.bin" ]; then
  cp "$top/build/u-boot/out/u-boot-dtb-tegra.bin" .
  echo "copied u-boot-dtb-tegra.bin"
else
  echo "note: build U-Boot (build/u-boot/build.sh), then copy build/u-boot/out/u-boot-dtb-tegra.bin here"
fi
