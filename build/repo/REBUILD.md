# Safe updates on the Surface RT: local `[surface]` repo

Our patched packages (kwin, kirigami, plasma-desktop, mesa-grate, the kernel, ...) live in a local pacman repo
on the Surface, `/srv/surface-repo`, listed in `/etc/pacman.conf` **above `[core]`**. pacman takes a package
from the first repo that has it, so Arch/ALARM versions of these names are ignored, even newer ones (tested
2026-11-07 with a fake kwin 6.8.9 in a second repo: `-Sup` kept 6.8.2-2.2).

That alone is not enough: KWin and Kirigami use Qt private API, and KWin/plasma-desktop must match the
Plasma release, but no Arch package pins those versions. So `surface-rt-tools` adds pacman hooks:

| Hook | When | Does |
|---|---|---|
| `surface-rt-hold.hook` | before install/upgrade of qt6-base, qt6-declarative, kcoreaddons, plasma-workspace, mesa, mesa-libgl, linux-armv7(-headers) | Aborts if the new pkgver differs from `/srv/surface-repo/compat`, or if mesa/linux-armv7 would replace ours. Nothing is changed. |
| `surface-rt-keep.hook` | before removing linux-surface-rt or mesa-grate | Aborts (removing the kernel package deletes `/boot/zImage`). |
| `surface-rt-ldd.hook` | after any upgrade/removal (~15 s) | Warns about missing shared libraries in our packages (optional ibus/scim/PackageKit ones ignored). |

Files in this directory:

| File | Where it runs | What |
|---|---|---|
| `check-updates.sh [host]` | PC, no root | Fetches the current ALARM dbs, prints compat ok/MOVED, our packages vs Arch, what `-Syu` would upgrade, and a verdict SAFE/WAIT. |
| `make-repo.sh` | PC | Builds `linux-surface-rt` (from `build/kernel/out`) and `surface-rt-tools` if that version doesn't exist yet, hard-links `build/kde/pkg` + `build/mesa/pkg`, makes `surface/` (db, compat, newest + one older file per package). |
| `compat` | | Qt/KF/Plasma pkgver our packages are built against. |
| `linux-surface-rt/` | | Kernel PKGBUILD + install script (fallback handling, depmod). pkgrel = kernel build number. |
| `surface-rt-tools/` | | Hooks, `hold-check`, `ldd-check`, `surface-repo-add` (root helper on the Surface). |
| `setup-surface-repo.sh` | Surface, `sudo sh`, once | Creates the repo, edits pacman.conf/extlinux.conf (with backups), installs both packages. |

## Updating the Surface (normal case)

1. On the PC: `build/repo/check-updates.sh`.
2. **SAFE:** on the Surface `sudo pacman -Syu`. If the hold hook still refuses (the repo moved between steps 1
   and 2), nothing was changed: go to "Arch moved".
3. **WAIT:** don't update; follow "Arch moved" first.
4. Watch for `surface-rt: WARNING ... not found` lines at the end of the transaction (ldd hook). They mean a
   library soname moved under one of our packages: rebuild that package.

## Deploying new builds of our packages

```sh
build/repo/make-repo.sh                                   # PC: stage build/repo/surface
tar cf - -C build/repo surface | ssh ${SURFACE:-alarm@surface-rt} 'rm -rf /tmp/pkgs/srt/surface; mkdir -p /tmp/pkgs/srt; tar xf - -C /tmp/pkgs/srt'
# Surface (root):
sudo surface-repo-add /tmp/pkgs/srt/surface/compat /tmp/pkgs/srt/surface/*.pkg.tar.*
sudo pacman -Syu                                          # installs the newer [surface] packages
```

Version rule: our pkgrel = Arch's pkgrel + `.N` (kwin 6.8.2-2 -> 6.8.2-2.3). The KDE fixups do this
themselves: `SRT_REL` (default per fixup: kirigami 1, kwin 3, plasma-desktop 1); bump it to rebuild the same Arch
version (`SRT_REL=4 build/kde/build.sh kwin`) and go back to 1 when Arch's pkgrel changes. Unpatched rebuilds
(plasma-nm, purpose, discover, ...) keep Arch's pkgrel; `[surface]` shadows the repo anyway.
Mesa: bump `pkgrel` in `build/mesa/PKGBUILD` and the version in `build/kde/deploy/test-grate.sh` (2 places).

