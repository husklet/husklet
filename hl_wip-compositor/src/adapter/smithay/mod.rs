//! `adapter/smithay` — the real Wayland protocol server around the neutral [`crate::scene`] policy.
//!
//! This is the "compositor creating Wayland" seam (OVERVIEW-v2 §7): it stands up Smithay's
//! `wayland_frontend` state cores, translates `wl_compositor` / `wl_shm` / `xdg_shell` callbacks into
//! [`crate::scene::service`] calls through the neutral [`crate::Compositor`] engine, and runs the
//! `calloop` socket serve loop. The compositing/pacing policy stays entirely in `scene`; this layer only
//! decodes the wire and moves pixels across the [`crate::scene::port::Presenter`] boundary.
//!
//! - [`state::HlState`] — the Smithay dispatch aggregate holding the protocol cores + the neutral engine
//!   (scene + `PngPresenter` + clock). An adapter object; business rules live in `scene`.
//! - [`present::PngPresenter`] — the headless [`crate::scene::port::Presenter`]: captures composed frames
//!   (and optionally writes PNGs), the same seam a Cocoa/DRM presenter would occupy.
//! - [`serve::run`] — bind the Unix socket + dispatch the `calloop` loop until stopped.
//!
//! Proven end-to-end headless in `tests/wayland_e2e.rs`: a real `wayland-client` commits a colored
//! buffer and the test asserts the pixels arrive at the `PngPresenter`.

pub mod present;
pub mod serve;
pub mod state;

pub use present::{CapturedFrame, Observations, PngPresenter, StoredBuffer};
pub use serve::{input_channel, run, run_auto, run_auto_with_input, InputChannel, InputSender};
pub use state::{ClientState, HlState, InputCommand, MonotonicClock};
