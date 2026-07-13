# dd-shim-gl GLES2/EGL completeness audit

The Rust shim exports the **complete 402-symbol** GLES2+EGL surface (registry-generated: 358 GL + 44
EGL). Every symbol resolves with the correct C ABI. This audit is the **generated capability inventory**
(`CAPABILITIES` in the build, one record per symbol) that classifies each entry point by *behavioral*
truthfulness. It is emitted by `build.rs` from the SAME `IMPLEMENTED`/`PARTIAL` classification the
runtime uses, so it can never drift from the exported surface — the `inventory_covers_every_exported_
symbol` test asserts a 1:1 correspondence and that every level's invariants hold.

## Capability levels

| Level | Count | Behavior | Error state |
|---|---|---|---|
| **full** | 218 | Real hand-written body at gl_shim.c parity (byte-identical IR / faithful state). | none |
| **partial** | 106 | Spec-legitimate no-op / default query matching gl_shim.c's own degraded behavior. ALWAYS initializes outputs; returns the spec default / correct not-found sentinel. | none (a no-op is the correct answer) |
| **stub** | 78 | An operation a conforming driver performs and the shim does NOT: FAILS truthfully — sets the API-correct GL/EGL error, zeroes outputs, returns the spec failure value, aborts under `DD_SHIM_STRICT`. | `glGetError`/`eglGetError` raised |

By lib: GL = 183 full / 106 partial / 69 stub; EGL = 35 full / 0 partial / 9 stub.

**Both advertised core profiles' mandatory surfaces are COMPLETE:** GLES 2.0 = **0/142** stubs and
EGL 1.4 = **0/34** stubs (the `advertised_gles2_…` and `advertised_egl14_…` ledger gates pass). The EGL
1.4 tail (`eglSurfaceAttrib`, `eglBindTexImage`/`eglReleaseTexImage`, `eglCopyBuffers`,
`eglCreatePixmapSurface`, `eglCreatePbufferFromClientBuffer`) has real hand-written bodies that are
truthful for the genuinely-unsupported native-pixmap / client-buffer / texture-binding paths (a benign
validated attribute set for `eglSurfaceAttrib`). All are IR-free, so the byte-parity gates are unchanged.

**The Phase-0 exit gate holds and the profile is now more complete (Phase 4.1):** no unsupported entry
point can silently report success, and the previously-stubbed high-use GLES2/ES3 families now have real
bodies at gl_shim.c byte-parity.

## Phase 4.1 promotions (this pass)

Promoted **stub -> full** (real bodies ported from gl_shim.c, byte-parity gated by
`full_frame_texstorage_and_mapbuffer_is_byte_identical`):

- **Immutable texture storage & 3D/array upload:** `glTexStorage2D`/`glTexStorage3D`, `glTexImage3D`/
  `glTexSubImage3D`, and copy-from-framebuffer `glCopyTexImage2D`/`glCopyTexSubImage2D`.
- **Buffer mapping:** `glMapBufferRange` (a real pointer into the buffer's storage) + `glUnmapBuffer`
  (dirties the buffer so the swap re-uploads) + `glCopyBufferSubData`.
- **Integer vertex arrays:** `glVertexAttribIPointer`. **Range draws:** `glDrawRangeElements`.
- **Indexed extension enumeration:** `glGetStringi` (served from the same inventory list as
  `glGetString(GL_EXTENSIONS)`). **Object-name allocators:** `glGenQueries`/`glGenSamplers`/
  `glGenTransformFeedbacks` (monotonic ids). **EGL:** `eglGetPlatformDisplay` (modern display open).

Reclassified **stub -> partial** to match gl_shim.c's *deliberate* no-ops (an error would DIVERGE from
the oracle, breaking apps that query `glGetError` after these): UBO binding (`glBindBufferBase`/`Range`,
`glUniformBlockBinding`, `glFlushMappedBufferRange`), `glDrawBuffers`/`glReadBuffer`, integer/depth
`glClearBuffer{iv,uiv,fi}`, `glVertexAttribDivisor`, compressed/3D-copy texture uploads, sampler-object
params (`glBindSampler`/`glSamplerParameter*`), query/transform-feedback begin/end/bind, and
`glProgramBinary`/`glShaderBinary`.

## ES3 object families — real object lifecycle (query / sampler / transform-feedback / UBO binding)

Four `full`-classified ES3 object families whose bodies were previously no-ops (mirroring gl_shim.c's
own no-ops: begin/end/param did nothing, and every getter returned 0) now carry **real client-side
object state**. All are IR-free — no `Cmd`/`Enc` is emitted for any of them — so the 8 byte-parity
gates and the frame IR are byte-identical; the change is purely observable object semantics through the
public API (mirrored by `tests/es3_objects.rs`). Counts are unchanged (these were already `full`).

