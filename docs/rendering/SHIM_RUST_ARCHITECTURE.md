# Guest GPU-shim architecture (Rust)

The guest-side GPU drivers dd injects into Linux apps — the GLES/EGL driver today, Vulkan and CUDA
next — are first-class Rust crates, not a test fixture. They cross-compile to the guest ELF targets
and export the Khronos/vendor C ABI, so an unmodified app that links `-lEGL -lGLESv2` (or Vulkan, or
CUDA) loads them as its driver.

This supersedes the single hand-rolled C file `dd-tests/guests/gl_shim.c`, which shipped as product
code from a test directory and re-implemented, by hand, the dd-gpu IR wire that the host already
defines. The Rust crates fix both problems: product code lives in product crates, and the IR is one
shared Rust type both sides compile against.

## Crates

```
dd-gpu              host-side: the IR (ir::Cmd), wire codec, GpuBackend trait, Metal/software executors
  └── dd-shim-common   guest-side foundation (rlib): re-exports dd-gpu's IR as the shared contract +
        │              owns the host-exec transport, completion wake, and GPU-memory registration
        ├── dd-shim-gl    GLES2/EGL front-end (cdylib) → libEGL.so.1 / libGLESv2.so.2
        ├── dd-shim-vk    (future) Vulkan ICD (cdylib) → libvulkan_dd.so / an ICD manifest
        └── dd-shim-cuda  (future) CUDA/NVML driver (cdylib) → libcuda.so.1 / libnvidia-ml.so.1
```

`dd-shim-common` is the only-place-the-wire-lives layer lifted out of `gl_shim.c`:

- **The shared IR contract.** It does *not* redefine the IR — it `pub use`s `dd_gpu::{ir, wire, id}`.
  A shim builds a `dd_gpu::ir::Cmd` stream and serializes it with `dd_gpu::ir::encode_stream`; the
  host decodes it with the *same* `dd_gpu::ir::Cmd::decode` (`dd_gpu::replay::replay_stream`). One Rust
  type, one encode/decode implementation, shared by value — the guest and host **cannot drift**. This
  is what `gl_shim.c`'s hand-rolled `iu8/iu32/istr` and its bare magic tag numbers (`iu8(8)` ==
  `CreateShader`) used to duplicate against `dd-gpu/src/ir.rs`'s `tag`/`etag` tables.
  (`dd_shim_common::transport::FrameBuilder` is the thin accumulator; `transport.rs`'s
  `framebuilder_encodes_the_shared_contract` test round-trips shim-encoded bytes through the host
  decoder as the anti-drift gate.)
- **The transport** (`transport::ExecConn`): the persistent Unix-socket channel to the host GPU-exec
  service (`$DD_GPU_EXEC`), the per-frame `[surface.id, w, h, ir.len()][ir]` header + 1-byte-ack frame
  protocol, lazy reconnect, and the residency-reset signal (a fresh host backend has an empty resource
  cache → re-emit all). Faithful port of `gl_shim.c`'s `exec_stream`.
- **Guest GPU-memory registration** (`transport::renderd::alloc`): the `renderD128`
  `DD_IOCTL_GPU_ALLOC` that mints the rung-2 IOSurface/dma-buf a frame renders into.
- **The completion-wake seam** (`transport::Doorbell` eventfd, `transport::futex_wake`): the primitives
  the future shared-memory command ring will signal on. The working path today blocks on the socket
  ack; the seam is here so a ring-mode shim (and `dd-shim-vk`) don't re-invent it.

The syscall surface is hand-declared `extern "C"` against libc — no external crates — matching the
`dd-gpu` / `dd-term-core` dependency-free discipline.

## Surface-completeness from the Khronos registry

`dd-shim-gl` exports the **complete GLES2 + EGL entry-point set** (358 GLES2 across ES 2.0–3.2 + 44
EGL = 402 symbols), not a hand-picked few. The surface is code-generated:

