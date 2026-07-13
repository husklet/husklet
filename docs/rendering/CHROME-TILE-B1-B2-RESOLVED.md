# Multi-process Chrome white content: RESOLVED B2 (renderer dormant), not B1 (tile-bridge)

Live-resolved 2026-07-12 on current `main` (worktree at `2988fc03` + the committed
`DD_TILE_TRACE` C-shim instrumentation). This closes the fork the content-tile PIN
(`CHROME-CONTENT-TILE-PIN.md` §5) left open: **B1** (the renderer rasters a content tile
into an external/cross-process buffer that dd's GL shim can't sample, so it is dropped at
the `!g_tex[tu].data` gate — a *tile-bridge* fix) **vs. B2** (the renderer never rasters —
no content is produced — an *upstream Mojo* fix). The task premise asserted B1; the live
trace **falsifies it. The verdict is B2.**

## How it was resolved (the deployed C shim, live, GPU compositing)

The PIN analyzed `dd-shim-gl` (the Rust shim, behind `DD_SHIM_IMPL=rust`, not default).
Chrome uses the **deployed C shim** `dd-tests/guests/gl_shim.c`. The same
`!g_tex[tu].data` drop gate exists there (`gl_shim.c:2037,2047,2052`). So the trace was
added to the C shim: `tiletrace_report()` (flag-gated `DD_TILE_TRACE`, pure read) replays
`eglSwapBuffers`' exact sampler-bound-texture selection and reports, per frame, how many
sampled textures are backed by CPU pixels vs. are **used-but-EMPTY** (`used==1,
data==NULL` — an external/EGLImage-backed tile the shim never received pixels for), plus
`offscreen_fbo_passes` (in-frame content raster passes).

Run: current-main engine + `ddcli` + `dd-display`, **default multi-process GPU
compositing** (`--in-process-gpu --use-gl=angle --use-angle=gl-egl`, **no**
`--single-process`, **no** `CHROME_SW`), offline blue page
`data:text/html,<body style='margin:0;background:#1a73e8;height:100vh'>`,
`CHROME_WINDOW_SIZE=512,384`, bounded. The instrumented shim was cross-built musl-aarch64
(NEEDED `libc.musl-aarch64.so.1`, byte-ABI-parity undefined-symbol subset of the deployed
shim) and staged into `~/.dd/gui/aarch64/lib.musl-chrome` for the run, then restored.

## Result: B2. Zero external tiles ever reach the shim.

956 swapped frames, chromium `EXIT=0`. Across **all 956** `[tiletrace]` frames:

```
[tiletrace] pid=42232 frame=1   ... offscreen_fbo_passes=0 sampled_with_data=18 sampled_EMPTY=0 sampled_unbound=0 max_data=512x256 max_empty=0x0
[tiletrace] pid=42232 frame=956 ... offscreen_fbo_passes=0 sampled_with_data=9  sampled_EMPTY=0 sampled_unbound=0 max_data=512x256 max_empty=0x0
```

- **`sampled_EMPTY = 0` in every frame** (max over 956 = 0); **0** `SAMPLED-EMPTY tile`
  lines. No used-but-empty / external-backed sampler texture was EVER bound.
- **`offscreen_fbo_passes = 0` in every frame.** No in-frame content raster pass.
- The only sampled textures are **512×256 UI glyph atlases, all with CPU data**
  (`sampled_with_data`, `max_data=512x256`).