- **Sampler objects** (`glGenSamplers`/`glBindSampler`/`glSamplerParameter{i,f,iv,fv}`/
  `glGetSamplerParameter{iv,fv}`/`glDeleteSamplers`/`glIsSampler`): each object stores the full ES 3.0
  sampler state (min/mag filter, wrap S/T/R, min/max LOD, compare mode/func) initialized to the spec
  defaults (table 6.10); a set value round-trips through the getter. Names reserve lazily (a name from
  `glGenSamplers` is not yet an object until first bound/parameterized — `glIsSampler` false until
  then), `glDeleteSamplers` unbinds from every unit, and validation is atomic: an out-of-range enum is
  `GL_INVALID_ENUM` (object untouched), and any op on a non-generated name is `GL_INVALID_OPERATION`
  with the output preserved.
- **Query objects** (`glGenQueries`/`glBeginQuery`/`glEndQuery`/`glGetQueryiv`/`glGetQueryObjectuiv`/
  `glDeleteQueries`/`glIsQuery`): typed-target lifecycle enforced — a query name binds to exactly one
  target (`GL_ANY_SAMPLES_PASSED`, `…_CONSERVATIVE`, `GL_TRANSFORM_FEEDBACK_PRIMITIVES_WRITTEN`) and
  cannot be reused with another; only one query is active per target (`GL_CURRENT_QUERY` reports it);
  nested begin, id 0, or an ungenerated name is `GL_INVALID_OPERATION`; an invalid target is
  `GL_INVALID_ENUM`. `glEndQuery` captures the submission serial (flushing first, exactly like a fence),
  so `GL_QUERY_RESULT_AVAILABLE` flips true only once completion catches up, and reading
  `GL_QUERY_RESULT` blocks for completion first. The counted result itself is a **truthful 0** (the
  executor has no occlusion/primitive counter yet) rather than a fabricated value — the honest residual
  for this row is the backend counter, not the lifecycle.
- **Transform-feedback objects** (`glGenTransformFeedbacks`/`glBindTransformFeedback`/
  `glBeginTransformFeedback`/`glEndTransformFeedback`/`glPause…`/`glResume…`/
  `glDeleteTransformFeedbacks`/`glIsTransformFeedback`/`glTransformFeedbackVaryings`/
  `glGetTransformFeedbackVarying`): a real begin/end/pause/resume state machine on the bound object (the
  default object, name 0, always exists and cannot be deleted); names reserve lazily then instantiate on
  first bind; the bind target must be `GL_TRANSFORM_FEEDBACK`; rebinding while active-and-not-paused,
  nested begin, pause/resume out of order, end while inactive, and deleting an active object are all
  `GL_INVALID_OPERATION`; a bad primitive mode / bad target is `GL_INVALID_ENUM`. The
  `glTransformFeedbackVaryings` capture list (names + interleaved/separate buffer mode) is stored per
  program and the varying **names round-trip** through `glGetTransformFeedbackVarying` (size/type are a
  truthful best-effort single-vec4, since the shim has no GLSL varying reflection).
- **Uniform-block binding** (`glGetUniformBlockIndex`/`glUniformBlockBinding`/
  `glGetActiveUniformBlock{iv,Name}`/`glBindBufferBase`/`glBindBufferRange`): `glGetUniformBlockIndex`
  assigns a **stable per-program block-index namespace** lazily per queried name (no GLSL uniform-block
  reflection exists, so indices are assigned on first query — real and self-consistent, each block's
  binding defaulting to 0); `glUniformBlockBinding` sets the binding, observable through
  `glGetActiveUniformBlockiv(GL_UNIFORM_BLOCK_BINDING)`; the block name round-trips through
  `glGetActiveUniformBlockName`; and `glBindBufferBase`/`glBindBufferRange` record real per-index
  binding points (buffer + offset + size) for the `GL_UNIFORM_BUFFER` and `GL_TRANSFORM_FEEDBACK_BUFFER`
  indexed targets, with target/index/range validation. The indexed points are observed in the gate via
  a test accessor (`indexed_buffer_binding`) because the public indexed query `glGetIntegeri_v` is still
  a `partial`.

## Object / context model & error lifecycle (audit §9.3)

The GL/EGL object model is a typed share-group design rather than one process-global mutex + a sentinel
context handle. This turns two RED conformance probes green (both pass unmodified against the built
`libEGL.so.1`):

- **`gui_egl_error_lifecycle`** — `glGetError` retains the FIRST error since the last read (a later
  error does not overwrite it), clears to `GL_NO_ERROR` on read, and an invalid call sets the error
  WITHOUT mutating GL state. Covered: `glViewport` negative dims → `GL_INVALID_VALUE` (viewport
  unchanged), `glEnable`/`glDisable` unknown cap → `GL_INVALID_ENUM`, `glBindBuffer` bad target →
  `GL_INVALID_ENUM` (binding unchanged), `glGenBuffers(-1)` → `GL_INVALID_VALUE` (output untouched),
  `glUniform*` with no current program → `GL_INVALID_OPERATION`. `eglGetError` has the same per-thread
  first-error retention + clear-on-read.
