#!/bin/bash
# PC side, no root anywhere: is `pacman -Syu` on the Surface safe right now, and what needs a rebuild?
#  - fetches the current ALARM armv7h core/extra/alarm dbs into build/repo/.cache
#  - compat: Qt / KF / Plasma versions our [surface] packages were built for vs the repo
#    (a mismatch means the surface-rt-hold hook will block -Syu until we rebuild)
#  - each [surface] package vs Arch/ALARM's version of it (Arch moved -> rebuild with our fixup)
#  - if the Surface answers over SSH: what -Syu would upgrade (with [surface] first)
# Usage: build/repo/check-updates.sh [ssh-host]     (default $SURFACE or alarm@surface-rt; "-" to skip SSH)
set -euo pipefail
cd "$(dirname "$0")"
host=${1:-${SURFACE:-alarm@surface-rt}}
mirror=http://mirror.archlinuxarm.org/armv7h
mkdir -p .cache
for r in core extra alarm; do
  curl -sfL -z .cache/$r.db -o .cache/$r.db "$mirror/$r/$r.db" || { echo "download of $r.db failed"; exit 1; }
done
# "name version repo" for every package, repos in pacman.conf order ([surface] first)
list() { bsdtar -tf "$1" | sed -n 's|^\(.*\)-\([^-/]*-[^-/]*\)/$|\1 \2|p' | sed "s/\$/ $2/"; }
{ list surface/surface.db.tar.gz surface; for r in core extra alarm; do list .cache/$r.db $r; done; } > .cache/all.txt
first() { awk -v n="$1" '$1==n && $3!="surface" {print $2, $3; exit}' .cache/all.txt; }  # Arch/ALARM's copy
pkgver_of() { echo "${1%-*}"; }
blocked=0

echo "== compat (built against) vs repo"
while IFS== read -r n want; do
  [[ $n =~ ^[a-z] ]] || continue
  read -r av ar < <(first "$n") || true
  if [ -z "${av:-}" ]; then echo "  $n: not in the repo?"; continue; fi
  if [ "$(pkgver_of "$av")" = "$want" ]; then echo "  ok      $n $want"
  else echo "  MOVED   $n $want -> $av ($ar)"; blocked=1; fi
done < surface/compat

echo "== our [surface] packages vs Arch/ALARM"
awk '$3=="surface"{print $1, $2}' .cache/all.txt | while read -r n ours; do
  base=$ours; [[ ${ours##*-} == *.* ]] && base=${ours%.*}     # 6.8.2-2.2 -> 6.8.2-2
  lookup=$n; [ "$n" = mesa-grate ] && lookup=mesa
  read -r av ar < <(first "$lookup") || { av=; ar=; }
  if [ -z "$av" ]; then printf "  %-28s %-14s only ours\n" "$n" "$ours"; continue; fi
  if [ "$n" = mesa-grate ]; then printf "  %-28s %-14s upstream mesa %s (%s), info only\n" "$n" "$ours" "$av" "$ar"; continue; fi
  c=$(vercmp "$av" "$base")
  if [ "$c" -gt 0 ]; then printf "  %-28s %-14s REBUILD: Arch has %s (%s)\n" "$n" "$ours" "$av" "$ar"
  elif [ "$c" -lt 0 ]; then printf "  %-28s %-14s ahead of %s %s\n" "$n" "$ours" "$ar" "$av"
  else printf "  %-28s %-14s same base as %s\n" "$n" "$ours" "$ar"; fi
done

if [ "$host" != - ] && inst=$(ssh -o ConnectTimeout=5 "$host" pacman -Q 2>/dev/null); then
  echo "== what pacman -Syu would upgrade on the Surface (repo dbs as fetched now)"
  awk 'NR==FNR { if (!($1 in v)) v[$1]=$2" "$3; next } ($1 in v) { split(v[$1], a, " "); if (a[1] != $2) print $1, $2, a[1], a[2] }' \
    .cache/all.txt <(echo "$inst") | while read -r n have new r; do
      [ "$(vercmp "$new" "$have")" -gt 0 ] && echo "  $n $have -> $new ($r)"; done | tee .cache/pending.txt
  echo "  $(wc -l < .cache/pending.txt) package(s)"
else
  echo "== (Surface not reached: skipped the pending-upgrade list)"
fi

echo
if [ $blocked = 1 ]; then
  echo "VERDICT: WAIT. Qt/KF/Plasma moved; the hook will refuse -Syu. Rebuild per REBUILD.md, then deploy."
else
  echo "VERDICT: SAFE. pacman -Syu keeps our packages (REBUILD lines above are optional pkgrel catch-ups)."
fi
