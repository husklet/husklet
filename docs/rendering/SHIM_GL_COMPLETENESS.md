# dd-shim-gl GLES2/EGL completeness audit

The Rust shim exports the **complete 402-symbol** GLES2+EGL surface (registry-generated: 358 GL + 44
EGL). Every symbol resolves with the correct C ABI. This audit is the **generated capability inventory**
(`CAPABILITIES` in the build, one record per symbol) that classifies each entry point by *behavioral*
truthfulness. It is emitted by `build.rs` from the SAME `IMPLEMENTED`/`PARTIAL` classification the
runtime uses, so it can never drift from the exported surface — the `inventory_covers_every_exported_
symbol` test asserts a 1:1 correspondence and that every level's invariants hold.

## Capability levels (Phase-0 truthfulness model)

| Level | Count | Behavior | Error state |
|---|---|---|---|
| **full** | 166 | Real hand-written body at gl_shim.c parity (byte-identical IR / faithful state). | none |
| **partial** | 110 | Spec-legitimate no-op / default query that matches gl_shim.c's own degraded behavior. ALWAYS initializes its outputs; returns the spec default or the correct not-found sentinel. | none (a no-op is the correct answer) |
| **stub** | 126 | An operation a conforming driver performs and the shim does NOT: it FAILS truthfully — sets the API-correct GL/EGL error, zero-initializes every output, returns the spec failure value, and aborts under `DD_SHIM_STRICT`. | `glGetError`/`eglGetError` raised |

By lib: GL = 138 full / 109 partial / 111 stub; EGL = 28 full / 1 partial / 15 stub.

**The Phase-0 exit gate holds:** no unsupported entry point can silently report success. A `stub` never
returns a success-by-default — it raises an API-correct error and initializes outputs; a `partial`
never claims to have done work it didn't. The old behavior (236 generated stubs returning benign
zero/null with no error) is gone.

## Truthful-failure & debugging controls

- **Default (lenient):** stubs raise the GL/EGL error and initialize outputs, then return; execution
  continues. `DD_SHIM_DEBUG=1` additionally logs each unimplemented/`partial` entry point once, the
  first time an app calls it (exploratory "what does this app actually use").
- **`DD_SHIM_STRICT=1`:** aborts at the FIRST `stub` call, printing the command, thread, advertised
  context, and the recent call history — an unsupported call fails loudly instead of degrading.

## Truthful advertisement (Phase 1.1 slice)

`glGetString(GL_VERSION/GL_SHADING_LANGUAGE_VERSION/GL_EXTENSIONS)` and `glGetIntegerv` version/extension
reporting are driven from the inventory's advertisement constants (`ADVERTISED_GL_VERSION`, …). The shim
advertises the **lowest coherent GLES profile the real bodies actually back — OpenGL ES 2.0** — and does
NOT claim ES 3.x merely because the ES3 symbols exist: the ES3 core additions (UBO binding, transform
feedback, fence sync, sampler objects, immutable/3D textures, MRT) are all `stub`s. ES3 remains an
explicit `DD_SHIM_ES3` opt-in (which advertises the ES3 identity strings on a context created with
major>=3). The extension string lists only host-backed extensions (`GL_OES_element_index_uint`,
`GL_OES_texture_npot`), and `GL_NUM_EXTENSIONS` is derived from that list.

## Per-symbol census

The tables below are generated from the SAME source of truth as the runtime (build.rs `IMPLEMENTED`/`PARTIAL` sets over `registry/gles2_egl.manifest`), emitted into `CAPABILITIES`. Regenerate with `dd-shim-gl/registry` tooling; the `inventory_covers_every_exported_symbol` test fails if this drifts from the exported surface.

### full — real hand-written body at gl_shim.c parity (166)

