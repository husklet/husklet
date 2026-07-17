//! hl-jit — a clean, platform-agnostic Rust API for configuring and running containers.
//!
//! `hl-jit` is the public runtime API. It selects a host backend at compile time (`hl-jit-darwin`
//! on macOS today; `hl-jit-linux` / `hl-jit-win` in the future) and exposes a uniform, ergonomic
//! interface for running containers directly from Rust — no shelling out, no Docker daemon needed.
//! `hl-daemon` is a thin Docker-Engine-API polyfill layered on top of this crate.
//!
//! ```no_run
//! use hl_jit::{Runtime, Container, Image};
//!
//! let rt = Runtime::new()?;
//! let c = Container::builder(Image::from_rootfs("/var/lib/hl/alpine"))
//!     .cmd(["/bin/sh", "-c", "echo hi"])
//!     .env("TERM", "xterm")
//!     .cpus(2)
//!     .memory_mb(512)
//!     .read_only(true)
//!     .publish(8080, 80)
//!     .bind("/host/data", "/data", false)
//!     .hostname("web")
//!     .build()?;
//!
//! let mut handle = rt.run(&c)?;
//! let status = handle.wait()?;
//! println!("exited {}", status.code());
//! # Ok::<(), hl_jit::Error>(())
//! ```

#![warn(missing_docs)]

// The backend surface (guest selector + the low-level launch contract) is re-exported so existing
// callers keep working unchanged while they migrate to the ergonomic API above.
pub use hl_jit_darwin::{available, Guest, PortMap, SpawnConfig, Volume};

mod runtime;
pub use runtime::{
    Container, ContainerBuilder, DeviceMount, DeviceProvider, DeviceRequest, Error, ExitStatus,
    Image, Launched, LogChunk, RunHandle, RunningContainer, Runtime, Stdio3, DEFAULT_GUEST_PATH,
};

/// Boundary ports — runtime-neutral seams where external backends plug in (see [`port::driver`] for the
/// GPU/accelerator driver-plugin registry).
pub mod port;
pub use port::{Driver, Drivers, ProviderDriver};
