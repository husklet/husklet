#![cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]

#[test]
fn memory_movsd_translates_with_exact_load_store_and_rip_relative_semantics() {
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(39),
        0,
        "classifier must admit only the supported memory MOVSD forms"
    );
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(41),
        0,
        "the existing executable/shared-alias guard must still refuse translation"
    );
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(219),
        0,
        "MOVSD must zero the load destination upper qword, preload the store source, and fix RIP-relative memory"
    );
}
