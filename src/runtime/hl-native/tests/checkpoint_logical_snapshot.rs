#![cfg(feature = "native-test-hooks")]

#[test]
fn logical_checkpoint_descriptor_lookup_stays_subquadratic_for_both_targets() {
    for isa in [1, 2] {
        let one = hl_native::checkpoint_logical_snapshot_test(isa, 1).unwrap();
        let sixty_four = hl_native::checkpoint_logical_snapshot_test(isa, 2).unwrap();
        let two_fifty_six = hl_native::checkpoint_logical_snapshot_test(isa, 3).unwrap();
        assert!(one > 0);
        assert!(sixty_four < 64 * 20);
        assert!(two_fifty_six < 256 * 20);
        assert!(two_fifty_six < sixty_four * 6, "lookup visits grew quadratically");
    }
}