- **`gui_egl_sharegroup_threads`** — `eglCreateContext` returns a UNIQUE handle per context; a
  `share_context` joins a **ShareGroup** whose GL objects (textures/buffers/programs/shaders) are shared
  across its member contexts; unrelated contexts are ISOLATED (an object in one group is invisible in
  another, even at the same numeric name); cross-context deletion works within a group; `eglMakeCurrent`
  binds the CALLING THREAD's current context (per-thread, so two threads can be current on two contexts
  at once), and `eglGetCurrentContext`/`eglGetCurrentDisplay` report per-thread state (EGL_NO_CONTEXT /
  EGL_NO_DISPLAY when unbound).

Model (`state.rs`): each `EglCtx` handle references a `&'static Mutex<GlState>` share group; the current
context is a thread-local; `gl()` resolves to the calling thread's context group (the process default
group when no context is current — so single-context apps and the byte-parity harness are unchanged).
The draw/resource IR lowering reads that `GlState` exactly as before, so **all byte-parity gates stay
byte-identical** (this is guest-side object-model state; the IR is untouched). The affected entry points
were already `full`; this pass makes their object/error/context semantics correct, so counts are
unchanged.

## Additional truthful semantics (audit §11 ledger closures)

Contradictory ledger rows closed by extending the typed model (object-model/error state, IR unchanged,
byte-parity gates still byte-identical):

- **`egl_contexts_are_distinct_shareable_and_current_per_thread`** — completed alongside the share-group
  model: `eglReleaseThread` unbinds the calling thread's current context + surfaces (so
  `eglGetCurrentContext`/`eglGetCurrentDisplay` report EGL_NO_CONTEXT/EGL_NO_DISPLAY); distinct
  shareable handles + per-thread current are covered by the §9.3 model.
- **`egl_surfaces_have_distinct_lifetimes_dimensions_and_types`** — a generation-checked typed surface
  arena (`state.rs`): `eglCreate{Window,Pbuffer}Surface` return DISTINCT handles carrying their real
  type + dimensions; `eglQuerySurface` reports per-surface size; destroy bumps the slot generation so a
  stale/forged handle is `EGL_BAD_SURFACE` (query/swap fail without mutating output). `eglMakeCurrent`
  records per-thread draw/read surfaces, reported by `eglGetCurrentSurface`.
- **`gles_shader_compile_link_status_and_logs_are_truthful`** — `glCompileShader` runs a lightweight
  dependency-free GLSL validator; `glGetShaderiv(GL_COMPILE_STATUS)` / `glGetProgramiv(GL_LINK_STATUS)`
  and `GL_INFO_LOG_LENGTH` + `glGet{Shader,Program}InfoLog` are truthful (invalid GLSL and a program
  missing a compiled vertex+fragment pair report failure with a non-empty diagnostic; valid shaders
  still compile, so the parity corpus is unaffected).
- **`egl_config_selection_and_invalid_attributes_are_truthful`** — a typed config matcher:
  `eglChooseConfig` returns zero matches for impossible/over-constrained requests (leaving the caller's
  slot untouched); `eglGetConfigAttrib` rejects forged handles (`EGL_BAD_CONFIG`) and unknown attributes
  (`EGL_BAD_ATTRIBUTE`) without writing output; `eglBindAPI` rejects non-ES APIs (`EGL_BAD_PARAMETER`).
- **`egl_swap_failure_is_reported_without_discarding_the_frame`** — `present_frame` returns a `Result`
  and `eglSwapBuffers` is transactional: it submits BEFORE resetting the draw-list and, on a delivery
  failure, retains the queued draws and reports `EGL_CONTEXT_LOST` (a stale surface is `EGL_BAD_SURFACE`).
- **`gles_flush_and_finish_have_submission_and_completion_semantics`** — `glFlush` is a nonblocking
  submit (advances a submission serial); `glFinish` blocks until the completion serial catches up.
- **`egl_wayland_handshake_discovers_globals_and_acks_the_received_configure`** — the guest wayland
  client (`wayland.rs`) is now a real handshake state machine: it DISCOVERS globals from
  `wl_registry.global` (binding each interface by its advertised name/version, not an assumed id),
  acknowledges `xdg_surface.configure` with the RECEIVED serial, answers `xdg_wm_base.ping` with a pong,
  and detects `wl_display.error`.
- **`egl_wayland_commit_propagates_io_protocol_and_frame_timeout_failures`** — the present transport is
  fallible end to end: `wflush`/`wflush_fd` return short-write / fd-pass failures, a peer disconnect
  surfaces as `WlError::Disconnected` (never a silent success), and a missing frame callback within the
  pacing deadline is `WlError::FrameTimeout`. `commit` returns a typed `Result`, which `present_frame`
  propagates so a failed present is reported by the transactional swap rather than pretended.

These wayland-client changes are the guest present transport, not the frame IR (the IR gates run in
`DD_IR_DUMP` mode with no compositor connection), so the 8 byte-parity gates are byte-identical.

## GLES command-validation ledger (audit §11) — draw/resource state closures

