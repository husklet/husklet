#![cfg(all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"), feature = "native-test-hooks"))]

#[test]
fn completed_concurrent_projection_is_idempotent() {
    assert_eq!(hl_native::native_supervised_name_projection_test(0), 0);
}