EGL lifecycle/present + GLES state/buffers/textures/shaders/uniforms/VAOs/FBOs/draw-recording. ES3-tagged members: `glBindVertexArray`, `glBlitFramebuffer`, `glClearBufferfv`, `glDeleteVertexArrays`, `glDrawArraysInstanced`, `glDrawElementsInstanced`, `glFramebufferTextureLayer`, `glGenVertexArrays`, `glIsVertexArray`, `glRenderbufferStorageMultisample`, `glUniform1ui`, `glUniform1uiv`, `glUniform2ui`, `glUniform2uiv`, `glUniform3ui`, `glUniform3uiv`, `glUniform4ui`, `glUniform4uiv`, `glUniformMatrix2x3fv`, `glUniformMatrix2x4fv`, `glUniformMatrix3x2fv`, `glUniformMatrix3x4fv`, `glUniformMatrix4x2fv`, `glUniformMatrix4x3fv`.

`eglBindAPI` | `eglChooseConfig` | `eglCreateContext`
`eglCreatePbufferSurface` | `eglCreateWindowSurface` | `eglDestroyContext`
`eglDestroySurface` | `eglGetConfigAttrib` | `eglGetConfigs`
`eglGetCurrentContext` | `eglGetCurrentDisplay` | `eglGetCurrentSurface`
`eglGetDisplay` | `eglGetError` | `eglGetProcAddress`
`eglInitialize` | `eglMakeCurrent` | `eglQueryAPI`
`eglQueryContext` | `eglQueryString` | `eglQuerySurface`
`eglReleaseThread` | `eglSwapBuffers` | `eglSwapInterval`
`eglTerminate` | `eglWaitClient` | `eglWaitGL`
`eglWaitNative` | `glActiveTexture` | `glAttachShader`
`glBindBuffer` | `glBindFramebuffer` | `glBindRenderbuffer`
`glBindTexture` | `glBindVertexArray` | `glBlendColor`
`glBlendEquation` | `glBlendEquationSeparate` | `glBlendEquationSeparatei`
`glBlendEquationi` | `glBlendFunc` | `glBlendFuncSeparate`
`glBlendFuncSeparatei` | `glBlendFunci` | `glBlitFramebuffer`
`glBufferData` | `glBufferSubData` | `glCheckFramebufferStatus`
`glClear` | `glClearBufferfv` | `glClearColor`
`glClearDepthf` | `glClearStencil` | `glColorMask`
`glCompileShader` | `glCreateProgram` | `glCreateShader`
`glCullFace` | `glDeleteBuffers` | `glDeleteFramebuffers`
`glDeleteProgram` | `glDeleteRenderbuffers` | `glDeleteShader`
`glDeleteTextures` | `glDeleteVertexArrays` | `glDepthFunc`
`glDepthMask` | `glDepthRangef` | `glDetachShader`
`glDisable` | `glDisableVertexAttribArray` | `glDrawArrays`
`glDrawArraysInstanced` | `glDrawElements` | `glDrawElementsInstanced`
`glEnable` | `glEnableVertexAttribArray` | `glFinish`
`glFlush` | `glFramebufferRenderbuffer` | `glFramebufferTexture2D`
`glFramebufferTextureLayer` | `glFrontFace` | `glGenBuffers`
`glGenFramebuffers` | `glGenRenderbuffers` | `glGenTextures`
`glGenVertexArrays` | `glGenerateMipmap` | `glGetAttribLocation`
`glGetBooleanv` | `glGetError` | `glGetFloatv`
`glGetFramebufferAttachmentParameteriv` | `glGetIntegerv` | `glGetProgramInfoLog`
`glGetProgramiv` | `glGetRenderbufferParameteriv` | `glGetShaderInfoLog`
`glGetShaderiv` | `glGetString` | `glGetUniformLocation`
`glHint` | `glIsBuffer` | `glIsEnabled`
`glIsFramebuffer` | `glIsProgram` | `glIsRenderbuffer`
`glIsShader` | `glIsTexture` | `glIsVertexArray`
`glLineWidth` | `glLinkProgram` | `glPixelStorei`
`glPolygonOffset` | `glReadPixels` | `glRenderbufferStorage`
`glRenderbufferStorageMultisample` | `glSampleCoverage` | `glScissor`
`glShaderSource` | `glStencilFunc` | `glStencilMask`
`glStencilOp` | `glTexImage2D` | `glTexParameterf`
`glTexParameterfv` | `glTexParameteri` | `glTexParameteriv`
`glTexSubImage2D` | `glUniform1f` | `glUniform1fv`
`glUniform1i` | `glUniform1iv` | `glUniform1ui`
`glUniform1uiv` | `glUniform2f` | `glUniform2fv`
`glUniform2i` | `glUniform2iv` | `glUniform2ui`
`glUniform2uiv` | `glUniform3f` | `glUniform3fv`
`glUniform3i` | `glUniform3iv` | `glUniform3ui`
`glUniform3uiv` | `glUniform4f` | `glUniform4fv`
`glUniform4i` | `glUniform4iv` | `glUniform4ui`
`glUniform4uiv` | `glUniformMatrix2fv` | `glUniformMatrix2x3fv`
`glUniformMatrix2x4fv` | `glUniformMatrix3fv` | `glUniformMatrix3x2fv`
`glUniformMatrix3x4fv` | `glUniformMatrix4fv` | `glUniformMatrix4x2fv`
`glUniformMatrix4x3fv` | `glUseProgram` | `glVertexAttribPointer`
`glViewport`