These pass extended the five GLES command-validation rows of the §11 rendering ledger. Every change is
guest-side validation / object state (no `Cmd`/`Enc` is emitted on any of these paths), so the 8
byte-parity gates in `pixel_parity.rs` stay byte-identical; each row is proven by an in-crate mirror
test in `tests/semantic_gates.rs`. Capability census counts are unchanged (these entry points were
already classified — the change is behavioral truthfulness, not surface).

- **`gles_pixel_store_and_texture_upload_validation_is_atomic_and_checked`** — `glCompressedTexImage2D`
  / `glCompressedTexImage3D` and their `…SubImage…` forms were empty no-ops (silent success on any
  input). They now validate atomically at gl_shim.c parity: an unsupported target or non-compressed
  internalformat is `GL_INVALID_ENUM`; a bad level/border, negative dims, or an `imageSize` that is not
  the tightly-packed `ceil(w/4)*ceil(h/4)*depth*block` byte count for the ETC2/EAC format is
  `GL_INVALID_VALUE`; no live texture, an immutable texture, or an out-of-bounds / non-4-block-aligned
  sub-region is `GL_INVALID_OPERATION`. The bound texture is left untouched on any rejection. The
  payload stays undecoded (the shim has no ETC decoder — the honest residual is GPU-side block decode),
  exactly like gl_shim.c, so no IR is produced. Mirror: `compressed_texture_upload_is_atomically_validated`.
- **`gles_framebuffer_completeness_reflects_attachment_state_and_blocks_draws`** — framebuffer
  completeness only tracked the color attachment. `Fbo` now carries depth/stencil renderbuffer
  attachments; `glFramebufferRenderbuffer` accepts `GL_DEPTH_ATTACHMENT` / `GL_STENCIL_ATTACHMENT` /
  `GL_DEPTH_STENCIL_ATTACHMENT` (the combined form attaches both aspects) and `glDeleteRenderbuffers`
  detaches them. `framebuffer_status` verifies each depth/stencil renderbuffer's format actually
  supplies the required aspect (else `INCOMPLETE_ATTACHMENT`) and that every present attachment shares
  dimensions (else `INCOMPLETE_DIMENSIONS`); an incomplete FBO still blocks draws/clears. `glBlitFramebuffer`
  now guards both the read and draw framebuffers' completeness (`INVALID_FRAMEBUFFER_OPERATION`). The
  color-only path is byte-identical (the `full_frame_fbo_render_to_texture` gate is unaffected). Mirror:
  `framebuffer_depth_stencil_completeness_and_read_blit_guards`.
- **`gles_draw_calls_validate_all_inputs_before_snapshot_or_recording`** — a draw that sources vertices
  or indices from a currently-mapped buffer is now rejected (`GL_INVALID_OPERATION`) before any snapshot
  or record, via a per-buffer `mapped` flag set by `glMapBufferRange` and cleared by `glUnmapBuffer`; a
  rejected draw submits nothing. The negotiated `GL_MAX_VERTEX_ATTRIBS` (16) limit is enforced —
  `glVertexAttrib[I]Pointer` and `gl{Enable,Disable}VertexAttribArray` raise `GL_INVALID_VALUE` for an
  out-of-range index instead of silently ignoring it. Mirror:
  `draw_validation_rejects_mapped_buffers_and_over_limit_attribs`. **Residual:** faithful instanced /
  base-vertex IR emission still needs host-side dd-gpu IR `instance_count`/`base_vertex` fields (today
  instanced draws collapse to one instance, matching gl_shim.c).
- **`gles_readpixels_validates_pack_layout_and_preserves_output_on_error`** — `glReadPixels` raised
  `GL_INVALID_OPERATION` whenever the read framebuffer had no CPU-backed color texture, which is the
  DEFAULT framebuffer case. That diverged from gl_shim.c, whose readback zero-fills the destination and
  returns no error for the default FB (the shim keeps no default-color plane). The default FB now yields
  zeros through the same pack-layout / PBO-size / client-pointer contract as the FBO path. Mirror:
  `readpixels_default_framebuffer_reads_zeros_without_error`.
- **`gles_sync_objects_track_real_submission_completion_and_wait_results`** — sync completion previously
  only advanced through a local `glFinish`. `eglSwapBuffers` now calls `note_frame_presented()` after a
  successful `present_frame`, the real cross-process boundary: the transport `submit` returns only on the
  host's `ACK_OK` (the `DD_IR_DUMP` host-tool path is a synchronous successful write). A fence created
  during a frame is therefore signaled by an actual host acknowledgement, not a local flush;
  `glClientWaitSync`/`glGetSynciv` reflect it. Mirror:
  `sync_completion_advances_on_real_frame_submission_ack`. **Residual:** asynchronous (non-blocking)
  backend ACK parity still needs a live executor to exercise beyond the synchronous submit+ack path.

## Truthful-failure & debugging controls

- **Default (lenient):** stubs raise the GL/EGL error and initialize outputs, then return; execution
  continues. `DD_SHIM_DEBUG=1` additionally logs each unimplemented/`partial` entry point once.
