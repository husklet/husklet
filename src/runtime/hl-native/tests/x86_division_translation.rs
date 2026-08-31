#![cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]

#[test]
fn same_isa_translator_admits_the_complete_integer_division_family() {
    assert_eq!(
        hl_native::x86_64_translit_displaced_test(190),
        0,
        "F6/F7 DIV and IDIV must admit byte, word, dword, and qword register/memory forms"
    );
}
