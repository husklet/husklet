# Chrome multi-process content-tile PIN (default GPU compositing)

Branch: `worktree-agent-ae49c18632f5a9421` (reset to `main` = `f95d2d7c`).
Scope: pin the EXACT place multi-process Chrome's web content is lost in the DEFAULT GPU-compositing
path (NOT `--single-process`, NOT `CHROME_SW`). This is a code-grounded PIN plus committed, flag-gated
instrumentation (`DD_TILE_TRACE`) the live Mac run uses to confirm the disambiguating branch. It does
NOT re-litigate the Mojo node-connect question (per the task's reconciled premise: in this mode the
renderer connects and pumps messages; the break is the renderer→viz content-tile buffer path).

> Environment note: this analysis was produced in the Linux worktree container. `dd-jit-darwin` and
> `dd-display`'s Metal backend require the macOS host, so the live Chrome-under-dd run cannot execute
> here. The instrumentation is committed (build-clean, parity tests green) so it runs on the Mac
> exactly as `docs/rendering/README.md §4` prescribes.

---

## 1. The mechanism, in one sentence

**dd's GL shim has no representation for a texture whose pixels live in an external/cross-process
buffer.** A texture is sampleable only if the shim itself holds its RGBA bytes — either uploaded via
`glTexImage2D`/`glTexSubImage2D` (populating `Texture::data`) or filled by an in-frame offscreen FBO
render pass the shim records. A Chrome multi-process content tile is *neither*: the renderer rasters it
into a SharedImage / GpuMemoryBuffer and hands viz a **handle/mailbox**, not CPU pixels and not an
in-process GL draw. So at composite time the tile texture has empty `data`, the frame lowering
**silently drops it from the bind group**, and viz's content quad samples an unbacked (zero → white)
texture. The content region is therefore white.

## 2. The load-bearing code (why the tile is dropped)

The frame lowering in `dd-shim-gl/src/frame.rs` gates every *sampler-bound* texture on it having CPU
pixel data:

- `build_replay_frame` (the multi-draw / compositor path), `frame.rs:416-424`:
  ```rust
  for i in 0..dpr.samp_names.len().min(4) {
      let unit = ...; let tu = d.tex_units[unit];
      if (tu as usize) < MAXTEX && s.tex[tu as usize].used
         && !s.tex[tu as usize].data.is_empty()   // <-- EMPTY texture is excluded
         && !texlist.contains(&tu) { texlist.push(tu); }
  }
  ```
- `build_single_draw_frame`, `frame.rs:190`: same `!s.tex[tu].data.is_empty()` gate.

A texture not in `texlist` gets no `CreateTexture`/`CopyBufferToTexture` and no `BindGroup` entry
(`frame.rs:286-296, 578-582`). `Texture` itself (`dd-shim-gl/src/state.rs:103-116`) has **only** a
`data: Vec<u8>` (RGBA8) — there is **no** `egl_image` / external-handle / dmabuf field. `Texture::data`
is written **only** by `tex_store_pixels` (`state.rs:838`), i.e. only by `glTexImage2D`/`glTexSubImage2D`
(`gles.rs:433,463`). So a texture the shim did not see CPU-uploaded is, by construction, empty and
unsampleable.

The staging upload (`lower.rs:103, texture_staging_cmds`) copies `t.data`; an empty `data` means an
empty upload. The result on the host/Metal side is an unwritten texture → white/zero content quad.

## 3. Why there is no buffer bridge to populate that tile

The only cross-process image-import entry points Chrome would use to back a tile texture with an
external buffer are **absent or stubbed** in the shim:

- `eglCreateImage` / `eglCreateImageKHR` — present in `registry/gles2_egl.manifest:372` but **NOT** in
  `build.rs`'s `IMPLEMENTED` set → emitted as a generated **no-op stub** returning a null `EGLImage`
  (`build.rs:74-96`; logs `unimplemented entry point: eglCreateImage` under `DD_SHIM_DEBUG`).
- `eglBindTexImage` — manifest `:367`, also a no-op stub.
- `glEGLImageTargetTexture2DOES` / `glEGLImageTargetRenderbufferStorageOES` — **not in the manifest at
  all**, so not even exported; `eglGetProcAddress`→`dlsym`→null.
- The advertised GL extensions are only `GL_OES_element_index_uint GL_OES_texture_npot`
  (`gles.rs:58`) — **no** `GL_OES_EGL_image`, no `GL_EXT_texture_format_BGRA8888`. ANGLE therefore
  sees no EGLImage/native-buffer texture support.