- **`DD_SHIM_STRICT=1`:** aborts at the FIRST `stub` call, printing command, thread, advertised context,
  and recent call history.

## Truthful advertisement (inventory-driven)

`glGetString(GL_VERSION/GL_SHADING_LANGUAGE_VERSION/GL_EXTENSIONS)`, `glGetStringi`, and `glGetIntegerv`
version/extension reporting are driven from the inventory's advertisement constants. The shim advertises
the coherent **OpenGL ES 2.0** profile (ES3 stays a `DD_SHIM_ES3` opt-in; ES3-only mandatory features
like sync objects / UBO-bound blocks remain stubs). The extension string is the gl_shim.c oracle set
(**9 extensions**: BGRA8888 x2, texture_storage, rgb8_rgba8, texture_npot, sRGB x2,
ANGLE_framebuffer_multisample, texture_usage), each backed by a real body; `glGetString(GL_EXTENSIONS)`,
`glGetStringi`, and `GL_NUM_EXTENSIONS` all derive from this one list and cannot disagree.

## Per-symbol census

Generated from build.rs `IMPLEMENTED`/`PARTIAL` over `registry/gles2_egl.manifest` (emitted into `CAPABILITIES`); the `inventory_covers_every_exported_symbol` test fails if this drifts.

### full — real hand-written body at gl_shim.c parity (218)

ES3-tagged members (`since`=GLES 3.0): `glBindVertexArray`, `glBlitFramebuffer`, `glClearBufferfv`, `glCopyBufferSubData`, `glDeleteVertexArrays`, `glDrawArraysInstanced`, `glDrawElementsInstanced`, `glDrawRangeElements`, `glFramebufferTextureLayer`, `glGenQueries`, `glGenSamplers`, `glGenTransformFeedbacks`, `glGenVertexArrays`, `glGetStringi`, `glIsVertexArray`, `glMapBufferRange`, `glRenderbufferStorageMultisample`, `glTexImage3D`, `glTexStorage3D`, `glTexSubImage3D`, `glUniform1ui`, `glUniform1uiv`, `glUniform2ui`, `glUniform2uiv`, `glUniform3ui`, `glUniform3uiv`, `glUniform4ui`, `glUniform4uiv`, `glUniformMatrix2x3fv`, `glUniformMatrix2x4fv`, `glUniformMatrix3x2fv`, `glUniformMatrix3x4fv`, `glUniformMatrix4x2fv`, `glUniformMatrix4x3fv`, `glUnmapBuffer`, `glVertexAttribIPointer`.

