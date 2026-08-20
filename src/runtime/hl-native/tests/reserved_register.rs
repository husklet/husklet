#![cfg(all(feature = "native-test-hooks", target_arch = "aarch64"))]

/// Darwin reserves host `x18` and clears it asynchronously between arbitrary
/// instructions, so emitted code may never keep a live value there. A `0` answer
/// also proves the fixture emitted the four soft-TLB entry loads it scans, so a
/// clean result cannot come from an empty buffer.
#[test]
fn emitted_code_never_holds_a_live_value_in_the_reserved_register() {
    assert_eq!(hl_native::aarch64_reserved_register_test(), 0);
}
