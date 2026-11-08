# Development notes

## Where changes live

| Component | Source tree | Patch file in this repo | How the build uses it |
|---|---|---|---|
| Kernel | `sources/libre-tegra_linux` (branch `surface-rt` = upstream series; ours uncommitted) | `build/kernel/00NN-*.patch` | builds the working tree |
| Mesa | `sources/libre-tegra_mesa` (same layout) | `build/mesa/grate-26.2.3-<pkgrel>.patch` | builds the working tree (mounted read-only into the container) |
| KWin | `sources/kwin` (`--dev`) | `build/kde/fixups/kwin-surface-rt-gles2.patch` | the fixup adds the patch to Arch's PKGBUILD |
| Kirigami | `sources/kirigami` (`--dev`) | `build/kde/fixups/kirigami-icon-plain.patch` | same |
| plasma-desktop | n/a | `build/kde/fixups/plasma-desktop-no-popup-disable.patch` | same |

After editing a tree, export the patch again so the repo stays reproducible:

```sh
git -C sources/kwin diff > build/kde/fixups/kwin-surface-rt-gles2.patch
# Mesa has new files too: include them
( cd sources/libre-tegra_mesa && git add -N . && git diff ) > build/mesa/grate-26.2.3-<pkgrel>.patch
```

Also update the file name in `fetch-sources.sh` when the Mesa patch is renamed. For the kernel, keep one patch
per change (`00NN-...`), and check that they still apply in order to the `surface-rt` branch.

## Version rules

- **mesa-grate:** bump `pkgrel` in `build/mesa/PKGBUILD`, and the version in `build/kde/deploy/test-grate.sh`,
  before every build.
- **Kernel:** build with `SRT_KBUILD=<n>` and set `pkgrel=<n>` in `build/repo/linux-surface-rt/PKGBUILD`.
  `make-repo.sh` never rebuilds an existing package version.
- **KDE:** our pkgrel is Arch's pkgrel + `.N` (kwin 6.8.2-2 becomes 6.8.2-2.3). Set it with `SRT_REL=<N>
  build/kde/build.sh <pkg>`. Unpatched rebuilds keep Arch's pkgrel.

Deploying, updating and the "Arch moved to a new Qt/Plasma" procedure: [build/repo/REBUILD.md](../build/repo/REBUILD.md).

## Faster KWin iterations

`build/kde/rebuild-kwin.sh <pkgrel>` reuses a saved makepkg tree in `build/kde/work/kwin`. It copies the
changed files from `sources/kwin` and runs `makepkg -e`, which is much faster than a full 55-minute build.

## Testing on the tablet

- `build/kde/deploy/test-grate.sh`: install test packages from `/tmp/pkgs` and start a Plasma test session
  with shader dumping. Run it as `alarm` on the tablet's own console.
- `build/kde/deploy/shot.sh out.png`: a screenshot of the running KWin session, taken over SSH.
- `build/gltest/run.sh`: shader tests, comparing grate against software rendering. Everything runs in
  `/tmp/gt` on the tablet, with no install.
- `build/perf/`: `deploy.sh` copies the tools to `/tmp/perf` on the tablet. `realrun.sh` and `scenario.sh`
  measure window-move FPS. `qmlprof/` profiles the Kickoff open/close cost.

These scripts read `SURFACE` (default `alarm@surface-rt`) for the SSH target. Scripts that run on the tablet
assume the user `alarm` with uid 1000 (the Arch Linux ARM default).
