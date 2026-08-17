#![cfg(feature = "native-test-hooks")]

fn run(scenario: u32, iterations: u32) {
    hl_native::identity_registry_test(scenario, iterations)
        .unwrap_or_else(|status| panic!("identity registry scenario {scenario} failed with status {status}"));
}

#[test]
fn writer_death_at_every_publication_phase_recovers() {
    for scenario in 1..=5 {
        run(scenario, 0);
    }
}

#[test]
fn readers_never_accept_cross_map_aba_snapshots() {
    run(6, 20_000);
}

#[test]
fn reaped_slots_are_reusable_beyond_twice_capacity() {
    run(7, 8_224);
}

#[test]
fn host_identity_mutation_recovers_across_writer_death() {
    run(8, 0);
    run(9, 0);
}

#[test]
fn one_transaction_preserves_multiple_new_entries() {
    run(10, 0);
}

#[test]
fn concurrent_registration_assigns_one_guest_identity() {
    run(11, 32);
}