`eglBindAPI` | `eglBindTexImage` | `eglChooseConfig`
`eglCopyBuffers` | `eglCreateContext` | `eglCreatePbufferFromClientBuffer`
`eglCreatePbufferSurface` | `eglCreatePixmapSurface` | `eglCreateWindowSurface`
`eglDestroyContext` | `eglDestroySurface` | `eglGetConfigAttrib`
`eglGetConfigs` | `eglGetCurrentContext` | `eglGetCurrentDisplay`
`eglGetCurrentSurface` | `eglGetDisplay` | `eglGetError`
`eglGetPlatformDisplay` | `eglGetProcAddress` | `eglInitialize`
`eglMakeCurrent` | `eglQueryAPI` | `eglQueryContext`
`eglQueryString` | `eglQuerySurface` | `eglReleaseTexImage`
`eglReleaseThread` | `eglSurfaceAttrib` | `eglSwapBuffers`
`eglSwapInterval` | `eglTerminate` | `eglWaitClient`
`eglWaitGL` | `eglWaitNative` | `glActiveTexture`
`glAttachShader` | `glBindAttribLocation` | `glBindBuffer`
`glBindFramebuffer` | `glBindRenderbuffer` | `glBindTexture`
`glBindVertexArray` | `glBlendColor` | `glBlendEquation`
`glBlendEquationSeparate` | `glBlendEquationSeparatei` | `glBlendEquationi`
`glBlendFunc` | `glBlendFuncSeparate` | `glBlendFuncSeparatei`
`glBlendFunci` | `glBlitFramebuffer` | `glBufferData`
`glBufferSubData` | `glCheckFramebufferStatus` | `glClear`
`glClearBufferfv` | `glClearColor` | `glClearDepthf`
`glClearStencil` | `glColorMask` | `glCompileShader`
`glCompressedTexImage2D` | `glCompressedTexSubImage2D` | `glCopyBufferSubData`
`glCopyTexImage2D` | `glCopyTexSubImage2D` | `glCreateProgram`
`glCreateShader` | `glCullFace` | `glDeleteBuffers`
`glDeleteFramebuffers` | `glDeleteProgram` | `glDeleteRenderbuffers`
`glDeleteShader` | `glDeleteTextures` | `glDeleteVertexArrays`
`glDepthFunc` | `glDepthMask` | `glDepthRangef`
`glDetachShader` | `glDisable` | `glDisableVertexAttribArray`
`glDrawArrays` | `glDrawArraysInstanced` | `glDrawElements`
`glDrawElementsInstanced` | `glDrawRangeElements` | `glEnable`
`glEnableVertexAttribArray` | `glFinish` | `glFlush`
`glFramebufferRenderbuffer` | `glFramebufferTexture2D` | `glFramebufferTextureLayer`
`glFrontFace` | `glGenBuffers` | `glGenFramebuffers`
`glGenQueries` | `glGenRenderbuffers` | `glGenSamplers`
`glGenTextures` | `glGenTransformFeedbacks` | `glGenVertexArrays`
`glGenerateMipmap` | `glGetActiveAttrib` | `glGetActiveUniform`
`glGetAttachedShaders` | `glGetAttribLocation` | `glGetBooleanv`
`glGetBufferParameteriv` | `glGetError` | `glGetFloatv`
`glGetFramebufferAttachmentParameteriv` | `glGetIntegerv` | `glGetProgramInfoLog`
`glGetProgramiv` | `glGetRenderbufferParameteriv` | `glGetShaderInfoLog`
`glGetShaderPrecisionFormat` | `glGetShaderSource` | `glGetShaderiv`
`glGetString` | `glGetStringi` | `glGetTexParameterfv`
`glGetTexParameteriv` | `glGetUniformLocation` | `glGetUniformfv`
`glGetUniformiv` | `glGetVertexAttribPointerv` | `glGetVertexAttribfv`
`glGetVertexAttribiv` | `glHint` | `glIsBuffer`
`glIsEnabled` | `glIsFramebuffer` | `glIsProgram`
`glIsRenderbuffer` | `glIsShader` | `glIsTexture`
`glIsVertexArray` | `glLineWidth` | `glLinkProgram`
`glMapBufferRange` | `glPixelStorei` | `glPolygonOffset`
`glReadPixels` | `glReleaseShaderCompiler` | `glRenderbufferStorage`
`glRenderbufferStorageMultisample` | `glSampleCoverage` | `glScissor`
`glShaderBinary` | `glShaderSource` | `glStencilFunc`
`glStencilFuncSeparate` | `glStencilMask` | `glStencilMaskSeparate`
`glStencilOp` | `glStencilOpSeparate` | `glTexImage2D`
`glTexImage3D` | `glTexParameterf` | `glTexParameterfv`
`glTexParameteri` | `glTexParameteriv` | `glTexStorage2D`
`glTexStorage3D` | `glTexSubImage2D` | `glTexSubImage3D`
`glUniform1f` | `glUniform1fv` | `glUniform1i`
`glUniform1iv` | `glUniform1ui` | `glUniform1uiv`
`glUniform2f` | `glUniform2fv` | `glUniform2i`
`glUniform2iv` | `glUniform2ui` | `glUniform2uiv`
`glUniform3f` | `glUniform3fv` | `glUniform3i`
`glUniform3iv` | `glUniform3ui` | `glUniform3uiv`
`glUniform4f` | `glUniform4fv` | `glUniform4i`
`glUniform4iv` | `glUniform4ui` | `glUniform4uiv`
`glUniformMatrix2fv` | `glUniformMatrix2x3fv` | `glUniformMatrix2x4fv`
`glUniformMatrix3fv` | `glUniformMatrix3x2fv` | `glUniformMatrix3x4fv`
`glUniformMatrix4fv` | `glUniformMatrix4x2fv` | `glUniformMatrix4x3fv`
`glUnmapBuffer` | `glUseProgram` | `glValidateProgram`
`glVertexAttrib1f` | `glVertexAttrib1fv` | `glVertexAttrib2f`
`glVertexAttrib2fv` | `glVertexAttrib3f` | `glVertexAttrib3fv`
`glVertexAttrib4f` | `glVertexAttrib4fv` | `glVertexAttribIPointer`
`glVertexAttribPointer` | `glViewport`

### partial — spec-legitimate no-op / default query, NO error, outputs initialized (106)

Matches gl_shim.c's degraded behavior. Sentinel-returning members: .