- `dd-shim-gl/registry/gles2_egl.manifest` — the compact, committed list of every entry point (name,
  return type, params), **generated from the Khronos API registry** `gl.xml` / `egl.xml` by
  `registry/extract_from_khronos.py` (filtering `feature api="gles2"` GL_ES_VERSION_2_0..3_2 and
  `feature api="egl"` EGL_VERSION_1_0..1_5). The manifest is committed (the 2.7 MB `gl.xml` is not) so
  the build needs no XML and no network; regenerate it when bumping the API level.
- `dd-shim-gl/build.rs` — reads the manifest, maps each C type to its Rust C-ABI type (pointer forms
  handled generally; an unknown *base* scalar panics — a fail-loud ABI generator), and emits a
  `#[no_mangle] extern "C"` function for every entry point **not** in its `IMPLEMENTED` set.
- Hand-written bodies (`src/egl.rs`, `src/gles.rs`) implement the semantics for the entry points
  listed in `IMPLEMENTED`; the generator skips those names (no duplicate symbols). Everything else is a
  spec-faithful **default stub**: correct ABI so the app links and runs, a `DD_SHIM_DEBUG`-gated
  "unimplemented entry point" trace (so we can see exactly which calls a given app makes, in order, to
  prioritize porting), and a benign default return. Stubs are replaced by real bodies incrementally —
  the shrinking long tail — with the exported surface never changing.

The generated census constants (`GLES2_ENTRYPOINTS`, `EGL_ENTRYPOINTS`, `GENERATED_STUBS`) back a
completeness test (`surface_is_complete_and_large`).

## Toolchain: cross-compiling Rust to the guest ELF

The key mechanic — a Rust `cdylib` cross-compiled to the guest target with a C ABI + the right
soname — is verified:

- **aarch64 guest: works today, no setup.** This dev host *is* `aarch64-unknown-linux-gnu`, which is
  the guest aarch64 target, so a native `cargo build -p dd-shim-gl --release` produces a valid aarch64
  Linux `.so`. `#[no_mangle] extern "C"` symbols export as `GLOBAL FUNC` in `.dynsym` (verified: 402
  `gl*`/`egl*` symbols). The soname is set via `cargo:rustc-cdylib-link-arg=-Wl,-soname,libEGL.so.1`
  in `build.rs` (verified `readelf -d` → `SONAME libEGL.so.1`). A plain C `dlopen` of the `.so` calls
  the entry points and gets the expected values (verified: `eglQueryString`/`glGetString` return the
  same strings as `gl_shim.c`; `eglGetProcAddress` resolves entry points to their real addresses).
- **x86_64 guest: needs a one-time toolchain add.** `x86_64-unknown-linux-gnu` `rust-std` is not
  installed and there is no `rustup` on this box (rustc is a source-tarball install). The cross linker
  (`x86_64-linux-gnu-gcc`) is present. To build the x86_64 guest `.so`, install the matching
  `rust-std` for `x86_64-unknown-linux-gnu` (via `rustup target add x86_64-unknown-linux-gnu`, or drop
  the std tarball into the sysroot) and link with `-C linker=x86_64-linux-gnu-gcc`. **Action item for
  the maintainer** — the aarch64 path needs nothing.

`dd-shim-common` / `dd-shim-gl` are workspace members but kept **out of `default-members`** (like
`dd-gpu-wgpu`), so the engine gate's `cargo build` surface is unchanged (verified: a default build
does not compile them). Build/test explicitly: `cargo test -p dd-shim-common`,
`cargo build -p dd-shim-gl --release`.

### Packaging the three sonames

