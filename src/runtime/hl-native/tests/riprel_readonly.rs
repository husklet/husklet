#![cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]

use std::sync::{Mutex, OnceLock};

fn native_globals() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(())).lock().unwrap_or_else(|poisoned| poisoned.into_inner())
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
        assert_eq!(hl_native::x86_64_translit_displaced_test(scenario), 0, "prefix scenario {scenario}");
    }
}

#[test]
fn readonly_riprel_option_off_emission_receipt() {
    let _guard = native_globals();
    // Re-derived from exact c8da42dfd with the same fixed guest/arena addresses and toolchain.  The C
    // hook also rebuilds once with the option absent and once explicitly set to zero, and returns zero
    // if their bytes, body length, or profile boundary differ.
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
        assert_eq!(hl_native::x86_64_translit_displaced_test(scenario), 0, "scenario {scenario}");
    }
}
