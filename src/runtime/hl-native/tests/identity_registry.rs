#![cfg(any(feature = "native-test-hooks", windows))]

#[cfg(not(windows))]
fn run(scenario: u32, iterations: u32) {
    hl_native::identity_registry_test(scenario, iterations)
        .unwrap_or_else(|status| panic!("identity registry scenario {scenario} failed with status {status}"));
}

#[cfg(not(windows))]
#[test]
fn writer_death_at_every_publication_phase_recovers() {
    for scenario in 1..=5 {
        run(scenario, 0);
    }
}

#[cfg(not(windows))]
#[test]
fn readers_never_accept_cross_map_aba_snapshots() {
    run(6, 20_000);
}

#[cfg(not(windows))]
#[test]
fn reaped_slots_are_reusable_beyond_twice_capacity() {
    run(7, 8_224);
}

#[cfg(not(windows))]
#[test]
fn host_identity_mutation_recovers_across_writer_death() {
    run(8, 0);
    run(9, 0);
}

#[cfg(not(windows))]
#[test]
fn one_transaction_preserves_multiple_new_entries() {
    run(10, 0);
}

#[cfg(not(windows))]
#[test]
fn concurrent_registration_assigns_one_guest_identity() {
    run(11, 32);
}

#[cfg(windows)]
#[test]
fn unsupported_host_reports_enotsup() {
    assert_eq!(hl_native::identity_registry_test(1, 0), Err(libc::ENOTSUP));
}
