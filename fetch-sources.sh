#!/bin/bash
# Fetch every upstream source tree this project builds from into sources/, pinned to the exact commits the
# published builds used, and apply the patches:
#   - third-party series (ThatzOkay/surface-rt-patches) are committed one patch per commit on a local
#     branch "surface-rt" (they have no From: header, so git am cannot be used);
#   - this project's own patches are applied on top and left uncommitted (git diff shows them).
# Re-running skips trees that already exist. No root needed. Shallow fetches: about 1.5 GB in total.
# Usage: ./fetch-sources.sh [--no-kde] [--dev]
#   --no-kde  skip the Arch KDE PKGBUILD repos (only needed to build the KDE packages)
#   --dev     also fetch kwin and kirigami source trees with our patches applied (for patch development;
#             the package builds don't need them, they download release tarballs)
set -euo pipefail
cd "$(dirname "$0")"
top=$PWD
S=$top/sources
mkdir -p "$S"
kde=1 dev=0
for a in "$@"; do case $a in --no-kde) kde=0 ;; --dev) dev=1 ;; *) echo "unknown option $a"; exit 2 ;; esac; done
GIT_ID=(-c user.name="fetch-sources" -c user.email="fetch-sources@localhost")

# fetch <dir> <url> <ref> <sha>: shallow checkout of <sha> (tag or branch <ref> as fallback), verified
fetch() {
  local dir=$S/$1 url=$2 ref=$3 sha=$4
  if [ -d "$dir/.git" ]; then echo "== $1: exists, skipped"; return 1; fi
  echo "== $1 ($ref, ${sha:0:12})"
  git init -q "$dir"
  git -C "$dir" remote add origin "$url"
  if ! git -C "$dir" fetch -q --depth 1 origin "$sha" 2>/dev/null; then
    git -C "$dir" fetch -q --depth 1 origin "$ref" 2>/dev/null || git -C "$dir" fetch -q --filter=blob:none origin "$ref"
    git -C "$dir" cat-file -e "$sha^{commit}" 2>/dev/null || git -C "$dir" fetch -q --filter=blob:none --unshallow origin "$ref"
  fi
  git -C "$dir" -c advice.detachedHead=false checkout -q "$sha"
  [ "$(git -C "$dir" rev-parse HEAD)" = "$sha" ] || { echo "$1: HEAD is not $sha"; exit 1; }
}
# series <dir> <patch>...: apply third-party patches as one commit each on branch surface-rt
series() {
  local dir=$S/$1; shift
  git -C "$dir" checkout -q -b surface-rt
  for p in "$@"; do
    patch -s -p1 -d "$dir" < "$p"
    git -C "$dir" add -A
    git "${GIT_ID[@]}" -C "$dir" commit -q -m "From surface-rt-patches: ${p#"$S"/surface-rt-patches/patches/}"
  done
  echo "   $# upstream patches committed on branch surface-rt"
}
# ours <dir> <patch>...: apply this project's patches, uncommitted
ours() {
  local dir=$S/$1; shift
  for p in "$@"; do patch -s -p1 -d "$dir" < "$p"; done
  echo "   $# project patches applied (uncommitted)"
}

# ThatzOkay's Surface RT patches (kernel, Mesa grate, Bluetooth firmware extractor) first: others need them
if fetch surface-rt-patches https://github.com/ThatzOkay/surface-rt-patches main \
     3da3813a03918f4516bb2d5bab52f255c71b6b3f; then
  ours surface-rt-patches "$top"/build/bluetooth/surface-rt-patches-extractor-rt81-recovery.patch
fi
P=$S/surface-rt-patches/patches

# Kernel: libre-tegra v7.0.1-stable + surface-rt-patches 0001, 0002, 0023-0029 + ours 0030-0035
if fetch libre-tegra_linux https://codeberg.org/libre-tegra/linux refs/tags/v7.0.1-stable \
     bfffc6d6656e87f3de3aa4c09d852849081ef088; then
  series libre-tegra_linux "$P"/0001-*.patch "$P"/0002-*.patch "$P"/002[3-9]-*.patch
  ours libre-tegra_linux "$top"/build/kernel/00[3-9][0-9]-*.patch
fi

