#![cfg(feature = "native-test-hooks")]

//! Which launch a captured process belongs to, and therefore whose parent, group and session it records.
//!
//! A container `exec` session is forked by hl-container out of its own daemon, so its top process has a
//! host parent OUTSIDE the container and no guest parent at all. `ckpt_self_identity` has always had a
//! branch for exactly that -- no parent, its own process group, its own session -- and the branch was
//! selected by `container_pid() == 1`.
//!
//! That predicate was a true statement about launch tops only while every launch top folded its own
//! identity to guest 1. Once the identity registry gave each launch its real guest pid, an exec top
//! answered its own number, the branch went unselected, and the exec top fell through to the descendant
//! path -- where its host parent is the daemon, in no pidmap, so the lookup failed and the member refused
//! its own dump. That refusal took every other member's channel down with it and the whole close failed.
//!
//! The predicate now asks the question of the LAUNCH: `g_init_hostpid` is written once per launch by the
//! launch top itself and inherited unchanged across fork, so "it equals getpid()" is true of a launch top
//! and of nothing else, whatever guest pid that launch was handed.
//!
//! Both scenarios below use a guest pid other than 1 deliberately. A launch top holding guest pid 1 reads
//! identically under either predicate and would let a fixture pass for the wrong reason.

/// The exec session's top process: recognised as a domain root even though its guest pid is not 1.
#[test]
fn an_exec_session_launch_top_has_no_guest_parent_and_owns_its_group_and_session() {
    for isa in [1, 2] {
        hl_native::checkpoint_launch_identity_test(isa, 0).unwrap_or_else(|status| {
            panic!("ISA {isa} did not record the exec session's launch top as a domain root: {status}")
        });
    }
}

/// The direction the fix must not widen: an ordinary forked guest process keeps a real parent and takes
/// its group from the host. A predicate that called everything a domain root would pass the test above.
#[test]
fn a_forked_descendant_of_the_same_launch_is_not_a_domain_root() {
    for isa in [1, 2] {
        hl_native::checkpoint_launch_identity_test(isa, 1)
            .unwrap_or_else(|status| panic!("ISA {isa} mistook a forked descendant for a domain root: {status}"));
    }
}

/// An unknown scenario is rejected rather than silently answered.
#[test]
fn an_unknown_scenario_is_refused() {
    for isa in [1, 2] {
        assert_eq!(hl_native::checkpoint_launch_identity_test(isa, 2), Err(-22));
    }
    assert_eq!(hl_native::checkpoint_identity_test(9, 0), Err(-22));
}
