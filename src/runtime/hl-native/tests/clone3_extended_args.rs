#![cfg(feature = "native-test-hooks")]

#[test]
fn clone3_versioned_arguments_are_validated_before_the_fork_boundary_on_both_guest_isas() {
    for isa in [1, 2] {
        for scenario in 1..=16 {
            hl_native::clone3_extended_args_test(isa, scenario).unwrap_or_else(|status| {
                panic!("clone3 argument ISA {isa} scenario {scenario} failed with status {status}")
            });
        }
    }
}

#[test]
fn clone3_argument_hook_rejects_unknown_guest_isas() {
    assert_eq!(hl_native::clone3_extended_args_test(0, 1), Err(-22));
    assert_eq!(hl_native::clone3_extended_args_test(3, 1), Err(-22));
}

#[test]
fn clone3_bad_pointer_fixture_owns_the_fault_handler_lifecycle_on_both_guest_isas() {
    for isa in [1, 2] {
        for _ in 0..2 {
            hl_native::clone3_extended_args_test(isa, 10)
                .unwrap_or_else(|status| panic!("clone3 bad-pointer ISA {isa} failed with status {status}"));
        }
    }
}
