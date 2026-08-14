//! Cargo-owned build and linkage boundary for Husklet's native C engine.
//!
//! The package exposes a deliberately small Rust facade. The C layout and its
//! host-service callback tables remain private implementation details so that
//! individual service groups can later move to Rust without changing callers.

mod bindings;
#[cfg(test)]
mod build_support;
#[cfg(unix)]
mod checkpoint;
mod engine;
mod provider;

#[cfg(test)]
mod artifact;

#[cfg(unix)]
pub use checkpoint::{CheckpointBroker, CheckpointTransport};
pub use engine::{Engine, EngineConfig, Exit};
pub use provider::{artifact_lifecycle_smoke, leak_check_nonvacuity};

/// Verifies that the dynamically loaded private engine exposes the ABI this Rust wrapper expects.
///
/// This hidden packaging probe crosses the real C boundary after artifact relocation.
#[doc(hidden)]
#[must_use]
pub fn artifact_smoke() -> bool {
    bindings::engine_metadata_is_valid()
}

/// Resolves the shared object that supplied the linked engine ABI symbol.
#[cfg(unix)]
#[doc(hidden)]
#[must_use]
pub fn artifact_path() -> Option<std::path::PathBuf> {
    bindings::engine_library_path()
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn bound_vector_io_test(isa: u32, scenario: u32) -> Result<(i64, u32, u64), i32> {
    // The production target runs each guest in its own child process. This test hook runs both target TUs
    // in the test process, where they deliberately share the process-global logical-VMA ledger.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::bound_vector_io_test(isa, scenario)
}

#[cfg(test)]
mod platform;

#[cfg(test)]
mod tests {
    use super::bindings;

    const LIBRARY_NAME: &str = env!("HL_NATIVE_LIBRARY_NAME");
    const LIBRARY_PATH: &str = env!("HL_NATIVE_LIBRARY_PATH");

    #[test]
    #[allow(unsafe_code)]
    fn shared_engine_exports_matching_abi() {
        assert!(bindings::engine_metadata_is_valid());
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
