//! Husklet's lightweight workspace configuration and headless application runtime.

pub mod config;
pub mod logging;
pub mod paths;

#[cfg(feature = "runtime")]
mod ffi;

#[cfg(feature = "runtime")]
pub mod runtime;
