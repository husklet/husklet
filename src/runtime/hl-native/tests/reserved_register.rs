#![cfg(feature = "native-test-hooks")]

/// Both hooks emit real code and scan it, so both are answerable only where the emitters are compiled --
/// an `AArch64` host. Off it they report `4`, "not applicable", and these fixtures assert that rather than
/// compiling themselves out: the exports still have to resolve, and the verdict that must never appear on
/// a host without the emitters is a clean `0`.
const CLEAN: i32 = if cfg!(target_arch = "aarch64") { 0 } else { 4 };

/// Darwin reserves host `x18` and clears it asynchronously between arbitrary
/// instructions, so emitted code may never keep a live value there. A `0` answer
/// also proves the fixture emitted the four soft-TLB entry loads it scans, so a
/// clean result cannot come from an empty buffer.
#[test]
fn emitted_code_never_holds_a_live_value_in_the_reserved_register() {
    assert_eq!(hl_native::aarch64_reserved_register_test(), CLEAN);
}

/// The x86-64 guest arm parks x86 condition codes in host scratch, so a zeroed
/// reserved register rewrites guest flags with no fault at all. A `0` answer
/// also proves every flag lowering in the fixture emitted code and that the
/// witness instructions the scan is built around are present.
#[test]
fn emitted_x86_flag_lowerings_never_name_the_reserved_register() {
    assert_eq!(hl_native::x86_reserved_register_test(), CLEAN);
}
