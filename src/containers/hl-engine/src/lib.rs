//! Supported Rust API and engine composition root.

pub mod activation;
pub mod cli;
pub mod composition;
pub mod config;
pub mod domain;
pub mod engine;
pub mod environment;
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod ffi;
pub mod launcher;
/// Compatibility surface for callers using the former flat launch-plan module.
pub mod launch_plan {
    pub use crate::launcher::plan::*;
}
pub mod native;
/// Compatibility surface for callers using the former native-host module.
pub mod native_host {
    pub use crate::native::*;
    pub use crate::native::{
        Descriptor as NativeDescriptor, Signal as NativeSignal, SignalInfo as NativeSignalInfo,
        SignalMask as NativeSignalMask, SignalSource as NativeSignalSource, Socket as NativeSocket,
        Timer as NativeTimer,
    };
}
/// Compatibility surface for callers using the former native-launcher module.
#[cfg(target_os = "linux")]
pub mod native_launcher {
    pub use crate::native::launcher::*;
    pub use crate::native::launcher::{
        ProcessLauncher as NativeLauncher, ProcessWorkspace as NativeWorkspace, Selection as NativeSelection,
    };
}
pub mod options;
pub mod program;
#[cfg(target_os = "linux")]
#[path = "runtime/api.rs"]
pub mod runtime;
#[path = "runtime/machine.rs"]
pub mod runtime_machine;

pub use domain::Domain;
