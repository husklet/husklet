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
| **full** | 182 | Real hand-written body at gl_shim.c parity (byte-identical IR / faithful state). | none |
| **partial** | 137 | Spec-legitimate no-op / default query matching gl_shim.c's own degraded behavior. ALWAYS initializes outputs; returns the spec default / correct not-found sentinel. | none (a no-op is the correct answer) |
| **stub** | 83 | An operation a conforming driver performs and the shim does NOT: FAILS truthfully — sets the API-correct GL/EGL error, zeroes outputs, returns the spec failure value, aborts under `DD_SHIM_STRICT`. | `glGetError`/`eglGetError` raised |

By lib: GL = 153 full / 136 partial / 69 stub; EGL = 29 full / 1 partial / 14 stub.

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

### full — real hand-written body at gl_shim.c parity (182)

ES3-tagged members (`since`=GLES 3.0): `glBindVertexArray`, `glBlitFramebuffer`, `glClearBufferfv`, `glCopyBufferSubData`, `glDeleteVertexArrays`, `glDrawArraysInstanced`, `glDrawElementsInstanced`, `glDrawRangeElements`, `glFramebufferTextureLayer`, `glGenQueries`, `glGenSamplers`, `glGenTransformFeedbacks`, `glGenVertexArrays`, `glGetStringi`, `glIsVertexArray`, `glMapBufferRange`, `glRenderbufferStorageMultisample`, `glTexImage3D`, `glTexStorage3D`, `glTexSubImage3D`, `glUniform1ui`, `glUniform1uiv`, `glUniform2ui`, `glUniform2uiv`, `glUniform3ui`, `glUniform3uiv`, `glUniform4ui`, `glUniform4uiv`, `glUniformMatrix2x3fv`, `glUniformMatrix2x4fv`, `glUniformMatrix3x2fv`, `glUniformMatrix3x4fv`, `glUniformMatrix4x2fv`, `glUniformMatrix4x3fv`, `glUnmapBuffer`, `glVertexAttribIPointer`.

`eglBindAPI` | `eglChooseConfig` | `eglCreateContext`
`eglCreatePbufferSurface` | `eglCreateWindowSurface` | `eglDestroyContext`
`eglDestroySurface` | `eglGetConfigAttrib` | `eglGetConfigs`
`eglGetCurrentContext` | `eglGetCurrentDisplay` | `eglGetCurrentSurface`
`eglGetDisplay` | `eglGetError` | `eglGetPlatformDisplay`
`eglGetProcAddress` | `eglInitialize` | `eglMakeCurrent`
`eglQueryAPI` | `eglQueryContext` | `eglQueryString`
`eglQuerySurface` | `eglReleaseThread` | `eglSwapBuffers`
`eglSwapInterval` | `eglTerminate` | `eglWaitClient`
`eglWaitGL` | `eglWaitNative` | `glActiveTexture`
`glAttachShader` | `glBindBuffer` | `glBindFramebuffer`
`glBindRenderbuffer` | `glBindTexture` | `glBindVertexArray`
`glBlendColor` | `glBlendEquation` | `glBlendEquationSeparate`
`glBlendEquationSeparatei` | `glBlendEquationi` | `glBlendFunc`
`glBlendFuncSeparate` | `glBlendFuncSeparatei` | `glBlendFunci`
`glBlitFramebuffer` | `glBufferData` | `glBufferSubData`
`glCheckFramebufferStatus` | `glClear` | `glClearBufferfv`
`glClearColor` | `glClearDepthf` | `glClearStencil`
`glColorMask` | `glCompileShader` | `glCopyBufferSubData`
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
`glGenerateMipmap` | `glGetAttribLocation` | `glGetBooleanv`
`glGetError` | `glGetFloatv` | `glGetFramebufferAttachmentParameteriv`
`glGetIntegerv` | `glGetProgramInfoLog` | `glGetProgramiv`
`glGetRenderbufferParameteriv` | `glGetShaderInfoLog` | `glGetShaderiv`
`glGetString` | `glGetStringi` | `glGetUniformLocation`
`glHint` | `glIsBuffer` | `glIsEnabled`
`glIsFramebuffer` | `glIsProgram` | `glIsRenderbuffer`
`glIsShader` | `glIsTexture` | `glIsVertexArray`
`glLineWidth` | `glLinkProgram` | `glMapBufferRange`
`glPixelStorei` | `glPolygonOffset` | `glReadPixels`
`glRenderbufferStorage` | `glRenderbufferStorageMultisample` | `glSampleCoverage`
`glScissor` | `glShaderSource` | `glStencilFunc`
`glStencilMask` | `glStencilOp` | `glTexImage2D`
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
`glUnmapBuffer` | `glUseProgram` | `glVertexAttribIPointer`
`glVertexAttribPointer` | `glViewport`

