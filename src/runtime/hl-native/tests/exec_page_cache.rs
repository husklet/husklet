#![cfg(feature = "native-test-hooks")]

use hl_native::exec_page_cache_test;

#[test]
fn stable_executable_pages_scan_the_nonexec_registry_once_per_thread() {
    for isa in [1, 2] {
        assert_eq!(exec_page_cache_test(isa, 0), Ok(1));
    }
}

#[test]
fn alternating_executable_pages_keep_independent_generation_bound_verdicts() {
    for isa in [1, 2] {
        assert_eq!(exec_page_cache_test(isa, 20), Ok(2));
    }
}

#[cfg(unix)]
#[test]
fn a_fork_child_revalidates_an_inherited_decoded_pc() {
    assert!(exec_page_cache_test(2, 11).is_ok());
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

#[test]
fn x86_decoded_pc_memo_revalidates_bytes_and_execute_permission() {
    assert_eq!(exec_page_cache_test(2, 5), Ok(1));
    for scenario in 6..=10 {
        assert!(exec_page_cache_test(2, scenario).is_ok(), "scenario={scenario}");
    }
}

#[test]
fn alternating_translation_targets_expose_every_authoritative_map_probe() {
    for isa in [1, 2] {
        assert_eq!(exec_page_cache_test(isa, 15), Ok(2));
        assert_eq!(exec_page_cache_test(isa, 16), Ok(3));
        assert_eq!(exec_page_cache_test(isa, 17), Ok(2));
    }
}

#[test]
fn jit_rollover_falls_back_to_an_executable_single_mapping() {
    for isa in [1, 2] {
        assert_eq!(exec_page_cache_test(isa, 18), Ok(42), "isa={isa}");
    }
}

#[test]
fn fetch_span_hits_reuse_the_authoritative_execute_page_verdict() {
    for isa in [1, 2] {
        let validations = exec_page_cache_test(isa, 19).unwrap();
        assert!(validations <= 1, "isa={isa} validated {validations} times");
    }
}
