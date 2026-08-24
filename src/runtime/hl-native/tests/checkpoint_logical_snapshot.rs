#![cfg(feature = "native-test-hooks")]

static FIXTURE: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[test]
fn logical_checkpoint_descriptor_lookup_stays_subquadratic_for_both_targets() {
    let _fixture = FIXTURE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
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

#[test]
fn logical_region_patch_failures_release_the_snapshot_without_counting_a_region() {
    let _fixture = FIXTURE.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
    for isa in [1, 2] {
        for scenario in [4, 5, 6] {
            hl_native::checkpoint_logical_snapshot_test(isa, scenario)
                .unwrap_or_else(|status| panic!("ISA {isa} logical patch scenario {scenario} failed at {status}"));
        }
    }
}
