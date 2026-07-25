//! The Wayland EGL platform adapter — the `wl_egl_window` ABI + the compositor present path.
//!
//! This is the "external, tech-named mechanism" (mirroring [`super::glsl`]) that teaches the GLES/EGL
//! front-end how to speak the **Wayland window system**: a real GUI app (`weston-simple-egl`, GTK) opens
//! its display with `eglGetPlatformDisplay(EGL_PLATFORM_WAYLAND_KHR, wl_display)`, wraps its `wl_surface`
//! in a `wl_egl_window` (via the `libwayland-egl` ABI [`WlEglWindow`]), then `eglCreateWindowSurface`s
//! against it and `eglSwapBuffers` to show a frame. The pieces here are split so the platform-recognition
//! Window ABI and protocol wire encoding are **pure, host-testable code** (no sockets), and only the
//! live [`Wayland`] session touches a fd.
//!
//! What lives here:
//!
//! - the EGL platform enums + `EGL_*_platform_wayland` extension strings the driver advertises,
//! - the `libwayland-egl` [`WlEglWindow`] handle (the app-visible `wl_egl_window*`) +
//!   [`parse_native_window`] that `eglCreateWindowSurface` reads to size the surface,
//! - [`rgba_to_xrgb8888`] — the readback→`wl_shm` pixel convert (GL bottom-left → top-left XRGB),
//! - [`Wayland`] — a dependency-free `wl_shm` present client (discover globals → bring up an
//!   xdg-toplevel → commit a shared-memory `wl_buffer` → pace on the frame callback). It is the
//!   SELF-CONTAINED present (the shim drives its own connection), ported from `hl-shim-gl/src/wayland.rs`
//!   with the dma-buf path swapped for core `wl_shm` so it needs no host buffer-return plumbing.
//!
//! HONEST SCOPE: presenting into an app's OWN `wl_surface` (the one it created on its own
//! `libwayland-client` connection) requires marshalling `wl_surface.attach`/`commit` onto THAT connection
//! (the Mesa `wl_proxy_marshal` path). This module instead drives its own compositor connection — correct
//! for the headless / shim-owned-surface case, and it always fails LOUDLY (never a fake present) when a
//! commit / handshake / pacing step does not complete.

use core::ffi::{c_int, c_void};
use std::os::fd::IntoRawFd;

mod platform;
mod protocol;
mod session;
mod shm;
mod window;

#[cfg(test)]
mod tests;

pub use platform::*;
pub use protocol::{Geometry, RegistryGlobal, WlError, WlResult};
pub use session::Wayland;
pub(crate) use shm::ShmBuffer;
pub use window::*;

use protocol::*;
