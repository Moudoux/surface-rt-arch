# Background: boot methods and sources

Research notes from the start of the project (November 2026). The Open Surface RT wiki is partly out of date.
This page records what the current code actually does.

## Two ways to boot Linux on the Surface RT

| | APX (used here) | EFI |
|---|---|---|
| How | Fusée Gelée bootrom exploit over USB (RCM) patches the iROM, then nvflash loads U-Boot into RAM | Jailbroken Windows boot manager (secure boot off via the Yahallo exploit) starts an EFI Linux loader |
| PC needed | Every cold boot (USB A-to-A) | No |
| L2 cache | Yes | Fixed in libre-tegra via the Microsoft TrustZone SMC service 0x2 (unconfirmed on device) |
| PMIC / DVFS | Yes | Fixed in the libre-tegra EFI DTS (regulators and OPPs kept; unconfirmed on device) |
| SMP | Yes | Yes (ACPI parking protocol patch) |
| cpuidle LP2 | Yes | **Open:** needs the TrustZone SMC that Windows uses for CPU power-down |
| RAM | 2 GiB | **1 GiB** in the EFI DTS ("2 GB causes issues with EFI"); open |
| Wi-Fi | Fixed by surface-rt-patches 0025 (nothing powers the chip under APX) | The firmware powers the chip |

APX is the fastest way to a fully featured system and is the development platform here. EFI is the only
untethered path, and its remaining gaps are cpuidle LP2 and 2 GiB RAM. Native untethered APX would need our
own BCT and bootloader in SPI flash. The wiki says the Winbond W25Q32 SPI flash is OTP/write-locked, which
would mean replacing the chip. That claim is unverified.

## Upstream sources

| Tree | Role |
|---|---|
| [libre-tegra/linux](https://codeberg.org/libre-tegra/linux) `v7.0.1-stable` | Kernel (same as postmarketOS `linux-postmarketos-grate`). Has the Surface RT APX and EFI DTS, the ACPI parking protocol, TrustZone L2 SMC and the grate DRM driver. There is no Surface RT DTS in mainline Linux. |
| [libre-tegra mesa](https://github.com/external-mirrors/libre-tegra-mesa) `grate-26.2.3` | Mesa with the grate Gallium driver |
| [ThatzOkay/surface-rt-patches](https://github.com/ThatzOkay/surface-rt-patches) | The most current Surface RT work: kernel patches (Wi-Fi, BT, audio, cover, HDMI, camera), the Mesa grate series 0001-0019, the BT firmware extractor |
| [U-Boot](https://source.denx.de/u-boot/u-boot) mainline | `surface-rt_defconfig`. Boots extlinux.conf from microSD, then eMMC. Volume Down opens a boot menu. |
| [Open-Surface-RT/fusee-tools](https://github.com/Open-Surface-RT/fusee-tools) | fusee-launcher, iROM payload sources, nvflash (v1.13 works for `--getbct`; v3.08 fails with "Bct file not found") |
| [Arch Linux ARM](https://archlinuxarm.org) armv7h | Distribution. Current Qt 6 / KF6 / Plasma 6.8, but plasma-desktop, plasma-nm, discover and others are stuck at 5.27, and purpose and plasma-mobile are missing: rebuilt here. ALARM's qt6-webengine is a stub (no WebEngineWidgets), so OpenConnect in plasma-nm is off. kaccounts-integration is uninstallable on armv7h, so it is dropped. |

## Build notes and gotchas

- Docker images: `srt-cross` (Debian trixie cross toolchain, runs as uid 1000). `srt-alarm-base` and
  `srt-alarm-custom` (armv7 Arch Linux ARM under qemu-user) are the base for `srt-mesa-build` and
  `srt-kde-build`.
- pacman inside qemu-user: Landlock sandbox and `CheckSpace` misbehave. The build images disable both;
  `customize.sh` restores them for the real device.
- pacman 7 on the device needs Landlock in the kernel (`CONFIG_SECURITY_LANDLOCK=y`, `landlock` in `CONFIG_LSM`).
- `sudo` doesn't work in qemu-user containers (setuid is ignored). The KDE container installs dependencies as
  its root, then runs makepkg via `runuser -u alarm`.
- GNU ld on 32-bit ARM rejects `-z pack-relative-relocs`, which ALARM's makepkg.conf sets. The KDE image
  strips it.
- `arch-meson` forces `--auto-features enabled`, which breaks on 32-bit ARM. The mesa-grate PKGBUILD calls plain
  `meson setup`.
- ThatzOkay's patches have no `From:` header, so `git am` fails. `fetch-sources.sh` uses `patch -p1` and one
  commit per patch.
- `docker export` leaves `/.dockerenv` and empty `/etc/hostname` and `/etc/hosts`. `assemble.sh` excludes them
  and the overlay supplies real ones.
- `systemd-repart` needs GPT, hence the GPT image with a "Linux root (ARM)" partition.