### partial — spec-legitimate no-op / default query, NO error, outputs initialized (110)

Matches gl_shim.c's degraded behavior. Introspection getters return spec defaults (0/empty); `glIs*`/location/index queries return the correct not-found sentinel (`GL_FALSE` / `-1` / `GL_INVALID_INDEX`); optional state (constant vertex attribs, separate-stencil, debug labels, hints) no-ops; `glDelete*` of never-issued objects is silently ignored per spec. Sentinel-returning members: .

`eglSurfaceAttrib` | `glBindAttribLocation` | `glBlendBarrier`
`glColorMaski` | `glDebugMessageCallback` | `glDebugMessageControl`
`glDebugMessageInsert` | `glDeleteProgramPipelines` | `glDeleteQueries`
`glDeleteSamplers` | `glDeleteSync` | `glDeleteTransformFeedbacks`
`glDisablei` | `glEnablei` | `glGetActiveAttrib`
`glGetActiveUniform` | `glGetActiveUniformBlockName` | `glGetActiveUniformBlockiv`
`glGetActiveUniformsiv` | `glGetAttachedShaders` | `glGetBooleani_v`
`glGetBufferParameteri64v` | `glGetBufferParameteriv` | `glGetBufferPointerv`
`glGetDebugMessageLog` | `glGetFragDataLocation` | `glGetFramebufferParameteriv`
`glGetGraphicsResetStatus` | `glGetInteger64i_v` | `glGetInteger64v`
`glGetIntegeri_v` | `glGetInternalformativ` | `glGetMultisamplefv`
`glGetObjectLabel` | `glGetObjectPtrLabel` | `glGetPointerv`
`glGetProgramBinary` | `glGetProgramInterfaceiv` | `glGetProgramPipelineInfoLog`
`glGetProgramPipelineiv` | `glGetProgramResourceIndex` | `glGetProgramResourceLocation`
`glGetProgramResourceName` | `glGetProgramResourceiv` | `glGetQueryObjectuiv`
`glGetQueryiv` | `glGetSamplerParameterIiv` | `glGetSamplerParameterIuiv`
`glGetSamplerParameterfv` | `glGetSamplerParameteriv` | `glGetShaderPrecisionFormat`
`glGetShaderSource` | `glGetSynciv` | `glGetTexLevelParameterfv`
`glGetTexLevelParameteriv` | `glGetTexParameterIiv` | `glGetTexParameterIuiv`
`glGetTexParameterfv` | `glGetTexParameteriv` | `glGetTransformFeedbackVarying`
`glGetUniformBlockIndex` | `glGetUniformIndices` | `glGetUniformfv`
`glGetUniformiv` | `glGetUniformuiv` | `glGetVertexAttribIiv`
`glGetVertexAttribIuiv` | `glGetVertexAttribPointerv` | `glGetVertexAttribfv`
`glGetVertexAttribiv` | `glGetnUniformfv` | `glGetnUniformiv`
`glGetnUniformuiv` | `glInvalidateFramebuffer` | `glInvalidateSubFramebuffer`
`glIsEnabledi` | `glIsProgramPipeline` | `glIsQuery`
`glIsSampler` | `glIsSync` | `glIsTransformFeedback`
`glMemoryBarrier` | `glMemoryBarrierByRegion` | `glMinSampleShading`
`glObjectLabel` | `glObjectPtrLabel` | `glPatchParameteri`
`glPopDebugGroup` | `glPrimitiveBoundingBox` | `glProgramParameteri`
`glPushDebugGroup` | `glReleaseShaderCompiler` | `glSampleMaski`
`glStencilFuncSeparate` | `glStencilMaskSeparate` | `glStencilOpSeparate`
`glValidateProgram` | `glValidateProgramPipeline` | `glVertexAttrib1f`
`glVertexAttrib1fv` | `glVertexAttrib2f` | `glVertexAttrib2fv`
`glVertexAttrib3f` | `glVertexAttrib3fv` | `glVertexAttrib4f`
`glVertexAttrib4fv` | `glVertexAttribI4i` | `glVertexAttribI4iv`
`glVertexAttribI4ui` | `glVertexAttribI4uiv`