# Mesa: libre-tegra grate-26.2.3 + surface-rt-patches mesa/0001-0019 + ours (build/mesa/grate-*.patch)
if fetch libre-tegra_mesa https://github.com/external-mirrors/libre-tegra-mesa grate-26.2.3 \
     2a7bf9cc5eb1b9ab07373c1814cd1320073af450; then
  series libre-tegra_mesa "$P"/mesa/00*.patch
  ours libre-tegra_mesa "$top"/build/mesa/grate-26.2.3-34.patch
fi

# U-Boot: mainline, unmodified (surface-rt_defconfig)
fetch mainline-u-boot https://source.denx.de/u-boot/u-boot.git main \
  709a97a05df66e394f40d249105d0baade46f107 || true

# Fusée Gelée tools (fusee-launcher submodule, iROM payload sources, NVIDIA nvflash binaries)
if fetch Open-Surface-RT_fusee-tools https://github.com/Open-Surface-RT/fusee-tools master \
     4633b5e313f890120e94dad90e763c51acbcbefb; then
  git -C "$S/Open-Surface-RT_fusee-tools" submodule -q update --init --depth 1
  [ "$(git -C "$S/Open-Surface-RT_fusee-tools/fusee-launcher-new" rev-parse HEAD)" = e3fbcbb2cc8d5d8ba52de2adfaf1c2fa97a5bc2f ] \
    || { echo "fusee-launcher-new: unexpected submodule commit"; exit 1; }
fi

# Smithay (Wayland compositor library) for rtshell (shell/, built with build/shell/build.sh): master at a
# pinned commit + ours (GLES2/grate fixes, X11 input focus for Xwayland)
if fetch smithay https://github.com/Smithay/smithay.git master \
     d09642ccba702c772f9a4837b78c63ce7f14229d; then
  ours smithay "$top"/build/shell/smithay-surface-rt.patch
fi

# Arch packaging repos for the KDE packages we rebuild (build/kde/build.sh <name>)
if [ $kde = 1 ]; then
  mkdir -p "$S/arch-pkgbuilds"
  while read -r n tag sha; do
    fetch "arch-pkgbuilds/$n" "https://gitlab.archlinux.org/archlinux/packaging/packages/$n.git" "refs/tags/$tag" "$sha" || true
  done <<'EOT'
discover 6.8.2-1 3efa2661dbcf7c3bcbf520c68546f53fd611dbc2
flatpak-kcm 6.8.2-1 e5d6ce867d6ad4f4005fe7cd021927c5aade732d
kirigami 6.31.0-1 700616543207c5773fa4665d5ddca2177c5f9a64
kwin 6.8.2-2 71fe8c5a51b18d504790f85a6547302b9071359e
plasma-browser-integration 6.8.2-1 05e00e2e2c8661bd64eb8bb72fea2f98fc308b83
plasma-desktop 6.8.2-1 1e6f666d3b9975f7309b2904140bd2c12e8d5dfa
plasma-mobile 6.8.2-1 9ddad656cd8caba312a85a44d5f7f95eb64fac67
plasma-nm 6.8.2-1 9e6c285427903c4c938426f3765dd68b62025b58
plasma-welcome 6.8.2-1 cfa81adcc3e901982c6cd8cbff02ffb8bceb4464
purpose 6.31.0-1 6543c21a9a5c53c3bffe9ece9a99897aa091b5ba
spectacle 1-6.8.2-1 13f14c4ccf2ddb27182f643f674760cd70fd3f65
EOT
fi

# KWin / Kirigami trees for patch development (build/kde/rebuild-kwin.sh, rebasing the fixup patches)
if [ $dev = 1 ]; then
  fetch kwin https://invent.kde.org/plasma/kwin.git refs/tags/v6.8.2 f130ab33c8e6f3b9d728aade87836086ae9a8ed4 \
    && { git -C "$S/kwin" checkout -q -b surface-rt; ours kwin "$top"/build/kde/fixups/kwin-surface-rt-gles2.patch; }
  fetch kirigami https://invent.kde.org/frameworks/kirigami.git refs/tags/v6.31.0 eecf183a956fdbfbf331c0ef61fd6e35220d4275 \
    && ours kirigami "$top"/build/kde/fixups/kirigami-icon-plain.patch
fi
echo "Done: sources/ is ready."
