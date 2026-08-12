//! Cargo-owned build and linkage boundary for Husklet's native C engine.
//!
//! The package exposes a deliberately small Rust facade. The C layout and its
//! host-service callback tables remain private implementation details so that
//! individual service groups can later move to Rust without changing callers.

mod bindings;
#[cfg(test)]
mod build_support;
mod engine;
mod provider;

#[cfg(test)]
mod artifact;

pub use engine::{Engine, EngineConfig, Exit};
pub use provider::leak_check_nonvacuity;

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
