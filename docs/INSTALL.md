# Install guide

From a clean checkout to Plasma on the Surface RT. Everything on the PC runs as your normal user in Docker.
Only a few clearly marked steps need root (udev rule, writing the SD card, and steps on the tablet itself).

Times are from the author's 24-thread PC. Anything built for armv7h runs under qemu-user emulation and is slow.

## 0. What you need

- Microsoft Surface RT (first generation, Tegra 3). Nothing is written to its eMMC or SPI flash. APX boot
  does not need Windows to be jailbroken.
- A **USB A-to-A cable**. Short, plain USB 2.0 cables work best: a 1 m KVM cable caused enumeration errors
  (-71/-32), and a plain Nedis USB 2.0 cable on an AMD xHCI port worked.
- A **microSD card**, 16 GB or larger (the image is 8 GiB and grows to fill the card on first boot).
- An **x86_64 Linux PC** with:
  - Docker, usable without sudo (your user in the `docker` group);
  - qemu-user binfmt for ARM (Arch: `pacman -S qemu-user-static qemu-user-static-binfmt`);
  - git, python3 (with venv), curl, lsusb;
  - for `build/repo/` (local pacman repo, update checks): `makepkg`, `repo-add`, `vercmp`, `bsdtar`, which
    means an Arch Linux host or an Arch container;
  - about 40 GB of free disk space.

## 1. Sources

```sh
git clone <this repository> surface-rt && cd surface-rt
./fetch-sources.sh            # ~1.5 GB; add --dev for kwin/kirigami trees (patch development only)
```

This clones each upstream tree at the commit the published builds used and applies the patches. See the
script header for the list.

## 2. Cross toolchain image

```sh
docker build -t srt-cross build/docker
```

## 3. Kernel

```sh
docker run --rm -e SRT_KBUILD=6 -v $PWD:/work srt-cross build/kernel/build.sh
```

Output: `build/kernel/out/` (zImage, DTBs, `staging/lib/modules/7.0.1-surface-rt-6`). The config is
`grate_defconfig` + the tree's `kernel/configs/pmos.config` + `build/kernel/archlinux-surface-rt.config`.
`SRT_KBUILD=<n>` gives each build its own release string and modules directory. Keep it equal to `pkgrel` in
`build/repo/linux-surface-rt/PKGBUILD` (6 at the time of writing). Add `config` as the last argument to
regenerate `.config`.

## 4. U-Boot and the APX boot tools

```sh
docker run --rm -v $PWD:/work srt-cross build/u-boot/build.sh     # mainline U-Boot, surface-rt_defconfig
build/fusee/setup.sh      # builds T30_patch_irom.bin, makes build/fusee/venv (pyusb), copies U-Boot
```

Install the udev rule so your user can reach the tablet in APX mode. It must be numbered below 73, or the
`uaccess` ACL is never applied (**root**):

```sh
sudo install -m644 build/fusee/70-tegra-rcm.rules /etc/udev/rules.d/ && sudo udevadm control --reload
```

## 5. SD image

The SD image is Arch Linux ARM's armv7 tarball, customised inside an emulated armv7 container (about 15 min).

```sh
cd build/rootfs
mkdir -p dl out
curl -L -o dl/ArchLinuxARM-armv7-latest.tar.gz http://os.archlinuxarm.org/os/ArchLinuxARM-armv7-latest.tar.gz
curl -L -o dl/ArchLinuxARM-armv7-latest.tar.gz.md5 http://os.archlinuxarm.org/os/ArchLinuxARM-armv7-latest.tar.gz.md5
(cd dl && md5sum -c ArchLinuxARM-armv7-latest.tar.gz.md5)
docker import --platform linux/arm/v7 dl/ArchLinuxARM-armv7-latest.tar.gz srt-alarm-base
docker build --platform linux/arm/v7 -f Dockerfile.custom -t srt-alarm-custom .
cid=$(docker create --platform linux/arm/v7 srt-alarm-custom /bin/true); docker export $cid > out/rootfs.tar; docker rm $cid
cd ../..
docker run --rm -v $PWD:/work srt-cross build/rootfs/assemble.sh   # -> build/rootfs/out/surface-rt-arch.img
```

