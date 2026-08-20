#![cfg(feature = "native-test-hooks")]

//! Both hooks read emitted machine code, and the emitters exist only when the host is the one the
//! translator targets. On any other host each hook answers `4` for "not applicable" rather than a
//! clean `0`, and these cases assert that instead of vanishing -- the same shape as
//! `x86_store_preflight.rs`, and for the same reason: a fixture that quietly stops running is the
//! failure mode these hooks were written to catch.
//!
//! The file used to gate on `target_arch = "aarch64"` at FILE scope, so on an x86-64 host it
//! compiled to zero tests and reported `test result: ok`, saying nothing about which backend the
//! build had actually selected.

/// Darwin reserves host `x18` and clears it asynchronously between arbitrary
/// instructions, so emitted code may never keep a live value there. A `0` answer
/// also proves the fixture emitted the four soft-TLB entry loads it scans, so a
/// clean result cannot come from an empty buffer.
#[test]
fn emitted_code_never_holds_a_live_value_in_the_reserved_register() {
    let verdict = hl_native::aarch64_reserved_register_test();
    if cfg!(target_arch = "aarch64") {
        assert_eq!(
            verdict, 0,
            "emitted aarch64 code kept a live value in the reserved register"
        );
    } else {
        assert_eq!(
            verdict, 4,
            "expected the not-applicable verdict on a host without the emitters"
        );
    }
}

/// The x86-64 guest arm parks x86 condition codes in host scratch, so a zeroed
/// reserved register rewrites guest flags with no fault at all. A `0` answer
/// also proves every flag lowering in the fixture emitted code and that the
/// witness instructions the scan is built around are present.
#[test]
fn emitted_x86_flag_lowerings_never_name_the_reserved_register() {
    let verdict = hl_native::x86_reserved_register_test();
    if cfg!(target_arch = "aarch64") {
        assert_eq!(verdict, 0, "an emitted x86 flag lowering named the reserved register");
    } else {
        assert_eq!(
            verdict, 4,
            "expected the not-applicable verdict on a host without the emitters"
        );
    }
}
