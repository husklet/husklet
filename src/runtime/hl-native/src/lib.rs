//! Cargo-owned build and linkage boundary for Husklet's native C engine.
//!
//! The package exposes a deliberately small Rust facade. The C layout and its
//! host-service callback tables remain private implementation details so that
//! individual service groups can later move to Rust without changing callers.

mod bindings;
mod engine;
mod provider;

pub use bindings::SyscallDispatch;
pub use engine::{Create, Engine, Exit, STATUS_OK};
pub use provider::{LIBRARY_NAME, Native};

/// Reports whether this package contains a production engine for the target.
#[must_use]
pub fn supported(target_os: &str, target_arch: &str) -> bool {
    platform::supported(target_os, target_arch)
}

mod platform;

#[cfg(test)]
mod tests {
    use super::{LIBRARY_NAME, Native};

    #[test]
    fn shared_engine_exports_matching_abi() {
        assert_eq!(Native.abi(), 5);
        assert!(!Native.version().to_bytes().is_empty());
        assert!(LIBRARY_NAME.contains("hl_native_engine"));
    }
}
