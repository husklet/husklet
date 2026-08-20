#![cfg(feature = "native-test-hooks")]

/// The guards this scans are emitted by the x86-64 guest's ARM64 lowerings, which are compiled only on
/// an AArch64 host. Rather than compile the fixture out off that host -- a test that silently never runs
/// hides more than one that fails -- assert the "not applicable" verdict the hook was given for exactly
/// this case. That still exercises the export and the loader's resolution of it, and it pins the one
/// answer that must never appear here: a clean `0`, which would claim a scan that never ran.
#[test]
fn emitted_direct_store_guards_use_exact_atomic_preflight() {
    let expected = if cfg!(target_arch = "aarch64") { 0 } else { 4 };
    assert_eq!(hl_native::x86_store_preflight_test(), expected);
}
