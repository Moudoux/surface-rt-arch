# armv7h (qemu-user) build image for KDE packages. Same base as build/mesa.
FROM srt-alarm-custom:latest
RUN sed -i '/^\[options\]/a DisableSandbox' /etc/pacman.conf && sed -i 's/^CheckSpace/#CheckSpace/' /etc/pacman.conf \
 && pacman -Syu --noconfirm --needed base-devel cmake extra-cmake-modules ninja git gnupg \
 && pacman -Scc --noconfirm \
 && echo 'alarm ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/builder
# GNU ld on 32-bit ARM rejects -z pack-relative-relocs (RELR), which ALARM's makepkg.conf sets
RUN sed -i 's/ *-Wl,-z,pack-relative-relocs//' /etc/makepkg.conf && ! grep -q pack-relative /etc/makepkg.conf
# ccache (build/kde/ccache, mounted at /ccache by build.sh). Separate layer, no -y: editing the -Syu layer
# above would rebuild it and upgrade the whole image past the Qt/KF versions on the Surface.
RUN pacman -S --noconfirm --needed ccache && pacman -Scc --noconfirm \
 && sed -i 's/^BUILDENV=\(.*\)!ccache/BUILDENV=\1ccache/' /etc/makepkg.conf && grep -q '^BUILDENV=.* ccache' /etc/makepkg.conf
USER alarm
WORKDIR /pkg
