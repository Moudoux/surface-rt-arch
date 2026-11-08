#!/bin/bash
# Tethered APX boot of the Surface RT. Nothing is written to the tablet's storage:
# the iROM patch and U-Boot live in RAM only until power-off.
#   1. Power off the Surface. Hold Volume Up, press Power -> screen stays black (APX mode).
#   2. Connect the USB A-A cable (USB 3 port on the PC is recommended by the tools' authors).
#   3. Run: ./boot-apx.sh bct   (once: saves this device's BCT), then re-enter APX and ./boot-apx.sh boot
set -euo pipefail
cd "$(dirname "$0")"
T=../../sources/Open-Surface-RT_fusee-tools
PY=./venv/bin/python
wait_apx() {
  echo "Waiting for Tegra APX device (0955:7130)..."
  until lsusb -d 0955:7130 >/dev/null 2>&1; do sleep 1; done
}
patch_irom() {
  wait_apx
  "$PY" "$T/fusee-launcher-new/fusee-launcher.py" -P 0x7130 payloads/T30_patch_irom.bin
  sleep 1
}
case "${1:-auto}" in
  bct)  step=bct ;;
  boot) step=boot ;;
  auto) if [ -s surface-rt.bct ]; then step=boot; else step=bct; fi ;;
  *) echo "usage: $0 [bct|boot]"; exit 2 ;;
esac
if [ "$step" = bct ]; then
  echo "== Reading this device's BCT (read-only)"
  patch_irom
  "$T/utils/nvflash_v1.13.87205" --getbct --bct surface-rt.bct --configfile "$T/utils/flash.cfg"
  ls -la surface-rt.bct
  echo "BCT saved. Hold Power ~10 s, re-enter APX (Vol Up + Power), then run: $0 boot"
  exit 0
fi
[ -s surface-rt.bct ] || { echo "No surface-rt.bct yet: run $0 bct first"; exit 1; }
echo "== Booting U-Boot from RAM"
patch_irom
"$T/utils/nvflash_v1.13.87205_miniloader_patched" --setbct --bct surface-rt.bct \
  --configfile "$T/utils/flash.cfg" --bl u-boot-dtb-tegra.bin --go
echo "U-Boot sent. It boots extlinux.conf from the microSD."
