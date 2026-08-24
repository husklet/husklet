#![cfg(feature = "native-test-hooks")]

//! The dispatcher diagnostics are observational only. These target-specific hooks prove that
//! sampling, reason accounting, monotonic clock deltas, and the disabled fast path share the exact
//! accumulator compiled into each engine library.

#[test]
fn x86_64_dispatch_profile_accumulates_exact_raw_values() {
    assert_eq!(hl_native::dispatch_profile_test(2), 0);
}

#[test]
fn aarch64_dispatch_profile_accumulates_exact_raw_values() {
    assert_eq!(hl_native::dispatch_profile_test(1), 0);
}
