#![cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]

use std::sync::{Mutex, OnceLock};

fn native_globals() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[test]
fn readonly_riprel_expansions_are_exact_and_prefix_bounded() {
    let _guard = native_globals();
    assert_eq!(hl_native::x86_64_translit_displaced_test(158), 0, "TEST r/m8, imm8");
    assert_eq!(hl_native::x86_64_translit_displaced_test(159), 0, "CMP r/m8, imm8");
}

#[test]
fn readonly_riprel_execution_preserves_flags_scratch_and_split_provenance() {
    let _guard = native_globals();
    assert_eq!(hl_native::x86_64_translit_displaced_test(160), 0, "translated TEST");
    assert_eq!(hl_native::x86_64_translit_displaced_test(161), 0, "translated CMP");
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(162),
        0,
        "source straddles a page and the byte target is the mapping's last byte"
    );
}

#[test]
fn readonly_riprel_rejected_prefixes_fall_back_at_the_real_builder_boundary() {
    let _guard = native_globals();
    for scenario in 163..=169 {
        assert_eq!(
            hl_native::x86_64_translit_displaced_test(scenario),
            0,
            "prefix scenario {scenario}"
        );
    }
}

#[test]
fn readonly_riprel_option_off_emission_receipt() {
    let _guard = native_globals();
    // Re-derived from exact c8da42dfd with the same fixed guest/arena addresses and toolchain.  The C
    // hook builds twice with the option explicitly set to zero and returns zero if their bytes, body
    // length, or profile boundary differ.  These values are therefore the old f39/c8 OFF baseline,
    // independent of the new missing-value default.
    assert_eq!(hl_native::x86_64_translit_displaced_test(170), 1_094_434_755);
    assert_eq!(hl_native::x86_64_translit_displaced_test(171), 1_846_790_643);
    assert_eq!(hl_native::x86_64_translit_displaced_test(172), (223 << 16) | 1);
}

#[test]
fn readonly_riprel_admission_is_snapshotted_per_execution_context() {
    let _guard = native_globals();
    assert_eq!(hl_native::x86_64_translit_displaced_test(173), 0);
}

#[test]
fn standalone_fs_partial_spill_uses_native_scratch_state() {
    let _guard = native_globals();
    assert_eq!(hl_native::x86_64_translit_displaced_test(174), 0);
}

#[test]
fn fs_load_bridge_handles_destination_encoding_and_context_reuse() {
    let _guard = native_globals();
    for scenario in 175..=180 {
        assert_eq!(
            hl_native::x86_64_translit_displaced_test(scenario),
            0,
            "scenario {scenario}"
        );
    }
}

#[test]
fn fs_load_bridge_absent_matches_explicit_on_and_off_retains_the_old_boundary() {
    let _guard = native_globals();
    let absent = hl_native::x86_64_translit_displaced_test(181);
    let off = hl_native::x86_64_translit_displaced_test(182);
    let on = hl_native::x86_64_translit_displaced_test(184);
    assert_eq!(absent, on, "a missing option must emit the explicit-ON body");
    assert_ne!(off, on, "explicit OFF must retain the old one-instruction boundary");
    // The raw hash is compared only between adjacent builds because it contains emitted process
    // addresses and is not stable across ASLR placements.  The explicit-OFF path is independently
    // frozen by the fixed-address RIP-relative receipt above and by the exact-output GCC control.
}

#[test]
fn fs_load_bridge_range_invalidation_retranslates_changed_source() {
    let _guard = native_globals();
    assert_eq!(hl_native::x86_64_translit_displaced_test(183), 0);
}

#[test]
fn natural_riprel_load_bridge_is_load_only_and_covers_every_safe_destination() {
    let _guard = native_globals();
    assert_eq!(hl_native::x86_64_translit_displaced_test(187), 0);
}

#[test]
fn natural_riprel_load_bridge_is_default_off_and_snapshotted_per_context() {
    let _guard = native_globals();
    assert_eq!(hl_native::x86_64_translit_displaced_test(188), 0);
}

#[test]
fn natural_riprel_load_bridge_option_reaches_the_block_builder() {
    let _guard = native_globals();
    assert_eq!(hl_native::x86_64_translit_displaced_test(189), 0);
}