### stub — unsupported operation: raises API-correct error, zeroes outputs, returns failure value, aborts under DD_SHIM_STRICT (126)

These do real work in a conforming driver; the shim does NOT implement them, so they FAIL truthfully instead of silently degrading. GL stubs raise `GL_INVALID_OPERATION` (`glShaderBinary`: `GL_INVALID_ENUM`); EGL stubs raise `EGL_BAD_ACCESS`. Families: transform feedback, occlusion/sync objects, sampler objects, program pipelines/separable `glProgramUniform*`, compute/indirect draws, immutable/3D/compressed/copy textures, buffer mapping, UBO binding (`glBindBufferBase/Range`, `glUniformBlockBinding`), MRT clears, instanced-vertex/format-binding attribs, and the EGL image/sync/platform-surface family.

**GL stubs (111):**

`glActiveShaderProgram` | `glBeginQuery` | `glBeginTransformFeedback`
`glBindBufferBase` | `glBindBufferRange` | `glBindImageTexture`
`glBindProgramPipeline` | `glBindSampler` | `glBindTransformFeedback`
`glBindVertexBuffer` | `glClearBufferfi` | `glClearBufferiv`
`glClearBufferuiv` | `glClientWaitSync` | `glCompressedTexImage2D`
`glCompressedTexImage3D` | `glCompressedTexSubImage2D` | `glCompressedTexSubImage3D`
`glCopyBufferSubData` | `glCopyImageSubData` | `glCopyTexImage2D`
`glCopyTexSubImage2D` | `glCopyTexSubImage3D` | `glCreateShaderProgramv`
`glDispatchCompute` | `glDispatchComputeIndirect` | `glDrawArraysIndirect`
`glDrawBuffers` | `glDrawElementsBaseVertex` | `glDrawElementsIndirect`
`glDrawElementsInstancedBaseVertex` | `glDrawRangeElements` | `glDrawRangeElementsBaseVertex`
`glEndQuery` | `glEndTransformFeedback` | `glFenceSync`
`glFlushMappedBufferRange` | `glFramebufferParameteri` | `glFramebufferTexture`
`glGenProgramPipelines` | `glGenQueries` | `glGenSamplers`
`glGenTransformFeedbacks` | `glGetStringi` | `glMapBufferRange`
`glPauseTransformFeedback` | `glProgramBinary` | `glProgramUniform1f`
`glProgramUniform1fv` | `glProgramUniform1i` | `glProgramUniform1iv`
`glProgramUniform1ui` | `glProgramUniform1uiv` | `glProgramUniform2f`
`glProgramUniform2fv` | `glProgramUniform2i` | `glProgramUniform2iv`
`glProgramUniform2ui` | `glProgramUniform2uiv` | `glProgramUniform3f`
`glProgramUniform3fv` | `glProgramUniform3i` | `glProgramUniform3iv`
`glProgramUniform3ui` | `glProgramUniform3uiv` | `glProgramUniform4f`
`glProgramUniform4fv` | `glProgramUniform4i` | `glProgramUniform4iv`
`glProgramUniform4ui` | `glProgramUniform4uiv` | `glProgramUniformMatrix2fv`
`glProgramUniformMatrix2x3fv` | `glProgramUniformMatrix2x4fv` | `glProgramUniformMatrix3fv`
`glProgramUniformMatrix3x2fv` | `glProgramUniformMatrix3x4fv` | `glProgramUniformMatrix4fv`
`glProgramUniformMatrix4x2fv` | `glProgramUniformMatrix4x3fv` | `glReadBuffer`
`glReadnPixels` | `glResumeTransformFeedback` | `glSamplerParameterIiv`
`glSamplerParameterIuiv` | `glSamplerParameterf` | `glSamplerParameterfv`
`glSamplerParameteri` | `glSamplerParameteriv` | `glShaderBinary`
`glTexBuffer` | `glTexBufferRange` | `glTexImage3D`
`glTexParameterIiv` | `glTexParameterIuiv` | `glTexStorage2D`
`glTexStorage2DMultisample` | `glTexStorage3D` | `glTexStorage3DMultisample`
`glTexSubImage3D` | `glTransformFeedbackVaryings` | `glUniformBlockBinding`
`glUnmapBuffer` | `glUseProgramStages` | `glVertexAttribBinding`
`glVertexAttribDivisor` | `glVertexAttribFormat` | `glVertexAttribIFormat`
`glVertexAttribIPointer` | `glVertexBindingDivisor` | `glWaitSync`

