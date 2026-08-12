//! Audited platform FFI implementations selected by the application.

#![allow(unsafe_code)]

#[cfg(target_os = "linux")]
pub(crate) mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(any(target_os = "macos", test))]
#[path = "ffi/macos/plan.rs"]
mod macos_plan;

#[cfg(target_os = "linux")]
pub use linux::LinuxHost;
#[cfg(target_os = "macos")]
pub use macos::DarwinHost;
