#![cfg(feature = "native-test-hooks")]

#[test]
fn emitted_direct_store_guards_use_exact_atomic_preflight() {
    assert!(hl_native::x86_store_preflight_test());
}
