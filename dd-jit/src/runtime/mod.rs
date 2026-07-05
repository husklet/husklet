//! The ergonomic runtime API: [`Runtime`] (host backend), [`Image`], [`Container`] + its builder, a
//! synchronous [`RunHandle`], and the async launch/supervision surface ([`RunningContainer`],
//! [`Launched`]). It is a typed layer over the backend's `SpawnConfig` launch contract; today it
//! launches the engine via the backend's command (subprocess), which Phase 3 replaces in-place with a
//! linked fork+FFI entry without changing this surface.
//!
//! The type lives split across cohesive submodules; everything the crate root re-exports is gathered
//! here so `lib.rs` has a single import site.

mod container;
mod engine;
mod error;
mod handle;
mod image;
mod runtime;

pub use container::{guest_env, resolve_user, Container, ContainerBuilder, DEFAULT_GUEST_PATH};
pub use engine::{Launched, LogChunk, RunningContainer, Stdio3};
pub use error::Error;
pub use handle::{ExitStatus, RunHandle};
pub use image::Image;
pub use runtime::Runtime;
