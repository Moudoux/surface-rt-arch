# GPU: grate on Tegra 3

The Surface RT's GPU is the Tegra 3 "ULP GeForce": a GR3D 3D engine and a GR2D 2D engine behind host1x.
It is a **GLES 2.0-class** part:

- FP20 fragment precision.
- No general loops or dynamic branching in fragment programs.
- Short shader programs (63 EXEC slots per fragment program).
- 32 fragment uniform components.

None of this can be fixed in software.

## Stack

| Layer | What |
|---|---|
| Kernel | libre-tegra 7.0.1 grate DRM driver (classic Tegra DRM ioctls), plus our 0030 (atomic completed-jobs accounting) and 0032 (GPU reset path: fence refcount fix, 1 s job timeout) |
| Mesa | libre-tegra `grate-26.2.3` Gallium driver + ThatzOkay's series 0001-0019 + `build/mesa/grate-26.2.3-34.patch` (ours), packaged as `mesa-grate` (grate + softpipe/llvmpipe fallback, EGL/GBM/GLES1/2, GLX via libglvnd) |
| KWin | 6.8.2 + `build/kde/fixups/kwin-surface-rt-gles2.patch` |
| Qt Quick | stock Qt 6; `QSG_DISTANCEFIELD_ANTIALIASING=gray` (system-wide, from surface-rt-tools) |

## What our Mesa patch adds (on top of 0001-0019)

All of it lives in `src/gallium/drivers/grate`. The patch was written while profiling KWin and Plasma on the
device.

- **Draw batching:** several draws per GPU job, within the hardware limit of 64 relocations per job. The
  render target is relocated once per job.
- **Fragment compiler:**
  - a scheduler (`fp/sched.c`) and a preamble pass (`fp/preamble.c`);
  - denser programs: more pixels per pass, fewer EXEC slots;
  - folding the "Modulate" blend into the fragment stage.
- **Synchronisation:** draws drain at synchronisation boundaries. A drain that hits another client holding
  the GPU for more than 1 s is retried, instead of failing with "context drain failed: -110".
- **Performance counters** (`grate_perf.c`, off by default):
  - `GRATE_PERF=<seconds>`: periodic counters on stderr;
  - `GRATE_PERF_SYNC=1`: per-program GPU time;
  - `GRATE_PERF_VERBOSE=<n>`: large transfers, with backtraces.

Other environment variables, for debugging and experiments: `GRATE_DEBUG` (e.g. `tgsi`),
`GRATE_DRM_TRACE`, `GRATE_DRM_WAIT`, `GRATE_BATCH`, `GRATE_BATCH_DRAWS`, `GRATE_BATCH_US`,
`GRATE_BATCH_GPU_MS`, `GRATE_BLEND_FOLD`, `GRATE_FP_*` (compiler passes), `GRATE_DEBUG_BO`,
`GRATE_TEX_UNMAP`. Read the driver source before relying on any of them.

For a software comparison, run with `LIBGL_ALWAYS_SOFTWARE=1` (llvmpipe/softpipe come from the same package).

## KWin patch

- KWin 6.8 requires `GL_OES_texture_3D`, but it only needs it for ICC-profile LUTs. Without 3D textures the
  patch now degrades gracefully.
- The colour-management shader needs 3 mat4 plus parameters, more than the 32 fragment uniform components.
  When it can't be built, KWin falls back to the shader without the colour transform. That is exact for
  identity pipelines (SDR sRGB panel).
- The internal QPA window tries ARGB8888 before ABGR8888. grate can sample ABGR8888 but can't render to it,
  and KWin picks formats in a random order.

## Known gaps

- **Not visible in Plasma today:**
  - stencil (rotated clips);
  - RGB `SRC_ALPHA_SATURATE`;
  - a real `fwidth` (returns 0, so SDF edges are hard).
- **Won't fix:**
  - the glamor gradient shader (needs ~70 constants, the bank has 32, so it falls back to software);
  - KWin's full colour-management shader (far over the limits).
- **Browsers:** Chromium/Skia shaders exceed the limits, so Electron apps run with `--disable-gpu`.
  Firefox's hardware WebRender needs GLES 3.0, so it uses software WebRender. KWin still composites both on
  the GPU.

## Tools

- `build/gltest/`: fragment-shader conformance tests. Each test runs on grate and on Mesa's software renderer
  on the tablet, and the pixels are compared (`run.sh`). The directory also has fill-rate and FBO
  benchmarks, and shader corpora captured from KWin and Qt Quick (`captured/`, `kwin/`).
- `build/perf/`: window-move FPS scenarios, nested KWin runs, CPU-frequency tuning, and launcher
  (Kickoff) CPU profiling (`qmlprof/`).
- `build/kde/deploy/test-grate.sh`: a test session with shader dumping (`MESA_SHADER_DUMP_PATH`).