The image is GPT with one ext4 partition (`SRT_ROOT`, PARTUUID `5352543a-0000-4000-8000-000000000001`). It has
the kernel from step 3 in `/boot`, no initramfs, NetworkManager, bluez, PipeWire, Weston, sshd and zram. Logins
are Arch Linux ARM's defaults, **`alarm`/`alarm` and `root`/`root`: change both on first boot.**

Write it to the card (**root**). Use the `/dev/disk/by-id/` name of your card reader, and check it twice:

```sh
ls -l /dev/disk/by-id/ | grep usb
sudo dd if=build/rootfs/out/surface-rt-arch.img of=/dev/disk/by-id/<your-card-reader> bs=4M conv=fsync status=progress
```

## 6. First boot (APX, tethered)

1. Put the microSD into the Surface. Power it off fully: **hold Power about 10 s**. After a normal shutdown
   (Linux or Windows), APX mode doesn't enumerate on USB until you do this.
2. Hold **Volume Up** and press **Power**. The screen stays black: this is APX mode. Then plug the A-to-A cable
   into the PC. `lsusb -d 0955:7130` should show an NVIDIA device.
3. **Once per device:** `build/fusee/boot-apx.sh bct`. This reads your device's BCT with nvflash (read-only)
   into `build/fusee/surface-rt.bct`. That file is specific to your device: don't share it. Then hold Power
   ~10 s and enter APX again.
4. **Every cold boot:** `build/fusee/boot-apx.sh boot`. It sends the iROM patch, then U-Boot, into RAM. U-Boot
   boots `extlinux.conf` from the microSD. "bootloader failed NvError 0x0" at the end is expected: U-Boot
   doesn't answer nvflash.

In U-Boot, hold Volume Down for its boot menu (UMS, fastboot, RCM). **Never run `saveenv`**: U-Boot's
environment lives on the eMMC.

On the tablet: log in as `alarm`, change the passwords (`passwd`, `sudo passwd root`), and connect Wi-Fi with
`nmtui` (WPA2 only). Recommended NetworkManager setting for this chip:

```sh
printf '[device]\nwifi.scan-rand-mac-address=no\n' | sudo tee /etc/NetworkManager/conf.d/mwifiex.conf
```

Optional: set your timezone, locale and keyboard (`timedatectl set-timezone ...`, `localectl set-locale ...`,
`localectl set-keymap ...`, `localectl set-x11-keymap ...`).

From here on the guide assumes SSH works. Set `SURFACE=alarm@<tablet-ip>` on the PC: the helper scripts read
it, and the default is `alarm@surface-rt`.

## 7. mesa-grate and the KDE packages (PC)

```sh
build/mesa/build.sh        # ~15 min -> build/mesa/pkg/mesa-grate-26.2.3-25-armv7h.pkg.tar.xz
build/kde/build.sh purpose kirigami kwin plasma-nm plasma-desktop spectacle plasma-welcome \
                   plasma-mobile discover plasma-browser-integration flatpak-kcm
```

The KDE builds run each Arch PKGBUILD (pinned in `sources/arch-pkgbuilds/`) in an armv7h container. Our
per-package changes are applied from `build/kde/fixups/<name>.sh`. Logs go to `build/kde/logs/<name>.log`.
kwin takes about 55 min, plasma-desktop about 28 min, the rest 8-33 min each. `build/kde/ccache` speeds up
rebuilds.

Why these packages:

- **kwin** gets a GLES 2.0/grate patch.
- **kirigami** gets a cheaper icon shader.
- **plasma-desktop** drops a popup workaround that cost 0.5 s of CPU per launcher open.
- **plasma-nm, purpose, spectacle, plasma-welcome, plasma-mobile, discover, plasma-browser-integration and
  flatpak-kcm** are stale (5.27) or missing in Arch Linux ARM armv7h.
