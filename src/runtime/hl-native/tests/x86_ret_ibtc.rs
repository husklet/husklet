#![cfg(all(feature = "native-test-hooks", target_os = "linux", target_arch = "x86_64"))]

#[test]
fn return_global_pair_preserves_architecture_and_cache_authority() {
    for scenario in 121..=135 {
        assert_eq!(
            hl_native::x86_64_translit_displaced_test(scenario),
            0,
            "RET global-pair scenario {scenario}"
        );
    }
}
