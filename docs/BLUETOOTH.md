# Bluetooth

The Surface RT's Wi-Fi/Bluetooth chip is a Marvell 88W8797. Wi-Fi runs over SDIO and Bluetooth over UART:
UARTC is `/dev/ttyTHS1`, H4 at 3 Mbaud with RTS/CTS.

The linux-firmware build (14.68.29.p59, BT over SDIO) doesn't bring Bluetooth up on the UART. That needs the
**14.62.17.p215 BT_UART** combo firmware, which ships inside the Windows driver. **It is not
redistributable, so it isn't in this repository.** Extract it from your own copy of Windows.

## 1. Get the Windows driver

Any of these works (the extractor verifies the exact file):

- **Windows RT 8.1 IR5 driver package:** `Devices/Nvidia/SurfaceRT/Install/mwls97w8arm.inf_arm_*/mwls97w8arm.sys`,
  SHA256 `ac47a0927099cffac55d51dbc91da32f848481f04987a8de5027548245a87fde`.
- **Microsoft Surface RT recovery image, Windows RT 8.1 (6.3.9600.16384):** `sources/install.wim`, file
  `Windows/System32/Drivers/mwls97w8arm.sys` (DriverVer 14.62.17215.91), SHA256
  `a586b6e85f1125c9c22fad303a73deed7c24cdc30359121cef15b1a54d0f86a3`. You can read it without unpacking the
  whole image:
  `7z e install.wim -o<dir> '1/Windows/System32/Drivers/mwls97w8arm.sys'`. Check the image index with `7z l`.

Both files contain the same firmware image at the same offset. Other driver versions are refused.

## 2. Extract

`fetch-sources.sh` applies `build/bluetooth/surface-rt-patches-extractor-rt81-recovery.patch` to ThatzOkay's
extractor, so it accepts both driver builds:

```sh
python3 -I sources/surface-rt-patches/tools/bluetooth/extract-windows-uart-firmware.py mwls97w8arm.sys sd8797_uapsta.bin
```

The output must be 434264 bytes with SHA256
`97cc4df3d978db1485c3531c13749cc91d12eae7570c38edeabfd2dd9e6d8b74`, tag
`$Id: w8797-B0, RF878X, FP62_WIN7, 14.62.17.p215, BT_UART $`.

## 3. Install on the tablet

Copy `sd8797_uapsta.bin` and `build/bluetooth/install-bt-firmware.sh` to the tablet, then (**root**):

```sh
sudo sh install-bt-firmware.sh ./sd8797_uapsta.bin
```

The script:

- checks the hash;
- installs the firmware to `/usr/lib/firmware/updates/mrvl/`;
- sets `mwifiex_sdio force_legacy_scan=1` in `/etc/modprobe.d/` (needed with p215; the parameter comes from
  surface-rt-patches 0027);
- enables `surface-rt-bluetooth.service` from surface-rt-tools.

Reboot, then check:

- `journalctl -k`: "FW download over, size 434264 bytes" and "mwifiex 1.0 (14.62.17.p215)".
- `systemctl status surface-rt-bluetooth`: "attached /dev/ttyTHS1 as H4".

Wi-Fi keeps working on p215.

## How the attach works

Plain `btattach` fails: the first exchange after opening the port is unreliable (stale bytes from an earlier
reply), and `hci_uart` gives up after one HCI Reset. `surface-rt-tools`' `bt-attach` does this instead:

1. It answers one HCI Reset itself at 3 Mbaud with RTS/CTS.
2. It drains the line.
3. Only then does it hand the port to the kernel (N_HCI, H4).
4. If 3 Mbaud gets no answer, it does what the Windows driver does: HCI Reset at 115200, then vendor command
   0xfc09 to switch to 3 Mbaud.

The service starts once mwifiex has loaded the firmware (`mlan0` exists) and before `bluetooth.service`.

## Limits

- No Bluetooth wake from suspend. The controller/hci_uart rejects the remote-wakeup flags; this is harmless
  otherwise.
- No Wi-Fi wake-on-WLAN: kernel patch 0033 drops the WLAN wake IRQ. With p215 the wake line (PU5) idles
  low, and the hard-coded level-low trigger aborted every suspend.
- The Windows driver also sends sleep-configuration vendor commands (0xfc07, 0xfc28, 0xfc53, 0xfc23). They
  aren't sent here yet.
