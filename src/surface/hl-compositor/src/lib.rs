//! `hl_compositor` — the platform-neutral compositor policy for the hl host renderer (crate
//! `hl_compositor`, lib `hl_compositor`).
//!
//! This crate is the compositing "brain" (OVERVIEW-v2 §7): the window tree, damage, popup placement,
//! frame pacing/scheduling, focus, and surface-commit rules — everything a Wayland compositor decides
//! that does NOT depend on the wire protocol, the GPU, or the host windowing system. It is organized by
//! the four uniform roles (§2):
//!
//! - [`scene::model`] — the neutral values + invariants: the `Scene` graph of surfaces in a
//!   window/subsurface/popup tree over outputs, a seat, damage regions, and the `PresentableImage` /
//!   `Positioner` value types.
//! - [`scene::port`] — the two boundary traits the policy talks through: `Presenter` (where finished
//!   frames go) and `Clock` (the monotonic time source). Both are trivially faked in tests.
//! - [`scene::service`] — the use-cases: `commit`, `popup` placement, `compose`, `schedule` (pacing),
//!   and `focus`.
//! - [`scene::Compositor`] — the thin wiring object binding a scene + presenter + clock.
//!
//! Optional adapters keep those policies usable without platform dependencies: `adapter::smithay`
//! translates Wayland requests into scene operations, while `surface::macos` implements native
//! Cocoa/Metal presentation. The default build remains the neutral core and its deterministic tests.

pub mod adapter;
pub mod scene;
/// Concrete platform `scene::port::Presenter` backends. Behind the `macos-surface` feature so the
/// pure-std scene core (and the Linux build) is unaffected; only the macOS presenter lives here today.
#[cfg(feature = "macos-surface")]
pub mod surface;

pub use scene::{CommitOutcome, Compositor, FrameOutcome};
