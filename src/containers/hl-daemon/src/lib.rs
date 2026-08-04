//! Docker-compatible server and headless composition over [`hl_container`].
#![forbid(unsafe_code)]

pub mod api;
#[cfg(feature = "runtime")]
mod builder;
#[cfg(feature = "runtime")]
mod daemon;
#[cfg(feature = "runtime")]
mod error;
#[cfg(feature = "runtime")]
mod events;
#[cfg(feature = "runtime")]
mod process;
#[cfg(feature = "runtime")]
mod server;

#[cfg(feature = "runtime")]
pub use daemon::{Containers, Daemon, Release};
#[cfg(feature = "runtime")]
pub use error::{Error, Result};
#[cfg(feature = "runtime")]
pub use process::{ProcessSample, ProcessSampler};
#[cfg(feature = "runtime")]
pub use server::Server;
