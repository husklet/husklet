#![cfg(all(target_os = "linux", target_arch = "x86_64", feature = "native-test-hooks"))]

#[test]
fn projection_boundary_refuses_hosts_token_injection_without_mutation() {
    for scenario in 0..5 {
        assert_eq!(hl_native::native_supervised_hostname_projection_test(scenario), 0);
    }
}
