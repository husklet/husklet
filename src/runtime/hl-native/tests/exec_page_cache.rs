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

#[cfg(unix)]
#[test]
fn x86_explicit_hot_contexts_isolate_and_revalidate_across_fork() {
    assert!(exec_page_cache_test(2, 21).is_ok());
}

#[cfg(unix)]
#[test]
fn x86_explicit_hot_contexts_are_isolated_between_pthreads() {
    assert!(exec_page_cache_test(2, 22).is_ok());
}

#[test]
fn x86_hot_context_allocation_failure_leaks_nothing_and_does_not_latch() {
    assert!(exec_page_cache_test(2, 23).is_ok());
}

#[test]
fn x86_stable_decode_authority_skips_fetch_but_every_invalidation_revalidates() {
    assert_eq!(exec_page_cache_test(2, 26), Ok(2));
    for scenario in 27..=33 {
        let result = exec_page_cache_test(2, scenario);
        assert!(result.is_ok(), "scenario={scenario}: {result:?}");
    }
    assert!(exec_page_cache_test(2, 35).is_ok());
}

#[test]
fn x86_decode_memo_key_miss_does_not_sample_authority() {
    assert_eq!(exec_page_cache_test(2, 36), Ok(1));
}

#[test]
fn x86_decode_memo_does_not_authorize_when_instability_latches_during_fetch() {
    assert_eq!(exec_page_cache_test(2, 37), Ok(2));
}

#[test]
fn x86_decode_authority_orders_reader_loads_against_writer_begin() {
    assert_eq!(exec_page_cache_test(2, 38), Ok(3));
}

#[test]
fn x86_decode_authority_waits_for_every_overlapping_writer() {
    assert_eq!(exec_page_cache_test(2, 39), Ok(5));
}

#[test]
fn x86_decode_authority_commit_lease_closes_the_publication_gap() {
    for scenario in 42..=43 {
        let result = exec_page_cache_test(2, scenario);
        assert!(result.is_ok(), "scenario={scenario}: {result:?}");
    }
}

#[test]
fn x86_decode_authority_tracks_each_production_publication_family() {
    assert!(exec_page_cache_test(2, 40).is_ok());
    assert!(exec_page_cache_test(2, 41).is_ok());
}

#[cfg(unix)]
#[test]
fn x86_decode_authority_revalidates_after_fork() {
    assert!(exec_page_cache_test(2, 34).is_ok());
    let inherited_lease = exec_page_cache_test(2, 44);
    assert!(inherited_lease.is_ok(), "scenario=44: {inherited_lease:?}");
}

#[test]
fn alternating_translation_targets_expose_every_authoritative_map_probe() {
    for isa in [1, 2] {
        assert_eq!(exec_page_cache_test(isa, 15), Ok(if isa == 1 { 3 } else { 2 }), "isa={isa}");
        assert_eq!(exec_page_cache_test(isa, 16), Ok(if isa == 1 { 4 } else { 3 }), "isa={isa}");
        assert_eq!(exec_page_cache_test(isa, 17), Ok(2), "isa={isa}");
    }
}

#[test]
fn translation_map_growth_preserves_every_index_and_lifecycle_boundary() {
    for isa in [1, 2] {
        for scenario in 55..=62 {
            assert!(
                exec_page_cache_test(isa, scenario).is_ok(),
                "isa={isa} scenario={scenario}"
            );
        }
    }
}

#[cfg(unix)]
#[test]
fn x86_hoisted_map_cache_pointer_survives_thread_generation_and_fork_boundaries() {
    assert!(exec_page_cache_test(2, 16).is_ok());
    assert!(exec_page_cache_test(2, 17).is_ok());
    assert!(exec_page_cache_test(2, 25).is_ok());
}

#[test]
fn jit_rollover_falls_back_to_an_executable_single_mapping() {
    for isa in [1, 2] {
        assert_eq!(exec_page_cache_test(isa, 18), Ok(42), "isa={isa}");
    }
}

#[cfg(target_os = "linux")]
#[test]
fn preferred_code_mapping_is_exact_and_never_replaces_an_occupied_alias() {
    assert_eq!(exec_page_cache_test(2, 53), Ok(5));
}

#[cfg(target_os = "linux")]
#[test]
fn fork_repair_replaces_only_the_owned_inherited_code_aliases() {
    assert_eq!(exec_page_cache_test(2, 54), Ok(2));
}

#[test]
fn fetch_span_hits_reuse_the_authoritative_execute_page_verdict() {
    for isa in [1, 2] {
        let validations = exec_page_cache_test(isa, 19).unwrap();
        assert!(validations <= 1, "isa={isa} validated {validations} times");
    }
}

#[test]
fn source_page_index_invalidates_one_and_multi_page_blocks_precisely() {
    for isa in [1, 2] {
        assert_eq!(exec_page_cache_test(isa, 45), Ok(1), "isa={isa}");
    }
}

#[test]
fn source_page_index_ignores_stale_nodes_after_tombstone_reuse() {
    for isa in [1, 2] {
        assert_eq!(exec_page_cache_test(isa, 46), Ok(1), "isa={isa}");
    }
}

#[test]
fn source_page_index_reset_changes_generation_without_clearing_tables() {
    for isa in [1, 2] {
        assert_eq!(exec_page_cache_test(isa, 47), Ok(1), "isa={isa}");
    }
}

#[test]
fn source_page_index_overflow_falls_back_to_the_live_map_scan() {
    for isa in [1, 2] {
        assert_eq!(exec_page_cache_test(isa, 48), Ok(1), "isa={isa}");
    }
}

#[cfg(unix)]
#[test]
fn source_page_index_is_inherited_coherently_across_fork() {
    for isa in [1, 2] {
        assert_eq!(exec_page_cache_test(isa, 49), Ok(1), "isa={isa}");
    }
}

#[test]
fn source_page_index_publication_precedes_concurrent_invalidation() {
    for isa in [1, 2] {
        assert_eq!(exec_page_cache_test(isa, 50), Ok(1), "isa={isa}");
    }
}

#[test]
fn source_page_index_grows_nodes_across_chunk_boundaries() {
    for isa in [1, 2] {
        assert_eq!(exec_page_cache_test(isa, 51), Ok(1), "isa={isa}");
    }
}