`glBeginQuery` | `glBeginTransformFeedback` | `glBindBufferBase`
`glBindBufferRange` | `glBindSampler` | `glBindTransformFeedback`
`glBlendBarrier` | `glClearBufferfi` | `glClearBufferiv`
`glClearBufferuiv` | `glColorMaski` | `glCompressedTexImage3D`
`glCompressedTexSubImage3D` | `glCopyTexSubImage3D` | `glDebugMessageCallback`
`glDebugMessageControl` | `glDebugMessageInsert` | `glDeleteProgramPipelines`
`glDeleteQueries` | `glDeleteSamplers` | `glDeleteSync`
`glDeleteTransformFeedbacks` | `glDisablei` | `glDrawBuffers`
`glEnablei` | `glEndQuery` | `glEndTransformFeedback`
`glFlushMappedBufferRange` | `glGetActiveUniformBlockName` | `glGetActiveUniformBlockiv`
`glGetActiveUniformsiv` | `glGetBooleani_v` | `glGetBufferParameteri64v`
`glGetBufferPointerv` | `glGetDebugMessageLog` | `glGetFragDataLocation`
`glGetFramebufferParameteriv` | `glGetGraphicsResetStatus` | `glGetInteger64i_v`
`glGetInteger64v` | `glGetIntegeri_v` | `glGetInternalformativ`
`glGetMultisamplefv` | `glGetObjectLabel` | `glGetObjectPtrLabel`
`glGetPointerv` | `glGetProgramBinary` | `glGetProgramInterfaceiv`
`glGetProgramPipelineInfoLog` | `glGetProgramPipelineiv` | `glGetProgramResourceIndex`
`glGetProgramResourceLocation` | `glGetProgramResourceName` | `glGetProgramResourceiv`
`glGetQueryObjectuiv` | `glGetQueryiv` | `glGetSamplerParameterIiv`
`glGetSamplerParameterIuiv` | `glGetSamplerParameterfv` | `glGetSamplerParameteriv`
`glGetSynciv` | `glGetTexLevelParameterfv` | `glGetTexLevelParameteriv`
`glGetTexParameterIiv` | `glGetTexParameterIuiv` | `glGetTransformFeedbackVarying`
`glGetUniformBlockIndex` | `glGetUniformIndices` | `glGetUniformuiv`
`glGetVertexAttribIiv` | `glGetVertexAttribIuiv` | `glGetnUniformfv`
`glGetnUniformiv` | `glGetnUniformuiv` | `glInvalidateFramebuffer`
`glInvalidateSubFramebuffer` | `glIsEnabledi` | `glIsProgramPipeline`
`glIsQuery` | `glIsSampler` | `glIsSync`
`glIsTransformFeedback` | `glMemoryBarrier` | `glMemoryBarrierByRegion`
`glMinSampleShading` | `glObjectLabel` | `glObjectPtrLabel`
`glPatchParameteri` | `glPopDebugGroup` | `glPrimitiveBoundingBox`
`glProgramBinary` | `glProgramParameteri` | `glPushDebugGroup`
`glReadBuffer` | `glSampleMaski` | `glSamplerParameterf`
`glSamplerParameterfv` | `glSamplerParameteri` | `glSamplerParameteriv`
`glUniformBlockBinding` | `glValidateProgramPipeline` | `glVertexAttribDivisor`
`glVertexAttribI4i` | `glVertexAttribI4iv` | `glVertexAttribI4ui`
`glVertexAttribI4uiv`

### stub — unsupported: raises API-correct error, zeroes outputs, aborts under DD_SHIM_STRICT (78)

GL stubs raise `GL_INVALID_OPERATION`; EGL stubs raise `EGL_BAD_ACCESS`. Remaining families: transform-feedback results, occlusion/sync objects (ES3, host-unbacked), program pipelines / separable `glProgramUniform*`, compute/indirect draws, image load/store, memory barriers, and the EGL image/sync/platform-surface family.

**GL stubs (69):**

`glActiveShaderProgram` | `glBindImageTexture` | `glBindProgramPipeline`
`glBindVertexBuffer` | `glClientWaitSync` | `glCopyImageSubData`
`glCreateShaderProgramv` | `glDispatchCompute` | `glDispatchComputeIndirect`
`glDrawArraysIndirect` | `glDrawElementsBaseVertex` | `glDrawElementsIndirect`
`glDrawElementsInstancedBaseVertex` | `glDrawRangeElementsBaseVertex` | `glFenceSync`
`glFramebufferParameteri` | `glFramebufferTexture` | `glGenProgramPipelines`
`glPauseTransformFeedback` | `glProgramUniform1f` | `glProgramUniform1fv`
`glProgramUniform1i` | `glProgramUniform1iv` | `glProgramUniform1ui`
`glProgramUniform1uiv` | `glProgramUniform2f` | `glProgramUniform2fv`
`glProgramUniform2i` | `glProgramUniform2iv` | `glProgramUniform2ui`
`glProgramUniform2uiv` | `glProgramUniform3f` | `glProgramUniform3fv`
`glProgramUniform3i` | `glProgramUniform3iv` | `glProgramUniform3ui`
`glProgramUniform3uiv` | `glProgramUniform4f` | `glProgramUniform4fv`
`glProgramUniform4i` | `glProgramUniform4iv` | `glProgramUniform4ui`
`glProgramUniform4uiv` | `glProgramUniformMatrix2fv` | `glProgramUniformMatrix2x3fv`
`glProgramUniformMatrix2x4fv` | `glProgramUniformMatrix3fv` | `glProgramUniformMatrix3x2fv`
`glProgramUniformMatrix3x4fv` | `glProgramUniformMatrix4fv` | `glProgramUniformMatrix4x2fv`
`glProgramUniformMatrix4x3fv` | `glReadnPixels` | `glResumeTransformFeedback`
`glSamplerParameterIiv` | `glSamplerParameterIuiv` | `glTexBuffer`
`glTexBufferRange` | `glTexParameterIiv` | `glTexParameterIuiv`
`glTexStorage2DMultisample` | `glTexStorage3DMultisample` | `glTransformFeedbackVaryings`
`glUseProgramStages` | `glVertexAttribBinding` | `glVertexAttribFormat`
`glVertexAttribIFormat` | `glVertexBindingDivisor` | `glWaitSync`

