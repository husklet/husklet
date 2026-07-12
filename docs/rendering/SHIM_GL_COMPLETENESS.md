# dd-shim-gl GLES2/EGL completeness audit

The Rust shim exports the **complete** 402-symbol GLES2+EGL surface (registry-generated). Every symbol
resolves and has the correct C ABI; this audit classifies each by *behavioral* completeness against the
gl_shim.c reference. Counts are as of this pass (148 real bodies / 254 default stubs).

Categories (per the completeness bar):
- **(a) real body at gl_shim.c parity** — byte-identical IR / faithful state. 148 entry points.
- **(b) real body needed but not yet ported** — gl_shim.c does meaningful work; the stub renders wrong.
- **(c) spec-faithful stub, correct as-is** — gl_shim.c *also* no-ops / returns the spec default, so the
  stub is at parity; nothing an app relies on is silently wrong beyond what gl_shim.c itself does.
- **(d) stub a real app calls and would break on** — the priority; these are filled first.

## (a) Real bodies (148) — the covered surface

EGL: display/config/context/surface query + lifecycle (`eglGetDisplay`/`Initialize`/`ChooseConfig`/
`GetConfigAttrib`/`CreateContext`/`MakeCurrent`/`QuerySurface`/`CreateWindowSurface`/`SwapBuffers`/…) +
`wl_egl_window_*`. GLES: buffers, textures (2D upload + params + pixel-store), shaders/programs +
the GLSL-ES→MSL translator + uniform-block layout, **all `glUniform*` setters** (float/int/uint scalar
+ vector + every matrix incl. mat3/2x3/… with MSL column re-stride), vertex attribs + **VAOs**, blend/
depth/cull/scissor/viewport scalar state + the `glGet*`/`glIs*` queries, draw recording (`glClear`/
`glDrawArrays`/`glDrawElements` + instanced), and the frame lowering (clear / single-draw / replay).
Six full-frame IR gates are byte-identical to gl_shim.c (clear, wl_egl_window, textured-triangle,
multi-draw replay, mat3-uniform, + resource lowering).

## (d) Category-(d) gaps filled in this pass

Entry points real apps (glmark2 / GTK4 / Chrome / Qt) call that were stubs and would render wrong:

| Entry points | Why it broke | Fix (byte-identical to gl_shim.c) |
|---|---|---|
| `glUniform{2,3,4}i`, `glUniform{1,2,3,4}iv`, `glUniform{1,2,3,4}ui{,v}`, `glUniformMatrix{2,3,2x3,3x2,2x4,4x2,3x4,4x3}fv` | app sets these uniforms → stub dropped them → wrong transforms/colors | `uni_write` / `uni_write_matrix` (mat3 columns re-stride to MSL's 16-byte stride) |
| `glDrawArraysInstanced`, `glDrawElementsInstanced` | instanced draws emitted nothing | delegate to the non-instanced draw (one instance), as gl_shim.c does |
| `glCheckFramebufferStatus` | returned `0` → every app aborts FBO setup | return `GL_FRAMEBUFFER_COMPLETE` |
| `glGenVertexArrays`, `glBindVertexArray`, `glDeleteVertexArrays`, `glIsVertexArray` | VAO bind didn't save/restore attrib state → multi-object scenes broke | port `g_vao` capture/restore (`vao_store_current`/`vao_load`) |

New parity test `full_frame_mat3_uniform_is_byte_identical` (mat3 + ivec2 uniforms) locks in the
uniform-buffer bytes byte-for-byte.

## Offscreen framebuffers — DONE (real-app IR parity closed)

The full FBO/renderbuffer subsystem is ported byte-for-byte: `glGen/Bind/Delete/IsFramebuffer`,
`glFramebufferTexture2D`/`glFramebufferTextureLayer`/`glFramebufferRenderbuffer`,
`glCheckFramebufferStatus`, `glGetFramebufferAttachmentParameteriv`, the renderbuffers
(`glGen/Bind/Delete/IsRenderbuffer`, `glRenderbufferStorage{,Multisample}`,
`glGetRenderbufferParameteriv`), plus `glBlitFramebuffer` and `glReadPixels` (CPU-side texture blit /
readback) and `glClearBufferfv`. `record_draw_call`/`record_clear_call`/`clear_scissor_rect` now
resolve a draw's `target_tex` from the bound draw-FBO's color attachment (`state::draw_fbo_target`), so
the replay lowering's per-draw `target_tex` segmentation + `color_target_format` (offscreen Rgba8 vs
surface Bgra8) drive the offscreen pass. `glClear` of a bound FBO bakes into the offscreen texture's CPU
data (no ClearRect), exactly as gl_shim.c. **Parity gate `full_frame_fbo_render_to_texture` (render to
an offscreen FBO then sample it to the default framebuffer — the render-to-texture pattern glmark2 /
GTK4 GskGLRenderer / Chrome-ANGLE use) is byte-identical, 67472 bytes.** This is the last real-app IR
parity gap; **full real-app IR parity is closed.**

## (b) Real bodies still needed (scoped, not yet ported)