- **Never install `plasma-meta` from the repo**: it is 5.27.

## 8. Local [surface] repo on the tablet

```sh
build/repo/make-repo.sh    # PC: packages linux-surface-rt + surface-rt-tools, stages build/repo/surface/
ssh $SURFACE 'mkdir -p /tmp/pkgs/srt'
tar cf - -C build/repo surface setup-surface-repo.sh | ssh $SURFACE 'tar xf - -C /tmp/pkgs/srt'
scp build/kde/deploy/install-plasma.sh build/perf/surface-rt-cpufreq.conf $SURFACE:/tmp/pkgs/
```

On the tablet (**root**):

```sh
sudo sh /tmp/pkgs/srt/setup-surface-repo.sh   # [surface] above [core], kernel package, pacman hooks
sudo pacman -S mesa-grate                      # replaces stock mesa (answer y to the conflict)
```

`[surface]` sits above `[core]`, so our builds shadow Arch Linux ARM's packages of the same name.
`surface-rt-tools` adds pacman hooks:

- They refuse Qt/KDE Frameworks/Plasma upgrades that our packages weren't built for.
- They refuse swaps back to stock mesa or linux-armv7, and removal of the kernel package.
- They warn about missing libraries after upgrades.

Before every `pacman -Syu`, run `build/repo/check-updates.sh` on the PC. Details:
[build/repo/REBUILD.md](../build/repo/REBUILD.md).

Check the GPU: `eglinfo -B` (mesa-utils) should report vendor "Grate", renderer "Tegra" and OpenGL ES 2.0.

## 9. Plasma

On the tablet:

```sh
sudo bash /tmp/pkgs/install-plasma.sh   # repo packages + ours from [surface] (~370 packages, ~330 MB)
```

Settings that matter on this hardware:

- **Text shaders:** `QSG_DISTANCEFIELD_ANTIALIASING=gray` is set system-wide by surface-rt-tools
  (`/usr/lib/environment.d/50-surface-rt.conf`). Qt's subpixel text shaders are too long for the GPU.
- **SDDM:** Wayland greeter on our KWin with the plasma-keyboard on-screen keyboard
  (`/etc/sddm.conf.d/10-surface-rt.conf`, also from surface-rt-tools). Enable it with
  `sudo systemctl enable --now sddm`.
- **On-screen keyboard in the session:** in `~/.config/kwinrc` add
  ```ini
  [Wayland]
  InputMethod=/usr/share/applications/org.kde.plasma.keyboard.desktop
  ```
- **Rounded corners off** (KWin's rounded-corner shader is costly on this GPU): in `~/.config/breezerc` add
  ```ini
  [Common]
  RoundedCorners=false
  ```
- **CPU frequency response:** `sudo install -m644 /tmp/pkgs/surface-rt-cpufreq.conf /etc/tmpfiles.d/`. With it,
  ondemand reacts at 25 % load, so window moves stay near 60 FPS.
- Make sure `alarm` is in the `video` and `render` groups (`install-plasma.sh` checks this).

The first session can also be started by hand from a text console (Ctrl+Alt+F2) with
`build/kde/deploy/start-plasma-test.sh`. It logs to `~/plasma-<n>.log`. Run it with `NO_GL_OVERRIDE=1`,
because our KWin no longer needs the GL extension override.

## 10. Optional

- **Bluetooth:** [BLUETOOTH.md](BLUETOOTH.md).
- **Firefox:** works in software WebRender. `build/apps/firefox-user.js` (copied to your profile as `user.js`)
  cuts processes, animations and telemetry.
- **Kernel updates, Arch moving to a new Qt/Plasma, rollback:** [build/repo/REBUILD.md](../build/repo/REBUILD.md).