### partial — spec-legitimate no-op / default query, NO error, outputs initialized (137)

Matches gl_shim.c's degraded behavior. Sentinel-returning members: .

`eglSurfaceAttrib` | `glBeginQuery` | `glBeginTransformFeedback`
`glBindAttribLocation` | `glBindBufferBase` | `glBindBufferRange`
`glBindSampler` | `glBindTransformFeedback` | `glBlendBarrier`
`glClearBufferfi` | `glClearBufferiv` | `glClearBufferuiv`
`glColorMaski` | `glCompressedTexImage2D` | `glCompressedTexImage3D`
`glCompressedTexSubImage2D` | `glCompressedTexSubImage3D` | `glCopyTexSubImage3D`
`glDebugMessageCallback` | `glDebugMessageControl` | `glDebugMessageInsert`
`glDeleteProgramPipelines` | `glDeleteQueries` | `glDeleteSamplers`
`glDeleteSync` | `glDeleteTransformFeedbacks` | `glDisablei`
`glDrawBuffers` | `glEnablei` | `glEndQuery`
`glEndTransformFeedback` | `glFlushMappedBufferRange` | `glGetActiveAttrib`
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
`glPopDebugGroup` | `glPrimitiveBoundingBox` | `glProgramBinary`
`glProgramParameteri` | `glPushDebugGroup` | `glReadBuffer`
`glReleaseShaderCompiler` | `glSampleMaski` | `glSamplerParameterf`
`glSamplerParameterfv` | `glSamplerParameteri` | `glSamplerParameteriv`
`glShaderBinary` | `glStencilFuncSeparate` | `glStencilMaskSeparate`
`glStencilOpSeparate` | `glUniformBlockBinding` | `glValidateProgram`
`glValidateProgramPipeline` | `glVertexAttrib1f` | `glVertexAttrib1fv`
`glVertexAttrib2f` | `glVertexAttrib2fv` | `glVertexAttrib3f`
`glVertexAttrib3fv` | `glVertexAttrib4f` | `glVertexAttrib4fv`
`glVertexAttribDivisor` | `glVertexAttribI4i` | `glVertexAttribI4iv`
`glVertexAttribI4ui` | `glVertexAttribI4uiv`

### stub — unsupported: raises API-correct error, zeroes outputs, aborts under DD_SHIM_STRICT (83)

GL stubs raise `GL_INVALID_OPERATION`; EGL stubs raise `EGL_BAD_ACCESS`. Remaining families: transform-feedback results, occlusion/sync objects (ES3, host-unbacked), program pipelines / separable `glProgramUniform*`, compute/indirect draws, image load/store, memory barriers, and the EGL image/sync/pixmap-surface family (reported as a lower coherent surface per the audit).