These do meaningful work in gl_shim.c; the stub is at spec (correct ABI, benign) but not at render
parity. Ordered by app impact:

1. **Texture storage / 3D / compressed** — `glTexStorage2D`/`3D`, `glTexImage3D`/`glTexSubImage3D`,
   `glCompressedTexImage2D`/`…`. gl_shim.c allocates RGBA8 storage; `glTexStorage2D` is used by GTK4 /
   Chrome for immutable textures.
3. **Pixel readback** — `glReadPixels`. gl_shim.c reads back the CPU-side texture; readback is
   host-side (not in the deferred IR), so this needs a synchronous round-trip to the executor.
4. **Buffer mapping / UBO binding** — `glMapBufferRange`/`glUnmapBuffer`/`glFlushMappedBufferRange`,
   `glBindBufferRange`/`glBindBufferBase`/`glUniformBlockBinding`. GTK4/Chrome map buffers and use
   uniform blocks; today uniforms go through the `[[buffer(1)]]` block the translator emits.

## (c) Spec-faithful stubs (correct as-is)

The remaining ~230 stubs are at parity with gl_shim.c because gl_shim.c *also* no-ops or returns the
spec default for them, so nothing an app relies on is more wrong than on the C shim:

- **Query getters** returning defaults: `glGetActiveUniform`/`glGetActiveAttrib`/`glGetShaderSource`/
  `glGetUniform*`/`glGetTexParameter*`/`glGetVertexAttrib*`/`glGetProgramBinary`/… (gl_shim.c returns
  empty/0 too — frameworks that introspect degrade identically).
- **No-op state** gl_shim.c doesn't back: `glVertexAttrib{1,2,3,4}f{,v}` (constant attributes),
  `glStencil{Func,Op,Mask}Separate` (stencil), `glBindAttribLocation` (the shim uses declaration order),
  `glSamplerParameter*`, `glProgramParameteri`, `glReleaseShaderCompiler`, `glShaderBinary`.
- **Unsupported-by-host features** — advertised accurately (NOT claimed): transform feedback
  (`glBeginTransformFeedback`/…), occlusion queries (`glBeginQuery`/…), fence sync
  (`glFenceSync`/`glClientWaitSync`/…), MRT/`glClearBuffer*`. These are honest no-ops; the shim never
  advertises them: `glGetString(GL_EXTENSIONS)` lists only backed extensions and `glGetIntegerv`
  reports conservative limits.

**Capability honesty:** the shim never claims a capability the host can't back — the extension string
and integer limits reflect only what the Metal executor implements. Debug-tracing (`DD_SHIM_DEBUG`)
prints any unimplemented entry point the first time an app calls it, so category-(d) regressions surface.

## GLES 3.0 + half-float vertex data (assessment — cross-cutting, not implemented)

GTK4's `GskGLRenderer`/`GskNglRenderer` take the GPU path (instead of the cairo/pixman *software*
fallback) only against an **ES 3.0** context, and they upload vertex data as **half-float** (`GL_HALF_
FLOAT`) attributes. Supporting this is a three-layer change (guest reference **gl_shim.c**, guest Rust
**dd-shim-gl**, host **dd-gpu** + Metal backend) — none alone is sufficient:

1. **Advertise ES 3.0 (guest, mostly done).** `eglCreateContext` already accepts version 3 under
   `DD_SHIM_ES3`; `glGetString(GL_VERSION)`→"OpenGL ES 3.0", GLSL→"3.00"; the translator already handles
   `#version 300 es` `in`/`out`/`texture()`. Gaps to make ES3 real for GTK: **UBOs**
   (`glBindBufferRange`/`glUniformBlockBinding` → a real bound uniform buffer, category-(b) above),
   VAOs (done), and the ES3 sampler/`glGetStringi` extension enumeration.
2. **Half-float vertex format (all three layers).**
   - *Guest:* `vertex_format_wire` needs a `GL_HALF_FLOAT` (0x140B) kind and `attr_elem_size` = 2 bytes;
     both gl_shim.c and dd-shim-gl (`src/wireenc.rs`) must add it identically (keeps IR byte-parity).
   - *Host IR (dd-gpu):* the `VertexFormat` wire enum + Metal descriptor must gain `Float16x2`/`Float16x4`
     (→ `MTLVertexFormat.half2`/`half4`). Today the packed vertex-format `u32` only encodes float/
     int/uint/byte/short kinds; half-float is a new kind the executor must map.
   - *Backend:* `dd-gpu`'s `MetalBackend` maps the new format to the MTL vertex format (a small, local
     addition once the IR carries it).
3. **Scope / sequencing.** The guest changes are small and byte-parity-testable offline (extend the
   `wireenc` unit tests + a half-float vertex workload in `pixel_parity`). The host changes (dd-gpu IR
   enum + Metal backend) require a mac build + the executor, so they land as a paired dd-gpu + shim
   change. Recommended order: (1) UBO binding + FBO (category-b) so ES3 contexts render at all, then
   (2) the half-float vertex format as a coordinated guest+host change. Until then, keep advertising ES2
   by default (GTK falls back to software, correctly) rather than claiming ES3 the host can't fully back.
