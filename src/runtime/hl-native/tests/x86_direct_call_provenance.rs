#![cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]

#[test]
fn direct_call_signal_windows_are_pre_or_post_commit_never_torn() {
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(146),
        0,
        "every direct-CALL byte must recover either original RSP/source or pushed RSP/target"
    );
}

#[test]
fn direct_call_pre_spill_guard_hit_is_typed_and_relocation_safe() {
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(220),
        0,
        "hook-local guard must miss through the typed shared path, then hit with a cleared marker"
    );
}
