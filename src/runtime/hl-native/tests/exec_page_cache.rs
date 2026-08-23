#![cfg(feature = "native-test-hooks")]

use hl_native::exec_page_cache_test;

#[test]
fn stable_executable_pages_scan_the_nonexec_registry_once_per_thread() {
    for isa in [1, 2] {
        assert_eq!(exec_page_cache_test(isa, 0), Ok(1));
    }
}

#[test]
fn every_executable_mapping_transition_invalidates_a_warm_page_verdict() {
    for isa in [1, 2] {
        for scenario in 1..=3 {
            assert!(
                exec_page_cache_test(isa, scenario).is_ok(),
                "isa={isa} scenario={scenario}"
            );
        }
    }
}

#[test]
fn a_partially_nonexecutable_page_is_never_cached_as_wholly_valid() {
    for isa in [1, 2] {
        let scans = exec_page_cache_test(isa, 4).unwrap();
        assert!(scans >= 4, "isa={isa} scanned only {scans} time(s)");
    }
}
