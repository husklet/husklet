#![allow(unsafe_code)]

/// Exercises a deliberately retained native allocation so leak tooling can
/// prove that it observes the integrated C engine.
#[must_use]
pub fn leak_check_nonvacuity() -> i32 {
    // SAFETY: the symbol takes no arguments and owns its test allocation.
    unsafe { super::bindings::hl_c_backend_leak_check_nonvacuity() }
}
