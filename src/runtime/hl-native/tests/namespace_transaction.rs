#![cfg(feature = "native-test-hooks")]

#[test]
fn namespace_publication_is_atomic_and_fork_safe_on_both_guest_isas() {
    for isa in [1, 2] {
        for scenario in 0..=3 {
            hl_native::namespace_transaction_test(isa, scenario).unwrap_or_else(|status| {
                panic!("namespace transaction ISA {isa} scenario {scenario} failed with status {status}")
            });
        }
    }
}

#[test]
fn namespace_test_hook_rejects_unknown_scenarios() {
    for isa in [1, 2] {
        assert_eq!(hl_native::namespace_transaction_test(isa, 4), Err(99));
    }
}
