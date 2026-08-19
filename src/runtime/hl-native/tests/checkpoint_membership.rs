#![cfg(feature = "native-test-hooks")]

//! Who is IN the capture.
//!
//! The coordinator freezes a member set and then publishes an image whose socket topology asserts that
//! both ends of every connected pair were stopped. That assertion is only true if membership names every
//! process of the container. It did not: the filter was descendancy of guest pid 1, and hl-container forks
//! an `exec` session out of its own daemon, so an exec session is a SIBLING of pid 1. Measured on a live
//! `PostgreSQL` cluster, three of eleven engine processes -- every `psql` client -- reported `descendant=0`
//! while holding the far end of a socket owned by a process that WAS captured.
//!
//! The replacement must not be a widening. The filter before descendancy was "same session", and it found
//! ZERO peers on that same cluster because the engine emulates guest `setsid(2)` with the host's, so every
//! guest session leader has its own host session id. So these fixtures pin both directions against real
//! live processes: a sibling of this process is admitted when the control plane put it in this container's
//! process domain, and refused when it did not -- while running the very same executable.

/// A container `exec` session is a member. The fixture's orphan is a SIBLING, not a descendant, and the
/// hook asserts that before it decides, so a fixture that accidentally produced a descendant fails rather
/// than passing for the wrong reason.
#[test]
fn a_container_exec_session_is_enumerated_as_a_capture_member() {
    for isa in [1, 2] {
        hl_native::checkpoint_membership_test(isa, 0)
            .unwrap_or_else(|status| panic!("ISA {isa} refused a container exec session at {status}"));
    }
}

/// The net is not widened to "any live engine process". A second container's exec session is alive, is a
/// sibling, and runs this same executable; only its process domain differs, and that is enough to refuse.
#[test]
fn a_process_in_another_containers_domain_is_not_a_capture_member() {
    for isa in [1, 2] {
        hl_native::checkpoint_membership_test(isa, 1)
            .unwrap_or_else(|status| panic!("ISA {isa} admitted a foreign container's process at {status}"));
    }
}

/// Membership is a published record, not an inference. A live sibling that never joined any domain is
/// refused, so an unrelated host process can never be interrupted into someone else's freeze.
#[test]
fn a_process_that_published_no_membership_is_not_a_capture_member() {
    for isa in [1, 2] {
        hl_native::checkpoint_membership_test(isa, 2)
            .unwrap_or_else(|status| panic!("ISA {isa} admitted an unregistered process at {status}"));
    }
}

#[test]
fn the_membership_hook_rejects_unknown_scenarios() {
    for isa in [1, 2] {
        assert_eq!(hl_native::checkpoint_membership_test(isa, 3), Err(-22));
    }
}
