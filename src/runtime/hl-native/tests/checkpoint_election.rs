#![cfg(feature = "native-test-hooks")]

//! WHO coordinates, and what everyone else commits.
//!
//! One container's process domain now shares ONE trigger word and ONE broker, so an exec session joins
//! the container's freeze instead of arming a capture of its own. There is one broker and one manifest,
//! so exactly one process may run the coordinator path -- and a coordinator does not commit a `proc.N`
//! group of its own.
//!
//! The election used to read `container_pid() == 1`, which is not that property: every ENGINE LAUNCH's
//! top process sets `g_init_hostpid` to its own pid, so a container exec session's top process reports
//! guest pid 1 exactly as the container init does. Measured on a live PostgreSQL cluster with three exec
//! sessions: four processes each printed `coordinator pid=... found 11 peer(s)`, none of the four
//! committed its own group, and the manifest was refused -- correctly -- as incomplete.
//!
//! The authority is the embedder's request, established at the launch boundary: only the machine holding
//! a `CheckpointControl` can be sent `REQUEST_CHECKPOINT`, and only it projects `HL_CHECKPOINT_COORDINATOR`.

/// The launch the embedder will ask for a capture coordinates.
#[test]
fn the_launch_the_embedder_asks_coordinates() {
    for isa in [1, 2] {
        hl_native::checkpoint_election_test(isa, 0)
            .unwrap_or_else(|status| panic!("ISA {isa} refused to coordinate the requested launch at {status}"));
    }
}

/// An exec session's top process is guest pid 1 by the only rule the engine has, and must NOT coordinate.
/// It must also name its group by its own identity: committing `proc.1` would collide with the
/// coordinator's own group and leave the group the coordinator waits for permanently absent, which is a
/// silently short manifest rather than a refused one.
#[test]
fn an_exec_session_top_is_a_member_not_a_second_coordinator() {
    for isa in [1, 2] {
        hl_native::checkpoint_election_test(isa, 1)
            .unwrap_or_else(|status| panic!("ISA {isa} let an exec session elect itself at {status}"));
    }
}

/// An ordinary guest process of the coordinating launch inherits the coordinator's option across fork and
/// must still take the member path.
#[test]
fn a_guest_process_of_the_coordinating_launch_commits_its_own_group() {
    for isa in [1, 2] {
        hl_native::checkpoint_election_test(isa, 2)
            .unwrap_or_else(|status| panic!("ISA {isa} elected a forked guest process at {status}"));
    }
}

#[test]
fn the_election_hook_rejects_unknown_scenarios() {
    for isa in [1, 2] {
        assert_eq!(hl_native::checkpoint_election_test(isa, 3), Err(-22));
    }
}
