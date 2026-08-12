#![allow(unsafe_code)]

/// The versioned native engine linked into this Cargo package.
///
/// This value is intentionally zero-sized. Cargo and `build.rs` own the shared
/// object's location and platform loader configuration; callers do not search
/// paths or open an arbitrary engine binary.
#[derive(Clone, Copy, Debug, Default)]
pub struct Native;

impl Native {
    /// Exercises one deliberately retained native allocation so leak tooling
    /// can prove that it observes the integrated C engine.
    #[must_use]
    pub fn leak_check_nonvacuity(self) -> i32 {
        // SAFETY: the symbol takes no arguments and owns its test allocation.
        unsafe { super::bindings::hl_c_backend_leak_check_nonvacuity() }
    }
}

/// Platform filename produced by this package's Cargo build.
#[cfg(test)]
pub(crate) const LIBRARY_NAME: &str = env!("HL_NATIVE_LIBRARY_NAME");

#[cfg(test)]
pub(crate) const LIBRARY_PATH: &str = env!("HL_NATIVE_LIBRARY_PATH");
