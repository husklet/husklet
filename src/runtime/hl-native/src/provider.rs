#![allow(unsafe_code)]

use std::ffi::CStr;

/// The versioned native engine linked into this Cargo package.
///
/// This value is intentionally zero-sized. Cargo and `build.rs` own the shared
/// object's location and platform loader configuration; callers do not search
/// paths or open an arbitrary engine binary.
#[derive(Clone, Copy, Debug, Default)]
pub struct Native;

impl Native {
    /// Returns the C engine ABI generation.
    #[must_use]
    pub fn abi(self) -> u32 {
        // SAFETY: the symbol has no arguments and is linked from the shared
        // object built from the matching package sources.
        unsafe { super::bindings::hl_engine_abi() }
    }

    /// Returns the C engine's immutable build version.
    #[must_use]
    pub fn version(self) -> &'static CStr {
        // SAFETY: the engine ABI promises a process-lifetime NUL-terminated
        // string. A null result would violate that ABI and is diagnosed here.
        let value = unsafe { super::bindings::hl_engine_version() };
        assert!(!value.is_null(), "native engine returned a null version");
        // SAFETY: the null check above establishes pointer validity and the C
        // ABI guarantees the referenced NUL-terminated bytes live forever.
        unsafe { CStr::from_ptr(value) }
    }
}

/// Platform filename produced by this package's Cargo build.
pub const LIBRARY_NAME: &str = env!("HL_NATIVE_LIBRARY_NAME");

#[cfg(test)]
pub(crate) const LIBRARY_PATH: &str = env!("HL_NATIVE_LIBRARY_PATH");