**EGL stubs (15):**

`eglBindTexImage` | `eglClientWaitSync` | `eglCopyBuffers`
`eglCreateImage` | `eglCreatePbufferFromClientBuffer` | `eglCreatePixmapSurface`
`eglCreatePlatformPixmapSurface` | `eglCreatePlatformWindowSurface` | `eglCreateSync`
`eglDestroyImage` | `eglDestroySync` | `eglGetPlatformDisplay`
`eglGetSyncAttrib` | `eglReleaseTexImage` | `eglWaitSync`

## (b) Real bodies still needed (highest app impact, currently `stub`)

These do meaningful work in gl_shim.c; today they fail truthfully (raise `GL_INVALID_OPERATION`).
Ordered by app impact — these are the next `stub`->`full` ports:

1. **Texture storage / 3D / compressed** — `glTexStorage2D`/`3D`, `glTexImage3D`/`glTexSubImage3D`,
   `glCompressedTexImage2D`/`…`. Used by GTK4 / Chrome for immutable textures.
2. **Pixel readback** — `glReadnPixels` (bounded `glReadPixels`); needs a synchronous round-trip.
3. **Buffer mapping / UBO binding** — `glMapBufferRange`/`glUnmapBuffer`/`glFlushMappedBufferRange`,
   `glBindBufferRange`/`glBindBufferBase`/`glUniformBlockBinding` (GTK4/Chrome ES3 path).

## GLES 3.0 assessment (cross-cutting, not implemented)

GTK4's `GskGLRenderer`/`GskNglRenderer` take the GPU path only against an **ES 3.0** context and upload
**half-float** vertex data. Making ES3 real is a three-layer change (guest gl_shim.c + guest dd-shim-gl
+ host dd-gpu/Metal): advertise ES3 (guest, gated by `DD_SHIM_ES3`), then real UBO binding + the
half-float vertex format (a coordinated guest+host change to `wireenc` + the dd-gpu `VertexFormat` enum
+ Metal descriptor). Until those land, the shim advertises ES2 by default (GTK falls back to software,
correctly) rather than claiming ES3 the host can't back — see the advertisement section above.
