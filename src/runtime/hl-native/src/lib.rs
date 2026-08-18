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
#[cfg(unix)]
pub use provider::artifact_lifecycle_smoke;
pub use provider::leak_check_nonvacuity;

/// Verifies that the dynamically loaded private engine exposes the ABI this Rust wrapper expects.
///
/// This hidden packaging probe crosses the real C boundary after artifact relocation.
#[doc(hidden)]
#[must_use]
pub fn artifact_smoke() -> bool {
    bindings::engine_metadata_is_valid()
}

/// Returns the exact dynamic export contract for the Cargo-selected native library.
#[doc(hidden)]
#[must_use]
pub const fn artifact_export_manifest() -> &'static str {
    #[cfg(feature = "native-test-hooks")]
    {
        include_str!("native/bridge/test_exports.txt")
    }
    #[cfg(not(feature = "native-test-hooks"))]
    {
        include_str!("native/bridge/exports.txt")
    }
}

/// Resolves the shared objects that supplied the linked engine lifecycle symbols.
#[cfg(unix)]
#[doc(hidden)]
#[must_use]
pub fn artifact_paths() -> Option<Vec<std::path::PathBuf>> {
    bindings::engine_library_paths()
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

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn identity_registry_test(scenario: u32, iterations: u32) -> Result<(), i32> {
    // Each scenario owns process-global fault injection state in the C test boundary.
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::identity_registry_test(scenario, iterations)
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn namespace_transaction_test(isa: u32, scenario: u32) -> Result<(), i32> {
    static SERIAL: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    bindings::namespace_transaction_test(isa, scenario)
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
#[must_use]
pub fn x86_store_preflight_test() -> bool {
    bindings::x86_store_preflight_test()
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
#[must_use]
pub fn linux_errno_from_host(domain: u32, host_errno: i32) -> i32 {
    bindings::linux_errno_from_host(domain, host_errno)
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn signal_errno_frame_test(isa: u32, domain: u32, redirect: bool, nr: u64, raw: i64) -> Result<(i64, i64), i32> {
    bindings::signal_errno_frame_test(isa, domain, redirect, nr, raw)
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_continuation_contract_test(isa: u32) -> Result<(), i32> {
    bindings::checkpoint_continuation_contract_test(isa)
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_restore_claim_test(isa: u32, scenario: u32) -> Result<(), i32> {
    bindings::checkpoint_restore_claim_test(isa, scenario)
}

#[cfg(feature = "native-test-hooks")]
#[doc(hidden)]
pub fn checkpoint_restore_rollback_test(isa: u32) -> Result<(), i32> {
    bindings::checkpoint_restore_rollback_test(isa)
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
