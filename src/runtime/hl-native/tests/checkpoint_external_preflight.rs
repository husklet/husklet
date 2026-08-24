#![cfg(feature = "native-test-hooks")]

#[test]
fn restore_validation_memo_keys_preserve_every_availability_input() {
    for isa in [1, 2] {
        hl_native::checkpoint_external_preflight_key_test(isa, 0).unwrap();
        assert_eq!(hl_native::checkpoint_external_preflight_key_test(isa, 1), Err(-22));
    }
}
