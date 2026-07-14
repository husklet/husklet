//! # hljit — the dd VM-less JIT container runtime + its bindings.
//!
//! The JIT runs a Linux container by translating its code and servicing its syscalls in userspace — no
//! VM. The C runtime (`src/runtime/`) is compiled by `build.rs` into one codesigned binary per guest
//! architecture (`aarch64`, `x86_64`); this crate exposes those binaries plus the typed
//! [`SpawnConfig`] launch contract and the [`spawn_io`]/[`LaunchConfig`] FFI entry the runtime forks
//! through. `hl-daemon` (and any other front-end) depends on this crate via the `hl-jit` API layer.

#![warn(missing_docs)]

mod launch;
mod guest;
mod types;
mod spawn_config;

pub use launch::{spawn, spawn_io, LaunchConfig, SpawnIo};
pub use guest::{available, Guest};
pub use types::{PortMap, Volume};
pub use spawn_config::SpawnConfig;
