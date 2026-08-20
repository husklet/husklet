#![cfg(feature = "native-test-hooks")]

/// The guard this reads is emitted, and the emitters exist only when the host is
/// the one the translator targets. On any other host the hook answers `4` for
/// "not applicable" rather than a clean `0`, and the test asserts that instead of
/// skipping: a fixture that quietly stops running is the failure mode these hooks
/// were written to catch.
#[test]
fn emitted_direct_store_guards_use_exact_atomic_preflight() {
    let verdict = hl_native::x86_store_preflight_test();
    if cfg!(target_arch = "aarch64") {
        assert_eq!(verdict, 0, "emitted store guards did not use an exact atomic preflight");
    } else {
        assert_eq!(
            verdict, 4,
            "expected the not-applicable verdict on a host without the emitters"
        );
    }
}
