# hl-gl (WIP staging crate)

Self-contained OpenGL-ES / EGL guest driver. Dissolves `hl-shim-gl` into one crate that records `gl*` /
`egl*` calls into a per-context state machine and **lowers them to `hl_gpu` IR — deferred and batched at
the `eglSwapBuffers` frame boundary**. Structurally IDENTICAL to `hl-cuda`: the only dependency is the
neutral protocol crate `hl-gpu` (package `hl_gpu`, lib `hl_gpu`); every path funnels through the
same `CommandSink` lowering seam.

Build/test standalone (empty `[workspace]` ⇒ excluded from the shared repo-root workspace):

```
cargo test --manifest-path hl-gl/Cargo.toml
```

## Layout (mirrors hl-cuda: model / service / adapter / result)

```
src/
  lib.rs                crate root + module decls + re-exports
  result.rs             EGL/GL error codes + GpuError → EGLint/GLenum maps  (cuda's result.rs analogue)
  model/                the GL object model + invariants (owned values; no Cmd, no transport)
    context.rs          GlContext: surface + bound GL state + tables + draw-list + IR id counters
    buffer.rs           GL buffer objects + table  (glGenBuffers/glBufferData)
    texture.rs          GL texture objects + table (glGenTextures/glTexImage2D) + sampler mapping
    program.rs          Shader/Program + table + DrawCall/Attr (recorded draw snapshot)
    glconst.rs          canonical Khronos GL/EGL numeric constants
  service/              the operation seam
    record.rs           the gl* recording ops — mutate the model, submit NOTHING (deferred lowering)
    frame.rs            build_frame_ir: recorded draw-list → the frame's Cmd stream
    swap.rs             eglSwapBuffers → submit frame IR + Present through the CommandSink (tested seam)
  adapter/
    glsl.rs             GLSL-ES vertex+fragment pair → shader-IR word payload (was translate.rs)
shim/  egl/lib.rs  gles/lib.rs     guest cdylib exports (DEFERRED this pass — small stubs)
build.rs                           no-op this pass (dual-arch cross-compile is deferred)
references/  registry/  oracle/    tracked first-party registry sidecars + parity oracles
tests/lowering.rs                  RecordingSink-based lowering tests
```

## Ported from

`hl-shim-gl/src/`: `egl.rs` (swap boundary) → `service/swap.rs`; `gles.rs` + `state.rs` (recording +
state machine) → `service/record.rs` + `model/`; `frame.rs` + `lower.rs` (swap-time emission) →
`service/frame.rs`; `translate.rs` (GLSL→shader source) → `adapter/glsl.rs`; `glconst.rs` →
`model/glconst.rs`; `wireenc.rs` vertex-format maps → inlined in `service/frame.rs`.

## Scope of this staging pass

FULLY lowered core path: `glGenBuffers`/`glBufferData` → `CreateBuffer`/`WriteBuffer`;
`glGenTextures`/`glTexImage2D` → `CreateTexture`/`CreateSampler`/staging `WriteBuffer` + `CopyBufferToTexture`;
`glCreateShader`/`glLinkProgram` (GLSL→shader-IR) → `CreateShader`/`CreateRenderPipeline`;
`glDrawArrays`/`glDrawElements` → a recorded draw; `eglSwapBuffers` → the frame's `CommandBuffer`
(`BeginRenderPass`/`SetPipeline`/…/`Draw`/`EndRenderPass`) + `Submit` + `Present`. Clear-only and
single-draw frames are covered. DEFERRED (like cuda phase 1): the shim cdylibs (`shim/`), `build.rs`
dual-arch cross-compile, the `hl_jit::Driver` plug, and the multi-draw / clear+draw **replay** frame path.