`gl_shim.c` is deployed as one `.so` under three sonames: `libEGL.so.1` carries the code, and
`libGLESv2.so.2` / `libwayland-egl.so.1` are thin `DT_NEEDED → libEGL.so.1` stubs. The Rust cdylib
bakes `SONAME libEGL.so.1`; the deploy step (renaming `libdd_shim_gl.so` → `libEGL.so.1` and creating
the two stub libraries) is unchanged. Note the cdylib currently also `DT_NEEDED`s `libgcc_s.so.1`
(Rust's unwinder) and `libc.so.6`; the glibc guest rootfs provides both. A `panic = "abort"` +
`-C link-arg=-Wl,--as-needed` build can drop `libgcc_s` if a leaner dependency set is wanted.

## Adding `dd-shim-vk` / `dd-shim-cuda`

Both are sibling cdylib crates on `dd-shim-common`:

1. `cargo new --lib dd-shim-vk`, `crate-type = ["cdylib"]`, `dependencies.dd-shim-common = { path = .. }`.
2. Generate the export surface from the registry: Vulkan from `vk.xml` (the Khronos Vulkan registry,
   same extractor pattern → a `vk_commands.manifest`); CUDA/NVML from the driver headers (the existing
   `dd-gpu/cuda`, `dd-gpu/nvml` hand-lists are the seed). Set the soname (`libvulkan_dd.so` + an ICD
   JSON for Vulkan; `libcuda.so.1` / `libnvidia-ml.so.1` for CUDA — matching `dd-gpu/cuda/build.sh`).
3. Encode work as `dd_gpu::ir::Cmd`s (the IR is already WebGPU/Vulkan-family with SPIR-V as the shader
   ABI, so a Vulkan front-end maps closely) and ship them through `dd_shim_common::transport` — the
   same channel, ack protocol, and memory registration `dd-shim-gl` uses. No transport is re-invented.

## Retiring `gl_shim.c` (the incremental cutover)

GLES must keep rendering the entire time, so `gl_shim.c` stays the deployed driver until `dd-shim-gl`
reaches pixel parity. The path:

1. **Foundation (done).** `dd-shim-common` (shared IR + transport, tested) and `dd-shim-gl` (complete
   registry-generated surface + query entry points, building the correct `.so`). `gl_shim.c` untouched
   → GLES unaffected.
2. **State machine (done — 112 entry points).** The GLES/EGL object + scalar state was ported into
   `dd-shim-gl` (`src/state.rs`, `src/gles.rs`, `src/egl.rs`), moving 105 names from generated stubs
   into `IMPLEMENTED`: buffers, textures (with the RGBA8 unpack-aware upload), shader/program objects,
   uniform-block storage, vertex-attrib/VAO state, blend/depth/cull/scissor/viewport scalar state and
   all the `glGet*`/`glIs*` queries, plus the EGL config/context/surface query + lifecycle
   (`eglChooseConfig`/`eglGetConfigAttrib`/`eglCreateContext`/`eglMakeCurrent`/`eglQuerySurface`/…).
   Like `gl_shim.c`, these accumulate state and emit no IR. The GL→wire enum/id maps
   (`src/wireenc.rs`) and the present-independent **resource lowering** (`src/lower.rs`:
   buffer/texture/sampler → `Cmd`) are ported and proven **byte-identical** to `gl_shim.c`'s emission.
   Still stubbed on purpose (owned by concurrent present-path work): `glClear`/`glDrawArrays`/
   `glDrawElements` recording, the GLSL-ES→(SPIR-V/MSL) translator (so `glGetUniformLocation`/
   `glGetAttribLocation` report "not found" for now), the residency cache, `eglCreateWindowSurface` +
   the wayland/dma-buf commit, and `eglSwapBuffers` (lower state → `Cmd` stream → `ExecConn::submit`).
3. **Present path — draw recording + surface bring-up + swap boundary (in progress).**
   `glClear`/`glDrawArrays`/`glDrawElements` now record into the frame draw-list (`src/state.rs`
   `DrawCall`), `eglCreateWindowSurface` brings up the presented surface, and `eglSwapBuffers`
   (`src/egl.rs` + `src/frame.rs`) lowers the draw-list to the dd-gpu IR and presents it (via
   `DD_IR_DUMP` in host-tool/parity mode, or `dd_shim_common::transport::ExecConn` in the deployed
   path; the wayland/dma-buf commit is the remaining display plumbing). The **clear-path frame is
   byte-identical to gl_shim.c**; the shader-bearing draw path is the remaining work (below).
4. **GLSL-ES → MSL translator (done — byte-verified).** `src/translate.rs` is a byte-for-byte port of
   gl_shim.c's `translate()` + ~20 helpers (strip-comments, collect decls, main-body extract,
   word-boundary/plain replace, type/builtin/relational/local-decl fixups, `fix_trunc`, sampler
   rewrites, `mod`/`mat3x2` helper injection, and the `uni_layout` uniform-block byte layout).
   `glLinkProgram` runs it → `CreateShader` MSL; `glGetUniformLocation`/`glGetAttribLocation` resolve
   against the layout + declaration order. Verified GREEN against gl_shim.c's own `-DDD_TR_TOOL gl_tr`
   over the whole `shader_translate/*.glsl` corpus (`tests/translate_parity.rs`).