The **only** buffer dd allocates or imports on the client side is the single default window surface,
via `renderd::alloc` → `DD_IOCTL_GPU_ALLOC` (`transport.rs:79`, called once from
`eglCreateWindowSurface`, `egl.rs:485`). That one surface is submitted to the GPU-exec service by
`surface.id` (`transport.rs:147 submit`) and committed to dd-display as one IOSurface/dma-buf. There
is **no per-tile content-buffer import** anywhere in the client path.

On the compositor side, `dd-display/src/server.rs` imports whole `wl_surface`s (wl_shm pools or
`zwp_linux_dmabuf` whose modifier low bits carry an IOSurface id, `server.rs:61-64,268-281`). Per
`WL_SHM-MULTIPROC-CONTENT-FINDINGS.md`, only **one** `wl_surface` (the toplevel) is ever produced — the
content is composited by viz *into* that toplevel via GL/IR, never delivered as a separate buffer. So
dd-display faithfully presents whatever the toplevel IR contains; it is not where content is lost.

**Net:** the content tile is lost in the GL shim's frame lowering — an externally-backed tile texture
has no `data`, no import path can give it any, and the `!data.is_empty()` gate drops it.

## 4. The single-vs-multi structural diff (this diff IS the bug)

| | tile backing the shim sees | why it works / fails |
|---|---|---|
| **`--single-process`** (renders content, README §3.1) | renderer + raster + composite are ONE process and ONE GL context. Content is OOP-rastered into an **offscreen FBO tile texture by GL commands the shim records directly** → the tile is an in-frame render target (`target_tex`) → IR carries `Begin target=512/514` with the page-bg clear. | The shim *has* the tile's contents because it watched the raster happen in-process. No external buffer to import. |
| **multi-process, default GPU compositing** (white, README §3.2) | the renderer rasters into a **SharedImage / GpuMemoryBuffer** and ships viz a **handle/mailbox**; viz composites by sampling that texture. dd's shim has no import for an external buffer and no cross-context/cross-process texture aliasing → the tile texture has empty `data`. | The `!data.is_empty()` gate drops the tile → content quad samples zero → white. IR contains only `Begin target=1` + the white `ClearRect{texture:1, 16,82 480x270}` placeholder (README §3.2), never `Begin target=512/514`. |

The structural difference — *in single-process the tile is a shim-visible in-frame FBO render target;
in multi-process the tile is an external cross-process buffer the shim can neither import nor alias* —
is exactly the bug.

## 5. The one remaining fork the live trace must resolve (Q(a)/(b)/(c))

From code alone, two sub-mechanisms both produce "white content, only `target=1` in the IR", and they
have different fixes. `DD_TILE_TRACE` (below) resolves which, on the next live Mac run:

- **Fork B1 — external-buffer tile (buffer bridge).** viz issues a content DrawQuad that *samples* a
  texture with empty `data`. `DD_TILE_TRACE` prints `sampled_EMPTY=N>0` with `SAMPLED-EMPTY tile:
  glTex=… WxH data=0`. Correlates with `DD_SHIM_DEBUG` `unimplemented entry point: eglCreateImage /
  eglBindTexImage`. → The tile lives in a SharedImage/GpuMemoryBuffer the shim never received pixels
  for. **Fix layer: the GLES shim's buffer bridge** (§6).
- **Fork B2 — renderer produced nothing (upstream Mojo).** viz never issues a content textured quad at
  all; the only content-region op is the solid white `ClearRect` (a SolidColorDrawQuad placeholder for
  an unready tile). `DD_TILE_TRACE` prints `sampled_EMPTY=0` and `offscreen_fbo_passes=0` and no
  SAMPLED tile lines for the content region. → matches the "renderer 100% dormant" samples in
  `WALL7-FINDINGS.md`/README §3.2; the fix is upstream (Mojo node-connect completion), not the tile
  path. The task's premise asserts B1; the trace is what makes that assertion falsifiable.

Either way the trace also answers the task's explicit questions:
- **Q(a) does the shim see the renderer's raster?** → `offscreen_fbo_passes` count. `>0` = yes (GL
  raster into tiles reached the shim, as in single-process); `0` = no in-process tile raster.
