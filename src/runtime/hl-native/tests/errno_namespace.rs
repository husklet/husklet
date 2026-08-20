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

/// The `domain 0` arm selects THIS host's table, so a Linux errno number is not a valid input to it
/// on a host whose namespace diverges: Darwin 11 is `EDEADLK` (Linux 35), not the `EAGAIN` the
/// number looks like, and UCRT 40 is `ENOSYS` (Linux 38), not `ELOOP`. On Linux the same claim is
/// its mirror image -- the identity arm must leave both numbers untouched, which is what separates
/// "selected the identity" from "fell through to one of the other two tables".
///
/// The body was two `#[cfg]` arms, macOS and Windows, and nothing else. On Linux -- the host this
/// is developed on now -- it was an empty function that reported `ok`. `cfg!` keeps every arm
/// compiled and leaves something to assert on every host, and an unclassified host fails loudly
/// rather than passing: `errno.c`'s own `#else` sends it to the Darwin table by default, which is a
/// decision nobody has made deliberately.
#[test]
fn the_build_host_arm_selects_this_host_table_rather_than_the_linux_identity() {
    let darwin_probe = hl_native::linux_errno_from_host(0, 11);
    let ucrt_probe = hl_native::linux_errno_from_host(0, 40);
    assert!(
        if cfg!(target_os = "linux") {
            (darwin_probe, ucrt_probe) == (11, 40)
        } else if cfg!(target_os = "macos") {
            darwin_probe == 35
        } else if cfg!(target_os = "windows") {
            ucrt_probe == 38
        } else {
            false
        },
        "build-host arm on {} translated Linux 11 to {darwin_probe} and Linux 40 to {ucrt_probe}",
        std::env::consts::OS
    );
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
