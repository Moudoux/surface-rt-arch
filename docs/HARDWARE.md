# Hardware status

Status with this repository's kernel (`linux-surface-rt` 7.0.1, build 6), booted via APX, checked on one
Surface RT 32 GB in November 2026.

| Component | State | Notes |
|---|---|---|
| Display, backlight | Works | |
| Touchscreen | Works | Tap, drag, long-press and kinetic scroll in Plasma; plasma-keyboard on-screen keyboard. |
| Type Cover | Works, hotplug too | surface-rt-patches 0002 keeps `vdd_cover` on. The Touch Cover (capacitive) is untested. |
| Buttons (power, volume, Windows) | Work | The Windows button toggles the launcher. |
| Lid (hall sensor) | Works | Closing suspends, opening wakes (Plasma default). |
| Auto-rotation / tablet mode | Works | Cover off: KWin tablet mode, auto-rotate in all four orientations, touch follows. |
| Battery / charger | Readings work | Shown in Plasma's battery applet. Drain is not measured yet. |
| CPU DVFS, cpuidle, thermal | Work | 51 MHz-1.4 GHz, 3 idle states. `build/perf/surface-rt-cpufreq.conf` tunes ondemand for the desktop. |
| Suspend / resume | Works (deep = LP2) | Wake by power button or lid. Wi-Fi, GPU, touch, Bluetooth and the lock screen survive. LP1/LP0 are not done. |
| GPU (GR3D/GR2D) | Works | See [GPU.md](GPU.md). |
| Wi-Fi (Marvell 88W8797, SDIO) | Works, **WPA2 only** | surface-rt-patches 0025 powers the chip under APX. The firmware has no WPA3-SAE (hardware/firmware limit). |
| Bluetooth (88W8797, UART) | Works | Needs firmware extracted from Windows: [BLUETOOTH.md](BLUETOOTH.md). No wake from suspend. |
| microSD | Works, UHS-I SDR104 (~84 MB/s) | Patch 0034: sdhci-tegra stops clocking the card between commands, so mainline's post-CMD11 DAT0 check never saw the card release DAT. Before the patch, cards fell back to 25 MB/s high speed. |
| USB-A host | Works | |
| Sensors (accel, gyro, magnetometer, light) | Enumerate over IIO | The accelerometer drives auto-rotation. The light sensor read 0 lux in one test. |
| Speakers / headphones (WM8962) | Devices present | Listen test not done yet. An intermittent `wm8962 ... ASoC error (-16)` at boot (2 of 6 boots); the card still registers. |
| Microphone | **Not working** | surface-rt-patches 0023/0024 are applied but unvalidated. |
| HDMI | **Untested** | surface-rt-patches 0029 is applied. Upstream reports 1920x1080@60 working. |
| Front camera | Not in this kernel | surface-rt-patches' camera series captures raw Bayer; a userspace debayer path is needed. |
| Rear camera | Not working | Never answered in upstream tests. |
| H.264 decode (tegra_vde) | Module loads | No player integration. |
| eMMC (Windows), SPI flash | Never written | Keep it that way. |

## Hard limits

These can't be fixed in software:

- Wi-Fi is WPA2 only.
- The GPU is GLES 2.0 class with FP20 fragment precision and short shader programs.
- APX boot is tethered. The SPI flash is reportedly write-locked; this claim is unverified.

## Boot quirk

After any normal power-off (Linux or Windows), the bootrom does enter RCM: the screen is black and the PC
sees a USB device. But enumeration then fails (`device descriptor read/64, error -110/-71`). Holding Power
about 10 s first clears it. The cause is somewhere in the always-on power domain and hasn't been identified;
it may be inherent to the device. The Open Surface RT wiki gives the same advice.
