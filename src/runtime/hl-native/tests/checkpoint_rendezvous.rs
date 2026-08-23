#![cfg(feature = "native-test-hooks")]

//! Who the coordinator is still WAITING for, and who it may stop waiting for.
//!
//! The whole-tree rendezvous waits for every enumerated peer to commit its own group, under one ~5s
//! deadline. An ordinary guest tree churns across the instant the peer set is enumerated -- a shell's
//! `sleep .05`, a `make` job, any fork that lives tens of milliseconds -- so a peer can be enumerated,
//! interrupted, and then simply exit before it ever reaches a checkpoint safepoint. The loop had no way to
//! stop waiting for it, so closing a perfectly healthy workspace burned the whole budget and then refused
//! with `participant <pid> never committed proc.<pid>`. Nothing had been lost; the capture failed anyway.
//!
//! The obvious fix -- drop a peer that is no longer alive -- is the dangerous one, and is why this was
//! deliberately left unfixed once. "Gone" is also true of a member that registered, published half its
//! objects, and was killed mid-dump. Dropping THAT peer publishes a manifest missing a member whose state
//! the user expects back: a silently incomplete checkpoint, which is worse than a failed close.
//!
//! The discriminator is "gone AND never `REGISTER_READY`'d for this generation", and it is exact rather
//! than heuristic because it is the complement of the broker's own publication gate: a connection that has
//! not registered is refused every byte-publishing operation, so "never registered" IS "published nothing".
//!
//! These four scenarios drive the real predicate, including the real `PARTICIPANT_REGISTERED` round trip
//! over a real checkpoint channel, against a real host process. The fake broker refuses any request that
//! is not that operation naming exactly the peer under test, so the exempting scenarios cannot pass by
//! never asking.

/// A peer that has not registered YET is a peer that still can. Liveness alone never grants the exemption.
#[test]
fn a_living_peer_that_has_not_registered_is_still_waited_for() {
    for isa in [1, 2] {
        hl_native::checkpoint_rendezvous_test(isa, 0)
            .unwrap_or_else(|status| panic!("ISA {isa} stopped waiting for a live peer at {status}"));
    }
}

/// The defect itself: a transient fork child that exited before joining the capture published nothing, so
/// the rendezvous drops it instead of burning the tree's deadline and refusing a healthy capture.
#[test]
fn a_peer_that_exited_before_registering_is_dropped_from_the_rendezvous() {
    for isa in [1, 2] {
        hl_native::checkpoint_rendezvous_test(isa, 1)
            .unwrap_or_else(|status| panic!("ISA {isa} kept waiting for a peer that never joined at {status}"));
    }
}

/// The half that stops the fix from being a loophole. A peer that registered and then died published
/// objects and never committed them; its state is genuinely lost, and the capture must still refuse.
#[test]
fn a_registered_peer_that_died_before_committing_still_refuses_the_capture() {
    for isa in [1, 2] {
        hl_native::checkpoint_rendezvous_test(isa, 2)
            .unwrap_or_else(|status| panic!("ISA {isa} dropped a member that had already published at {status}"));
    }
}

/// An unknown answer is not consent. A broker that cannot be reached grants no exemption to anyone.
#[test]
fn an_unanswerable_broker_grants_no_exemption() {
    for isa in [1, 2] {
        hl_native::checkpoint_rendezvous_test(isa, 3)
            .unwrap_or_else(|status| panic!("ISA {isa} exempted a peer on an unknown answer at {status}"));
    }
}

#[test]
fn the_rendezvous_hook_rejects_unknown_scenarios() {
    for isa in [1, 2] {
        assert_eq!(hl_native::checkpoint_rendezvous_test(isa, 8), Err(-22));
    }
}

/// The refusal snapshot is taken before teardown removes `/proc` state.  The two independent bits must
/// remain independent: collapsing registration into liveness recreates the old message that could not
/// distinguish a missed safepoint from a member whose dump had already begun.
#[test]
fn outstanding_participants_are_classified_by_liveness_and_registration() {
    for isa in [1, 2] {
        for scenario in 4..=7 {
            hl_native::checkpoint_rendezvous_test(isa, scenario).unwrap_or_else(|status| {
                panic!("ISA {isa} did not distinguish outstanding participant scenario {scenario}: {status}")
            });
        }
    }
}
