#![cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]

#[test]
fn completed_unsupported_instruction_returns_supported_successor_to_translation() {
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(222),
        0,
        "a progressing TL_NO instruction must redispatch exactly once, while consecutive TL_NO and translit-off remain bounded"
    );
}
