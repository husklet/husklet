//! The ergonomic runtime API: [`Runtime`] (host backend), [`Image`], [`Container`] + its builder, a
//! synchronous [`RunHandle`], and the async launch/supervision surface ([`RunningContainer`],
//! [`Launched`]). [`Runtime::run`] (sync) and [`Runtime::start`]/`start_into` (async) marshal a
//! [`Container`] into the backend's typed launch config and fork the engine directly through the linked
//! FFI entry (`dd_jit_darwin::spawn`/`spawn_io` → `ddjit_spawn`) — no subprocess, no shell, no env dialect.
//!
//! The types live split across cohesive submodules; everything the crate root re-exports is gathered
//! here so `lib.rs` has a single import site.

mod container;
mod device;
mod engine;
mod error;
mod handle;
mod image;
mod runtime;

pub use container::{Container, ContainerBuilder, DEFAULT_GUEST_PATH};
pub use device::{DeviceMount, DeviceProvider, DeviceRequest};
pub use engine::{Launched, LogChunk, RunningContainer, Stdio3};
pub use error::Error;
pub use handle::{ExitStatus, RunHandle};
pub use image::Image;
pub use runtime::Runtime;
