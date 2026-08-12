//! Cargo-owned build and linkage boundary for Husklet's native C engine.
//!
//! The package exposes a deliberately small Rust facade. The C layout and its
//! host-service callback tables remain private implementation details so that
//! individual service groups can later move to Rust without changing callers.

mod bindings;
mod engine;
mod provider;

#[cfg(test)]
mod artifact;

pub use bindings::SyscallDispatch;
pub use engine::{Engine, EngineConfig, Exit, STATUS_OK};
pub use provider::{LIBRARY_NAME, Native};

pub use platform::{ExecutionMode, GuestIsa, HostArch, HostOs, HostTarget, PLANNED_HOSTS, SUPPORTED_HOSTS};

mod platform;

#[cfg(test)]
mod tests {
    use super::{LIBRARY_NAME, Native, provider::LIBRARY_PATH};

    #[test]
    fn shared_engine_exports_matching_abi() {
        assert_eq!(Native.abi(), 5);
        assert!(!Native.version().to_bytes().is_empty());
        assert!(LIBRARY_NAME.contains("hl_native_engine"));
        let library = std::path::Path::new(LIBRARY_PATH);
        assert!(
            library.is_file(),
            "Cargo-owned native library is missing: {}",
            library.display()
        );
        assert_eq!(library.file_name().and_then(|name| name.to_str()), Some(LIBRARY_NAME));
    }
}