5. **Draw-time emission (done — single-draw path).** `src/frame.rs` `build_single_draw_frame` lowers a
   non-clear draw to the exact `dd_gpu::ir::Cmd`/`Enc` sequence gl_shim.c's non-replay `eglSwapBuffers`
   emits: VBO/index/texture/uniform resources, `CreateShader` + `CreateRenderPipeline` (vertex layout,
   blend/depth, topology), `CreateBindGroup`, and the render pass (Begin/SetPipeline/Viewport/Scissor/
   SetBindGroup/SetVertexBuffer/Draw/End) with the Y-flipped viewport/scissor.
6. **Pixel/IR-parity harness — LIVE, flagship gate GREEN (`tests/pixel_parity.rs`).** The flagship
   `full_frame_textured_triangle_is_byte_identical` compiles the SAME GLES workload (shader compile +
   VBO + 2×2 texture + mat4 uniform + sampler + `glDrawArrays`) against **both** shims' `.so` (gl_shim.c's
   `libEGL.so.1` and dd-shim-gl's cdylib), runs each with `DD_IR_DUMP`, and asserts the IR is identical
   — **GREEN: 1316 bytes, byte-for-byte**. The clear frame is likewise byte-identical (43 bytes).
7. **Remaining:** (a) the **replay path** (multi-draw / clear+draw frames — gl_shim.c's per-draw
   `20+d`/`30+d`/`40+d` ids + segmented render passes) for full glmark2/Chrome coverage, same
   byte-equivalent `Cmd`/`Enc` pattern; (b) the **deployed-path display plumbing** — the wayland/dma-buf
   surface handshake + `wl_commit` in `eglSwapBuffers` (only the IR-submit half is wired; `DD_IR_DUMP`
   and the parity gate don't need it).
8. **Cutover (proposed — not flipped):** the `~/.dd/gui/<arch>/lib` deploy builds dd-shim-gl's cdylib as
   `libEGL.so.1` + the `libGLESv2.so.2`/`libwayland-egl.so.1` DT_NEEDED stubs, selected by
   `DD_SHIM_IMPL=rust` so a validation run swaps to the Rust libs while the default stays gl_shim.c's.
   Flip the default only after a live glmark2/Chrome pixel-check.

## GLES-still-works evidence (this increment)

- `gl_shim.c` is **unmodified** (`git diff` empty) and still compiles to the deployed `libEGL.so.1`
  (aarch64 `.so`, `SONAME libEGL.so.1`, clean build) and to its `-DDD_TR_TOOL` translator tool.
- `dd-gpu` (the shared IR) is unchanged and its 70 tests pass.
- The default `cargo build` (engine-gate surface) does not compile the new crates.
- `dd-shim-common` tests pass (shared-IR round-trip through the host decoder; exec-socket framing;
  `GpuAlloc` layout). `dd-shim-gl` builds the 402-symbol `.so` and a C `dlopen` drives it correctly.
