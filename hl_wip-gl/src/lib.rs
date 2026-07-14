//! hl-gl — the self-contained OpenGL-ES / EGL guest driver crate.
//!
//! It does exactly ONE thing (mirroring hl-cuda): **lower** an intercepted GLES2/3 + EGL operation into
//! the neutral hl-GPU IR and submit it through a [`hl_gpu::CommandSink`]. The host GPU renders; this
//! crate never touches Metal/OpenGL types. Unlike the CUDA driver — where every op submits
//! immediately — GL is **deferred-lowering**: a `gl*` call RECORDS into per-context state, and the whole
//! frame's IR is emitted at the `eglSwapBuffers` boundary.
//!
//! ## Layering (v2 doctrine — mirrored across the cuda/vulkan/gl drivers)
//! * [`model`] — the GL object model + its invariants: the per-context state ([`model::context::GlContext`]),
//!   the buffer/texture tables, the shader/program table, the bound state, and the recorded draw-list.
//!   Owned values; no `Cmd` construction, no transport.
//! * [`service`] — the operation seam. [`service::record`] holds the `gl*` recording ops (they mutate the
//!   model and submit NOTHING); [`service::frame`] builds the frame's `Cmd` stream from the recorded
//!   draw-list; [`service::swap`] is `eglSwapBuffers` — it submits the frame IR + a `Present` through the
//!   [`hl_gpu::CommandSink`]. This is the tested lowering surface.
//! * [`adapter`] — external, tech-named mechanisms: [`adapter::glsl`] (a GLSL-ES vertex+fragment pair →
//!   the shader-IR word payload a `CreateShader`/`CreateRenderPipeline` carries). Mirrors cuda's
//!   `adapter::ptx`.
//! * [`result`] — the EGL/GL error-code contract + the `GpuError` → `EGLint`/`GLenum` maps.
//!
//! ## Scope of this staging pass
//! The core render path is FULLY lowered: `glGenBuffers`/`glBufferData` → `CreateBuffer`/`WriteBuffer`;
//! `glGenTextures`/`glTexImage2D` → `CreateTexture`/`CreateSampler`/`WriteBuffer`(+`CopyBufferToTexture`);
//! `glCreateShader`/`glShaderSource`/`glCompileShader` + `glCreateProgram`/`glAttachShader`/`glLinkProgram`
//! (GLSL→shader-IR via [`adapter::glsl`]) → `CreateShader`/`CreateRenderPipeline`; the bound draw state +
//! `glDrawArrays`/`glDrawElements` → a recorded draw op; `eglSwapBuffers` → the frame's `CommandBuffer`
//! (`BeginRenderPass`/`SetPipeline`/…/`Draw`/`EndRenderPass`) + `Present`. Deferred to later passes: the
//! injectable shim cdylibs (`shim/`), the `build.rs` dual-arch cross-compile, and the `hl_jit::Driver`
//! plug — wiring, not lowering, kept out to keep this crate a light, standalone workspace.

pub mod adapter;
pub mod model;
pub mod result;
pub mod service;

// Ergonomic re-exports: downstream (and the shims, later) read `hl_gl::{GlContext, GlBuffer, …}`.
pub use model::buffer::GlBuffer;
pub use model::context::{GlContext, GlSurface};
pub use model::program::{DrawCall, Program, Shader};
pub use model::texture::GlTexture;
