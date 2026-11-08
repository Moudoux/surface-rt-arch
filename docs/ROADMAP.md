# Roadmap

Roughly in order of impact. Contributions and test reports are welcome, especially from other devices
(different microSD cards, Touch Cover, HDMI monitors).

## 1. Untethered boot (EFI)

This is the biggest gap: today every cold boot needs the PC. The software route is EFI boot (Yahallo
jailbreak, Windows Boot Manager, then GRUB or the EFI stub).

1. Build an EFI-bootable image.
2. Confirm L2, DVFS and Wi-Fi under EFI.
3. Port this project's APX DTS changes (0031, 0033, Bluetooth, HDMI) to `tegra30-microsoft-surface-rt-efi.dts`.
4. Find out why 2 GiB breaks under EFI (EFI memory map / reserved regions).
5. Get cpuidle LP2: it needs the TrustZone SMC that Windows uses for CPU power-down. The TrustZone code has
   to be reverse-engineered; the wiki gives MVBAR as 0x811f8000.

## 2. Hardware still open

- Audio: listen test (speakers, headphones, switching), the UCM profile, and the intermittent
  `wm8962 ... ASoC error (-16)` at boot.
- Microphone (WM8962 digital mic route, patches 0023/0024).
- HDMI: hotplug, modes, KWin multi-output on grate, HDMI audio.
- Battery drain measurement. The power I2C bus wakes ~92 times/s at idle; the source is unknown.
- Bluetooth sleep-configuration vendor commands (see [BLUETOOTH.md](BLUETOOTH.md)).
- Front camera (series exists upstream; needs a debayer path), H.264 decode integration.
- Text console (fbcon) ghosting.

## 3. GPU

- Stencil (rotated clips), RGB `SRC_ALPHA_SATURATE`, real `fwidth`.
- Rounded corners in KWin at a lower cost: apply the RoundedCorners trait only to the quads that need it.

## 4. Upstreaming

- 0034 (sdhci-tegra UHS switch check) probably affects every Tegra SDHCI board that tries UHS. It is a
  candidate for mainline.
- The grate Mesa work, the KWin graceful-degradation changes and the kernel grate fixes (0030, 0032) could go
  to libre-tegra / KDE.
