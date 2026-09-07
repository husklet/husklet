#[test]
fn direct_call_signal_windows_are_pre_or_post_commit_never_torn() {
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(146),
        0,
        "every direct-CALL byte must recover either original RSP/source or pushed RSP/target"
    );
}