- **0** `eglCreateImage` / `eglBindTexImage` / `glEGLImageTargetTexture2DOES` calls —
  Chrome/ANGLE never even attempted the external-image import path. (The PIN's "import
  verbs are stubbed" concern is therefore moot here — ANGLE does not take that path.)
- The content region is an affirmatively-composited **solid white placeholder** every
  frame (957× `record_clear ... rect=16,32 480x270 color=1,1,1,1` — a SolidColorDrawQuad),
  matching `README.md` §3.2's `ClearRect{texture:1, 16,82 480x270, white}`.
- Rendered frame (`tile-b1b2-evidence/multiproc-tiletrace-white-content.png`): UI (title
  bar with the data: URL, infobar, window buttons) renders sharp; content region
  **blue = 0.0%, white = 91.6%**, centre pixel `(255,255,255)`. `Network service crashed`
  ×1 (the upstream Mojo bring-up failure signature).

## Why this refutes B1 and confirms B2

B1 requires the renderer to actually raster a content tile that then reaches the GPU/viz
compositor as an external buffer the shim drops. If that were happening, the shim would
bind a content-sized **used-but-empty** texture (`sampled_EMPTY>0`) and/or take an
`eglCreateImage`/EGLImage import path and/or record an offscreen raster pass. **None of
those occur — in any of 956 frames.** There are no external tiles to drop; the
`!g_tex[tu].data` gate never fires on a content tile because no content tile exists. The
white is not a *dropped* tile — it is a placeholder viz composites because the renderer's
content frame never arrives.

This is the shim-layer confirmation of the same B2 already pinned two other ways on current
main: `README.md` §3.2 (GPU path — renderer 100% parked, 0/2752 IR captures carry content)
and `WL_SHM-MULTIPROC-CONTENT-FINDINGS.md` (CPU/wl_shm path — renderer's primordial IPC
channel never connects; viz gets zero BeginFrame/CompositorFrame; only one `wl_surface`).
Three independent vantage points (GL→IR, wl_shm CompositorFrameSink, and now the shim's
own sampler-backing census) agree: **the renderer is dormant pre-raster.**

## Why the gl_shim.c / dd-display buffer bridge will NOT help

The PIN's proposed fix (add an external-handle `Texture` variant, implement
`eglCreateImage` + `glEGLImageTargetTexture2DOES`, advertise `GL_OES_EGL_image`, stop
dropping externally-backed tiles, emit an "bind = existing IOSurface id" IR) makes an
externally-backed tile sampleable. But there is **no externally-backed tile** in this path —
`sampled_EMPTY=0`, zero EGLImage import attempts, zero offscreen passes. Building the bridge
would add machinery nothing exercises; the content would stay white because the bytes never
get produced or handed to viz. This is precisely the STEP-2b "don't build the bridge blind"
case.

## The actual fix (upstream, engine — out of scope for a tile/dd-display change)

Per `WL_SHM-MULTIPROC-CONTENT-FINDINGS.md` §Fix-plan: the blocker is the child's Mojo
node-connect completion — highest-probability gap is an **SCM_RIGHTS-received socket** (the
`AcceptBrokerClient` broker channel) whose readiness is never re-armed on the child's
kqueue/epoll after the handle lands. Existing `xproc-inbound`/`zygote-inbound`/`scm-futex`
micro-gates pass but none arms epoll on an *SCM_RIGHTS-received socket* and asserts a
post-registration write wakes it. Build that micro-gate (`dd-tests/guests/ext_ipc/`); if it
reproduces, fix the engine's readiness-prime for received sockets
(`dd-jit-darwin/.../syscall/event.c`). The Chrome-side node-connect handshake is
DVLOG-stripped in this release build, so drive the diagnosis from the gate + syscall traces,
not Chrome VLOGs. Interim working content path: `--single-process` (`README.md` §3.1).

## Reproduce

Instrumentation: `dd-tests/guests/gl_shim.c` `tiletrace_report()` (commit in this branch),
gated on `DD_TILE_TRACE=1`. Cross-build musl-aarch64 (`aarch64-unknown-linux-musl-cc -O2
-fPIC -shared -Wl,-soname,libEGL.so.1`, then `patchelf --replace-needed libc.so
libc.musl-aarch64.so.1`), stage as `libEGL.so.1`/`libGLESv2.so.2`/`libwayland-egl.so.1` in
`~/.dd/gui/aarch64/lib.musl-chrome`, and drive `run_chrome_gpu_bounded.sh` (add
`DD_TILE_TRACE` to its env-forward list) with the default multi-process GPU config above.
Read the browser/GPU process stderr in `launch.log` for `[tiletrace]`: `sampled_EMPTY>0` /
`offscreen_fbo_passes>0` = B1; all-zero = B2.
