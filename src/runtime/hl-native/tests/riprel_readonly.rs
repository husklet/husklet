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
