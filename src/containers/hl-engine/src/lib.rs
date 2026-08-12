//! Supported Rust API and engine composition root.

// The FFI layer mirrors hl-runtime: handlers consume the request and plan structs they are
// handed, keep their receiver so a syscall family reads uniformly, and carry the plan and route
// tuples the ABI defines rather than types worth naming. Checkpoint wire Debug impls print the
// identifying fields only, matching the record they serialize.
#![allow(
    clippy::needless_pass_by_value,
    clippy::unused_self,
    clippy::type_complexity,
    clippy::missing_fields_in_debug,
    clippy::unnecessary_wraps,
    clippy::items_after_statements,
    clippy::field_reassign_with_default,
    clippy::assigning_clones,
    clippy::default_trait_access,
    clippy::cast_ptr_alignment,
    clippy::large_stack_arrays,
    clippy::vec_init_then_push,
    clippy::needless_continue,
    clippy::match_wildcard_for_single_variants,
    clippy::wrong_self_convention,
    clippy::large_types_passed_by_value,
    clippy::trivially_copy_pass_by_ref,
    clippy::manual_non_exhaustive,
    clippy::format_push_string,
    clippy::struct_field_names,
    clippy::len_without_is_empty,
    clippy::verbose_bit_mask,
    clippy::question_mark,
    clippy::assertions_on_constants,
    clippy::option_option
)]

pub mod activation;
pub mod cli;
pub mod composition;
pub mod config;
pub mod domain;
pub mod engine;
pub mod environment;
#[cfg(target_os = "linux")]
pub mod executable;
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
#[cfg(hl_retained_c)]
mod execution;
#[cfg(hl_retained_c)]
#[doc(hidden)]
#[must_use]
pub fn retained_c_link_anchor() -> usize {
    execution::retained_c_link_anchor()
}
pub mod options;
pub mod program;
pub mod retained_worker;
#[cfg(target_os = "linux")]
#[path = "runtime/api.rs"]
pub mod runtime;
mod session;

pub use domain::Domain;
