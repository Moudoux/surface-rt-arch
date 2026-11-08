#!/bin/bash
# Runs inside srt-cross (no root needed). Builds out/surface-rt-arch.img from rootfs.tar + kernel.
set -euo pipefail
B=/work/build
OUT=$B/rootfs/out
K=$B/kernel/out
IMG_SIZE_MIB=${IMG_SIZE_MIB:-8192}
ROOT_UUID=5352543a-0000-4000-8000-000000000001
mkdir -p "$OUT"
rm -f "$OUT/root.ext4" "$OUT/surface-rt-arch.img"
# Extra files layered into the tar: kernel, dtbs, modules, overlay
STAGE=$(mktemp -d)
mkdir -p "$STAGE/boot/dtbs" "$STAGE/usr/lib/modules"
cp "$K/arch/arm/boot/zImage" "$STAGE/boot/"
cp "$K"/arch/arm/boot/dts/nvidia/tegra30-microsoft-surface-rt*.dtb "$STAGE/boot/dtbs/"
cp -a "$K"/staging/lib/modules/* "$STAGE/usr/lib/modules/"
rm -f "$STAGE"/usr/lib/modules/*/build "$STAGE"/usr/lib/modules/*/source
cp -a "$B/rootfs/overlay/." "$STAGE/"
# Merge: base tar first, then our files owned by root (bsdtar rewrites uid/gid)
bsdtar -cf "$OUT/extra.tar" --uid 0 --gid 0 -C "$STAGE" .
# Drop Docker artifacts and any base file that the overlay replaces, so the tar has no duplicates
( cd "$STAGE" && find . \( -type f -o -type l \) ) > "$OUT/exclude.lst"
echo ./.dockerenv >> "$OUT/exclude.lst"
bsdtar -cf "$OUT/full.tar" -X "$OUT/exclude.lst" @"$OUT/rootfs.tar"
bsdtar -rf "$OUT/full.tar" @"$OUT/extra.tar"
rm -rf "$STAGE" "$OUT/exclude.lst"
PART_MIB=$((IMG_SIZE_MIB - 2))
mke2fs -q -t ext4 -L SRT_ROOT -d "$OUT/full.tar" "$OUT/root.ext4" "${PART_MIB}M"
truncate -s "${IMG_SIZE_MIB}M" "$OUT/surface-rt-arch.img"
sfdisk -q "$OUT/surface-rt-arch.img" <<EOT
label: gpt
first-lba: 2048
start=2048, size=$((PART_MIB*2048)), type=69DAD710-2CE4-4E3C-B16C-21A1D49ABED3, uuid=$ROOT_UUID, name=root, attrs=LegacyBIOSBootable
EOT
dd if="$OUT/root.ext4" of="$OUT/surface-rt-arch.img" bs=1M seek=1 conv=notrunc,sparse status=none
rm -f "$OUT/root.ext4" "$OUT/extra.tar" "$OUT/full.tar"
sfdisk -l "$OUT/surface-rt-arch.img"
ls -la "$OUT"
