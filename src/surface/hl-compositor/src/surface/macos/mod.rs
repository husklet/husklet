//! The macOS surface presenter: a concrete [`crate::scene::port::Presenter`] that draws a composed frame
//! to a REAL macOS window — Cocoa `NSWindow` + `CAMetalLayer` + Metal — or, headless, into an offscreen
//! `MTLTexture` it can read back.
//!
//! Ported (platform-neutral seam preserved) from the old `hl-display` present path: `metal.rs` (the
//! shared `MTLDevice`/queue + BGRA upload/blit/readback), `present_cocoa.rs` (the `NSWindow` +
//! `CAMetalLayer` window and the composite render pass sampling the surface texture over a white
//! background). IOSurface allocation and lifetime ownership live in `hl-iosurface`.
//!
//! Compiled only on macOS behind the `macos-surface` feature — the Linux/pure-std build never sees it.
//!
//! ## Where the pixels come from
//! The neutral [`crate::scene::model::PresentableImage`] carries GEOMETRY only (size/format/gpu flag) —
//! the neutral policy composes and paces on geometry alone. A real adapter attaches the actual bytes
//! out-of-band: call [`MacPresenter::attach_bgra`] (a `wl_shm` BGRA buffer) or
//! [`MacPresenter::attach_iosurface`] (an owned IOSurface lease) for a surface, THEN drive the neutral
//! `present()`; the presenter composites the currently-attached content for that surface id. This is the
//! same split the Wayland compositor uses (commit stashes the buffer; present composes it).

mod capture;
mod layer;
mod metal;
mod present;
mod transform;
mod window;

pub use hl_iosurface::Surface as IOSurface;
pub use metal::{BgraFrame, MetalCtx};
pub use present::MacPresenter;
pub use window::DisplayConfig;
