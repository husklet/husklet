//! `hl_compositor` — the platform-neutral compositor policy for the hl host renderer (crate
//! `hl_wip_compositor`, lib `hl_compositor`).
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
//! ## What is DEFERRED (not built here)
//! The Smithay-required `HlState` aggregate and the three adapters are later tasks and deliberately
//! absent:
//! - `adapter/smithay` — Wayland protocol dispatch translating `wl_*`/`xdg_*` callbacks into the
//!   `scene::service` calls above.
//! - `adapter/cocoa` (macOS) and `adapter/drm` (Linux) — concrete [`scene::port::Presenter`]s.
//!
//! Because the whole policy is expressed against the two ports, it is fully unit-testable with a fake
//! clock and a fake presenter — no Smithay, no GPU, no Cocoa/DRM (see `tests/scene.rs`).

pub mod adapter;
pub mod scene;

pub use scene::{Compositor, CommitOutcome, FrameOutcome};
