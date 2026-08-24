#![cfg(feature = "native-test-hooks")]

#[test]
fn descriptor_record_storage_tracks_the_visible_population() {
    for isa in [1, 2] {
        hl_native::checkpoint_fd_capacity_test(isa, 0).unwrap();
        hl_native::checkpoint_fd_capacity_test(isa, 1).unwrap();
        hl_native::checkpoint_fd_capacity_test(isa, 2).unwrap();
        assert_eq!(hl_native::checkpoint_fd_capacity_test(isa, 3), Err(-22));
    }
}
