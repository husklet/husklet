//! External, tech-named mechanisms the GL driver drives (mirrors hl-cuda's `adapter/`).
//!
//! * [`glsl`] — the GLSL-ES front-end: a vertex+fragment GLSL-ES pair → the shader-IR word payload a
//!   `CreateShader`/`CreateRenderPipeline` carries (the host compiles the translated source, just as
//!   cuda's PTX descriptor is compiled host-side). Ported from `hl-shim-gl/src/translate.rs`. Per v2
//!   doctrine the GLSL front-end lives in the driver, not the neutral protocol — exactly as the PTX
//!   parser lives in the CUDA driver.
//! * [`wayland`] — the Wayland EGL platform: the `wl_egl_window` ABI the app links, the platform
//!   recognition + advertised `EGL_*_platform_wayland` extensions, and a dependency-free `wl_shm` present
//!   client. Ported from `hl-shim-gl/src/wayland.rs`; drives `eglCreateWindowSurface`/`eglSwapBuffers` for
//!   real Wayland GUI apps (`weston-simple-egl`, GTK).
//! * [`wayland_app`] — presents the read-back frame onto the app's OWN `wl_surface` (the real-window
//!   milestone): `dlopen(RTLD_NOLOAD)`s the app's already-loaded `libwayland-client`, marshals a `wl_shm`
//!   buffer + `attach`/`commit` onto the app's surface via `wl_proxy_marshal_flags` on a PRIVATE event
//!   queue (the Mesa EGL-Wayland pattern). The preferred `eglSwapBuffers` present; [`wayland`]'s
//!   self-owned toplevel is the fallback when this presenter is unavailable.

pub mod glsl;
pub mod wayland;
pub mod wayland_app;
