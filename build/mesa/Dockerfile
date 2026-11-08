FROM srt-alarm-custom:latest
RUN sed -i '/^\[options\]/a DisableSandbox' /etc/pacman.conf && sed -i 's/^CheckSpace/#CheckSpace/' /etc/pacman.conf \
 && pacman -Sy --noconfirm --needed base-devel meson ninja python-mako python-packaging python-yaml python-pycparser \
      glslang wayland-protocols xorgproto llvm elfutils libunwind lm_sensors libxrandr libxshmfence libxxf86vm libglvnd \
 && pacman -Scc --noconfirm \
 && echo 'alarm ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/builder
USER alarm
WORKDIR /pkg
