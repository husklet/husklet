//! Platform surface presenters — concrete [`crate::scene::port::Presenter`] backends that take a
//! composed frame and put it on a real display.
//!
//! The neutral [`crate::scene`] policy decides *what* to present (the window tree, damage, pacing) and
//! hands each finished [`crate::scene::model::PresentableImage`] across the `Presenter` port. A backend
//! here owns the *how*: the GPU device, the native window, the pixel upload/blit. The neutral core holds
//! none of this — a presenter depends on the policy, never the reverse.
//!
//! - [`macos`] (feature `macos-surface`, macOS only) — a Cocoa `NSWindow` + `CAMetalLayer` + Metal
//!   presenter. It uploads a surface's `wl_shm` BGRA buffer to an `MTLTexture` (or zero-copy-wraps a host
//!   `IOSurface`), composites it over the window background, and blits into the layer's drawable. It also
//!   runs fully headless (offscreen `MTLTexture` it reads back) so the present path is provable on a
//!   real Metal GPU without a visible window / GUI login session.

#[cfg(all(feature = "macos-surface", target_os = "macos"))]
pub mod macos;