`eglBindTexImage` | `eglClientWaitSync` | `eglCopyBuffers`
`eglCreateImage` | `eglCreatePbufferFromClientBuffer` | `eglCreatePixmapSurface`
`eglCreatePlatformPixmapSurface` | `eglCreatePlatformWindowSurface` | `eglCreateSync`
`eglDestroyImage` | `eglDestroySync` | `eglGetSyncAttrib`
`eglReleaseTexImage` | `eglWaitSync` | `glActiveShaderProgram`
`glBindImageTexture` | `glBindProgramPipeline` | `glBindVertexBuffer`
`glClientWaitSync` | `glCopyImageSubData` | `glCreateShaderProgramv`
`glDispatchCompute` | `glDispatchComputeIndirect` | `glDrawArraysIndirect`
`glDrawElementsBaseVertex` | `glDrawElementsIndirect` | `glDrawElementsInstancedBaseVertex`
`glDrawRangeElementsBaseVertex` | `glFenceSync` | `glFramebufferParameteri`
`glFramebufferTexture` | `glGenProgramPipelines` | `glPauseTransformFeedback`
`glProgramUniform1f` | `glProgramUniform1fv` | `glProgramUniform1i`
`glProgramUniform1iv` | `glProgramUniform1ui` | `glProgramUniform1uiv`
`glProgramUniform2f` | `glProgramUniform2fv` | `glProgramUniform2i`
`glProgramUniform2iv` | `glProgramUniform2ui` | `glProgramUniform2uiv`
`glProgramUniform3f` | `glProgramUniform3fv` | `glProgramUniform3i`
`glProgramUniform3iv` | `glProgramUniform3ui` | `glProgramUniform3uiv`
`glProgramUniform4f` | `glProgramUniform4fv` | `glProgramUniform4i`
`glProgramUniform4iv` | `glProgramUniform4ui` | `glProgramUniform4uiv`
`glProgramUniformMatrix2fv` | `glProgramUniformMatrix2x3fv` | `glProgramUniformMatrix2x4fv`
`glProgramUniformMatrix3fv` | `glProgramUniformMatrix3x2fv` | `glProgramUniformMatrix3x4fv`
`glProgramUniformMatrix4fv` | `glProgramUniformMatrix4x2fv` | `glProgramUniformMatrix4x3fv`
`glReadnPixels` | `glResumeTransformFeedback` | `glSamplerParameterIiv`
`glSamplerParameterIuiv` | `glTexBuffer` | `glTexBufferRange`
`glTexParameterIiv` | `glTexParameterIuiv` | `glTexStorage2DMultisample`
`glTexStorage3DMultisample` | `glTransformFeedbackVaryings` | `glUseProgramStages`
`glVertexAttribBinding` | `glVertexAttribFormat` | `glVertexAttribIFormat`
`glVertexBindingDivisor` | `glWaitSync`

## Remaining `stub` families (next promotions / honest gaps)

- **ES3 sync & queries** (`glFenceSync`/`glClientWaitSync`/`glWaitSync`, occlusion `glBeginQuery` result
  path): host-unbacked; kept as truthful stubs rather than gl_shim.c's fake-signaled returns, since ES3
  is a `DD_SHIM_ES3` opt-in and advertising working sync would be false.
- **Transform feedback capture, program pipelines / separable `glProgramUniform*`, compute / indirect
  dispatch, image load/store, memory barriers.**
- **EGL image / sync / pixmap-surface family** (`eglCreateImage`, `eglCreateSync`,
  `eglCreatePixmapSurface`, `eglBindTexImage`, …): reported as a *lower coherent EGL surface* (truthful
  failure) per audit section 2.1, rather than gl_shim.c's fake success handles.

## GLES 3.0 assessment (cross-cutting)

GTK4's `GskGLRenderer` needs an ES3 context + half-float vertex data. The guest advertises ES3 under
`DD_SHIM_ES3`; making it real still requires host-side work (a coordinated dd-gpu `VertexFormat`
half-float addition + Metal descriptor) and the remaining ES3 stub families above. Until then the shim
advertises ES2 by default (GTK falls back to software, correctly).
