//! Boundary **ports**: runtime-neutral seams where an external backend plugs into a container launch.
//!
//! A port is a trait the runtime defines but never implements — the concrete meaning lives in the
//! backend's own crate. [`driver`] holds the driver-plugin seam ([`Driver`] + the [`Drivers`] registry):
//! how one or more GPU/accelerator/display backends inject the mounts, env, and render-node a launch
//! needs, without hl-jit ever learning what any of them *is*.

pub mod driver;

pub use driver::{Driver, Drivers, ProviderDriver};
