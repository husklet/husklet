#![cfg(feature = "native-test-hooks")]

#[test]
fn current_host_errno_values_map_to_linux_guest_numbers() {
    for (host, linux) in [
        (libc::EAGAIN, 11),
        (libc::ENOTSUP, 95),
        (libc::ETIMEDOUT, 110),
        (libc::ECONNREFUSED, 111),
    ] {
        assert_eq!(hl_native::linux_errno_from_host(0, host), linux, "host errno {host}");
    }
}

#[test]
fn linux_namespace_values_are_not_a_valid_input_on_divergent_hosts() {
    #[cfg(target_os = "macos")]
    assert_ne!(hl_native::linux_errno_from_host(0, 11), 11);

    #[cfg(target_os = "windows")]
    assert_ne!(hl_native::linux_errno_from_host(0, 40), 40);
}

#[test]
fn divergent_darwin_and_ucrt_values_map_without_the_build_host() {
    assert_eq!(hl_native::linux_errno_from_host(1, 35), 11); // Darwin EAGAIN
    assert_eq!(hl_native::linux_errno_from_host(1, 45), 95); // Darwin ENOTSUP
    assert_eq!(hl_native::linux_errno_from_host(1, 61), 111); // Darwin ECONNREFUSED
    assert_eq!(hl_native::linux_errno_from_host(2, 40), 38); // UCRT ENOSYS
    assert_eq!(hl_native::linux_errno_from_host(2, 107), 111); // UCRT ECONNREFUSED
    assert_eq!(hl_native::linux_errno_from_host(2, 131), 22); // UCRT EOTHER
}

#[test]
fn signal_frame_observes_translated_errno_before_delivery_on_both_isas() {
    for isa in [1, 2] {
        assert_eq!(
            hl_native::signal_errno_frame_test(isa, 1, false, 130, -35).unwrap(),
            (-11, -11),
            "Darwin EAGAIN on ISA {isa}"
        );
        assert_eq!(
            hl_native::signal_errno_frame_test(isa, 2, false, 130, -40).unwrap(),
            (-38, -38),
            "UCRT ENOSYS on ISA {isa}"
        );
    }
}

#[test]
fn sigreturn_redirect_preserves_restored_linux_value_without_delivery() {
    for isa in [1, 2] {
        assert_eq!(
            hl_native::signal_errno_frame_test(isa, 1, true, 139, -35).unwrap(),
            (i64::MIN, -35),
            "sigreturn redirect on ISA {isa}"
        );
    }
}

#[test]
fn checkpoint_signal_precedence_and_restart_registers_hold_on_both_isas() {
    for isa in [1, 2] {
        hl_native::checkpoint_continuation_contract_test(isa)
            .unwrap_or_else(|status| panic!("ISA {isa} checkpoint continuation contract failed at {status}"));
    }
}
