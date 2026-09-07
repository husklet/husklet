#![cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]

#[test]
fn pre_spill_scratch_reconstructs_registers_and_flags_at_every_stage() {
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(238),
        0,
        "RAX/R11 and packed arithmetic flags must have exact signal provenance"
    );
}