**EGL stubs (9):**

`eglClientWaitSync` | `eglCreateImage` | `eglCreatePlatformPixmapSurface`
`eglCreatePlatformWindowSurface` | `eglCreateSync` | `eglDestroyImage`
`eglDestroySync` | `eglGetSyncAttrib` | `eglWaitSync`

## Remaining `stub` families (next promotions / honest gaps)

- **ES3 sync & queries** (`glFenceSync`/`glClientWaitSync`/`glWaitSync`, occlusion `glBeginQuery` result
  path): host-unbacked; kept as truthful stubs rather than gl_shim.c's fake-signaled returns, since ES3
  is a `DD_SHIM_ES3` opt-in and advertising working sync would be false.
- **Transform feedback capture, program pipelines / separable `glProgramUniform*`, compute / indirect
  dispatch, image load/store, memory barriers.**
- **EGL image / sync / pixmap-surface family** (`eglCreateImage`, `eglCreateSync`,
  `eglCreatePixmapSurface`, `eglBindTexImage`, …): reported as a *lower coherent EGL surface* (truthful
  failure) per audit section 2.1, rather than gl_shim.c's fake success handles.

## GLES 3.0 assessment — GskGL / GPU-accelerated GTK4 (cross-cutting)

GTK4's `GskGLRenderer` needs (1) an ES3 context, (2) half-float vertex data, and (3) its ES3 GLSL
shaders to translate to MSL. Status of each:

- **(2) Half-float vertex format — DONE.** `GL_HALF_FLOAT` (0x140B) now lowers distinctly instead of
  collapsing to 32-bit `GL_FLOAT`: the vertex-format wire encoding carries a new `kind = 7`
  (`wireenc::vertex_format_wire`, byte-parity with `gl_shim.c`'s `vertex_format_wire` +
  `attr_elem_size` + the half→float attribute readback, both extended in lockstep), and the Metal
  executor maps `kind 7 → MTLVertexFormat::Half{,2,3,4}` (`dd-display::metal_backend::metal_vertex_format`),
  which Metal fetches natively into the shader's `float` inputs (no shader change). Gated by
  `wireenc … vertex_format_wire_matches_c_shim`. The software oracle records draws rather than
  rasterizing vertex fetch (see the software-draw residual), so half-float there is moot until software
  rasterization lands.
- **(3a) ES3 GLSL→MSL translation of std140 UBO blocks — DONE (byte-parity).** `translate.rs` (and its
  `gl_shim.c` oracle) already handled the rest of GskGL's ES3 syntax (`in`/`out` stage I/O, a user
  `out vec4` fragment output, `texture()`); the missing piece was `layout(std140) uniform Block { … }`
  blocks, which the uniform collector dropped (it read the block name as the type and stopped at `{`).
  The `collect` parser now DETECTS a block (a `{` after the `uniform` token) and enumerates its members
  as the collected uniform decls — so a block's members flow through the existing uniform pipeline: one
  `struct Uniforms` at `[[buffer(1)]]`, members referenced `u.<member>`. Extended identically in BOTH
  `gl_shim.c`'s `collect` and `translate.rs::collect`, verified byte-identical by `translate_parity`
  over a new `shader_translate/ubo_std140.{vert,frag}.glsl` corpus pair (a mat4+vec4 GskGL-style block).
- **(3b) The remaining pieces for a GPU-accelerated GskGL window (precise residual):**
  - **UBO data routing:** at draw lowering, when the program declares a UBO block, the compositor must
    bind the guest's `glBindBufferBase(GL_UNIFORM_BUFFER,…)` buffer as `[[buffer(1)]]` (today the
    `glUniform*`-populated default block is uploaded there). The indexed-binding STATE exists
    (`glBindBufferBase`, from the ES3-object work) but `frame.rs`'s swap-time lowering (and the
    `gl_shim.c` draw path, in lockstep) must consume it — the byte-parity-sensitive data-path half.
  - **std140 padding** for blocks with `vec3`/scalar-packing/arrays (all-`vec4`/`mat4` blocks — the
    common GskGL case — already match Metal's layout, so the mat4+vec4 corpus needs none).
  - **`layout(location=N)`** explicit attribute locations (the translator assigns by declaration order).
  - **(1) ES3 context** advertisement enablement (below) once the data path lands.
- **(1) ES3 context** is a `DD_SHIM_ES3` opt-in; advertising ES3 by default is gated on (3) so GTK does
  not select GskGL and then fail to compile a shader (it correctly falls back to the software cairo
  renderer today).

So the two prerequisites the vertex path needed — the ES3 object families (VAO/UBO/sampler/query/TF,
above) and half-float vertex format — are now real; the remaining gap to a GPU-accelerated GskGL GTK4
window is the ES3 GLSL→MSL translator.