Rollback: the previous file of each package stays in `/srv/surface-repo`: `sudo pacman -U /srv/surface-repo/<file>`.

## Arch moved (new Qt, KDE Frameworks or Plasma)

`check-updates.sh` says `MOVED` (or the hook refused). Until our packages are rebuilt, don't run `-Syu`; never
work around it with `--ignore` (a partial upgrade, unsupported on Arch).

1. **Decide first whether to move now.** The build image has to move to the new versions, which is the only time the
   `-Syu` layer of `build/kde/Dockerfile` is refreshed (normally it must never change). Don't edit the file;
   keep the old image and rebuild without cache:
   ```sh
   docker tag srt-kde-build srt-kde-build:plasma-6.8.2      # keep the old one
   docker build --no-cache --platform linux/arm/v7 -t srt-kde-build build/kde
   ```
   (`build.sh` runs `docker build` again; it is cached from then on.)
2. Update `sources/arch-pkgbuilds/<pkg>` to Arch's new PKGBUILDs (kirigami, kwin, plasma-desktop, plasma-nm,
   purpose, and the other KDE packages in `[surface]` for a Plasma release).
3. Check every fixup in `build/kde/fixups/` still applies. Version-specific bits to recheck:
   - `kirigami.sh` replaces Arch's sha256 for the 6.31.0 tarball (KDE re-rolled it); drop that for a new version.
   - `kwin.sh` anchors its patch after Arch's `c9d72079...patch` source line; if Arch drops that patch, anchor
     after the tarball line instead. Check the log's `grep -n` output lists our patch in source= and prepare().
   - `kwin-surface-rt-gles2.patch`, `kirigami-icon-plain.patch`, `plasma-desktop-no-popup-disable.patch`:
     rebase in `sources/kwin`, `sources/kirigami` (git diff, no commits) if they don't apply.
   - Is QTBUG-146992 / plasma-desktop's `enabled: dialog.visible` still there? If KDE changed it, re-measure the Kickoff open/close cost (`build/perf/qmlprof/`).
4. Build in dependency order: `build/kde/build.sh kirigami kwin plasma-desktop plasma-nm purpose` (+ the rest of
   `[surface]` for a Plasma release). Times: kwin ~55 min, plasma-desktop ~28 min (ccache should cut rebuilds).
5. mesa-grate only needs a rebuild if a library it links changes soname (libdrm, libelf, LLVM if used, wayland,
   X libs): the ldd hook or `ldd /usr/lib/libGLESv2.so*` tells.
6. Edit `build/repo/compat` to the new versions, `make-repo.sh`, deploy (above) with the new compat. Then `-Syu`
   upgrades Arch's packages and ours in one transaction and the hook passes.
7. Run the usual checks on the Surface (test-grate.sh session, realrun.sh, Kickoff, kernel log).

## Kernel updates

- Build: `docker run --rm -e SRT_KBUILD=<n> -v $PWD:/work srt-cross build/kernel/build.sh` with the next build
  number. With `SRT_KBUILD` the release is `7.0.1-surface-rt-<n>`, so each build has its own modules dir.
  (Builds 1-4 are `7.0.1-surface-rt+`.)
- Set `pkgrel=<n>` in `linux-surface-rt/PKGBUILD`, `make-repo.sh`, deploy, `sudo pacman -Syu`.
- The install script copies the running zImage to `/boot/zImage.old` and the dtb to `.dtb.fallback` (extlinux
  `fallback` entry); when the release string changes it keeps the old modules dir (one fallback kept). Then
  depmod. A new kernel still needs an APX boot from the PC.
- `build/kernel/deploy/install-kernel.sh` is the old manual path; don't use it any more (it overwrites
  package-owned files).
- Never `pacman -R linux-surface-rt` (the keep hook refuses).
