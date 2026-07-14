//! Platform adapters around the neutral [`crate::scene`] policy.
//!
//! An adapter translates a concrete outside world into `scene::service` calls and implements the
//! `scene::port` traits. The neutral policy holds NO adapter types; adapters depend on the policy.
//!
//! - [`smithay`] (feature `smithay-adapter`) — the real Wayland protocol server: it stands up Smithay's
//!   `wayland_frontend` state cores, translates `wl_*`/`xdg_*` callbacks into `scene::service` calls,
//!   and runs the calloop socket serve loop. Headless-provable via a `PngPresenter`.

#[cfg(feature = "smithay-adapter")]
pub mod smithay;