- **Q(b) how is each tile backed/shared?** → a content tile that appears as `sampled_with_data` was
  CPU-uploaded (`glTexImage2D` — shared-memory/bitmap path); one that appears as `SAMPLED-EMPTY` is an
  external SharedImage/GpuMemoryBuffer handle with no shim-side pixels.
- **Q(c) what does viz sample, and why empty?** → the `SAMPLED-EMPTY`/`sampled` lines name the exact GL
  texture id + dims viz's content quad binds and whether it was written.

## 6. Fix location + plan

**Primary fix layer: the GLES shim's cross-process buffer bridge (`dd-shim-gl`).** Give `Texture` an
external-backing variant and wire the import verbs so an externally-filled tile becomes sampleable:

1. Add an external-handle field to `state::Texture` (e.g. `external: Option<ExternalImage>` carrying
   the IOSurface/dma-buf/mailbox id + dims), and implement `eglCreateImage` + a real
   `glEGLImageTargetTexture2DOES` export (add to the manifest) that records the binding
   `texture ← EGLImage(handle)`.
2. In `frame.rs` lowering, treat an externally-backed texture as *present* (do NOT drop it on the
   `!data.is_empty()` gate); emit a bind entry that references the imported buffer by id instead of a
   staged CPU upload.
3. Advertise `GL_OES_EGL_image` (and, if Chrome probes it, the dma-buf import extension) in
   `gles.rs`'s `GL_EXTENSIONS`, and add the matching `EGL_KHR_image_base` to the display-extension
   string in `egl.rs` so ANGLE takes the EGLImage path.
4. **dd-display / GPU-exec side (secondary):** ensure the IR gains a "bind texture = existing
   IOSurface/dma-buf id" resource so the host Metal backend samples the already-rendered buffer
   (`dd-display/src/server.rs` already understands the IOSurface-id-in-modifier convention,
   `server.rs:61-64` — reuse it for per-tile content buffers).
5. **Engine (only if the trace shows a native GpuMemoryBuffer/gbm allocation with a real fd):** back
   the tile allocation so the handle Chrome ships is a real importable IOSurface. The audit's "0 PRIME
   calls" says gbm is not used today, so this is likely unnecessary — the trace confirms.

If the live trace instead lands on **Fork B2** (`sampled_EMPTY=0`, no content quad), the tile bridge is
moot and the fix is upstream Mojo node-connect completion (`WALL7-FINDINGS.md §5.1`) — that is a
different workstream and out of scope for a tile-path fix.

## 7. The instrumentation (committed, flag-gated, no behavior change)

- `dd-shim-gl/src/tiletrace.rs` (new module) — replays `frame.rs`'s exact tile-selection logic against
  the swap-time `GlState` and, only when `DD_TILE_TRACE` is set, prints per frame:
  `draws / clears / default_pass / offscreen_fbo_passes / sampled_with_data / sampled_EMPTY`, then a
  line per sampled tile (with GL id + WxH + data length) flagging the EMPTY ones. If
  `DD_TEXTURE_DUMP_DIR` is set it also writes a per-frame texture manifest and the raw RGBA of each
  non-empty sampled tile.
- Two one-line hooks: `pub mod tiletrace;` in `lib.rs`, and `crate::tiletrace::trace_frame(s);` at the
  top of `build_frame_ir` in `frame.rs` (post-guard). No refactor of sibling-owned code.
- Verified: `cargo build -p dd-shim-gl` clean; `cargo test -p dd-shim-gl` all green (byte-parity
  preserved — `trace_frame` is a pure read and a no-op unless the env var is set).

### How to run it (on the Mac, serialized — see README §4)
```
# default multi-process GPU compositing, offline blue page:
DD_SHIM_DEBUG=1 DD_TILE_TRACE=1 DD_TEXTURE_DUMP_DIR=<dir> \
  CHROME_WINDOW_SIZE=512,384 CHROME_APP_URL="data:text/html,<body style='margin:0;background:#1a73e8;height:100vh'>" \
  CHROME_KEEP_STATE=0 CHROME_DISPLAY_START_DELAY=12 \
  target-chrome-codex/run_chrome_gpu_bounded.sh   # DDISP/DDJIT_DIR/DDCLI overridden to this build
```
Then read the GPU/viz process stderr for `[tiletrace]` lines. `sampled_EMPTY>0` with a content-sized
tile = Fork B1 (buffer bridge, §6). `sampled_EMPTY=0` + no content quad = Fork B2 (upstream Mojo).
