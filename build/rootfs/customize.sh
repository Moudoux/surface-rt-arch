#!/bin/bash
# Runs as root INSIDE the linux/arm/v7 ALARM container (qemu). Never on the host.
set -euxo pipefail
pacman-key --init
pacman-key --populate archlinuxarm
# qemu-user: pacman's Landlock sandbox and disk-space check misbehave under emulation
sed -i 's/^#\?DisableSandbox.*/DisableSandbox/; s/^CheckSpace/#CheckSpace/' /etc/pacman.conf
grep -q '^DisableSandbox' /etc/pacman.conf || sed -i '/^\[options\]/a DisableSandbox' /etc/pacman.conf
pacman -Syu --noconfirm
pacman -Rns --noconfirm linux-armv7 || true
rm -rf /boot/* /usr/lib/modules/*
pacman -S --noconfirm --needed \
  sudo networkmanager bluez bluez-utils iw wireless-regdb \
  linux-firmware linux-firmware-marvell \
  alsa-utils alsa-ucm-conf pipewire pipewire-pulse pipewire-alsa wireplumber \
  zram-generator dosfstools e2fsprogs \
  weston foot mesa mesa-utils libdrm \
  vim htop less man-db bash-completion openssh
# system config
echo surface-rt > /etc/hostname
sed -i 's/^#en_US.UTF-8/en_US.UTF-8/' /etc/locale.gen && locale-gen
echo LANG=en_US.UTF-8 > /etc/locale.conf
ln -sf /usr/share/zoneinfo/UTC /etc/localtime
echo '%wheel ALL=(ALL:ALL) ALL' > /etc/sudoers.d/10-wheel && chmod 440 /etc/sudoers.d/10-wheel
systemctl enable NetworkManager bluetooth sshd systemd-timesyncd
systemctl disable systemd-networkd systemd-resolved 2>/dev/null || true
# keep pacman from installing the generic kernel later
sed -i 's/^#IgnorePkg.*/IgnorePkg = linux-armv7 linux-armv7-headers/' /etc/pacman.conf
# restore normal pacman behaviour for the real device
sed -i 's/^DisableSandbox$//; s/^#CheckSpace/CheckSpace/' /etc/pacman.conf
pacman -Scc --noconfirm
rm -f /etc/machine-id && touch /etc/machine-id
