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

/// A member's exit report hangs off the process-scoped exit hook, not off the dispatcher's return.
///
/// A restored member is not a child of the host that holds it, so nothing can reap it: the only way its
/// status reaches the host is the report it sends over its own checkpoint channel on the way out. That
/// report used to be sent from exactly one place, the return from `run_guest` at the bottom of
/// `ckpt_restore_proc_run` -- and the ordinary way a Linux process ends does not go through it, because
/// `exit_group(2)` reaches the host `_exit` straight from the syscall handler. Measured on the product
/// Continue-later journey: a restored `/bin/sh -i` that exited 0 was recorded as
/// `Fault { status: -1, reason: Unknown }`, which is what the host correctly says about a member killed
/// outright, so a session that simply finished was indistinguishable from one that was destroyed.
///
/// This pins the PLACEMENT: the report is reachable from the exit path both `exit(2)` and `exit_group(2)`
/// share, and the send is still guarded to a process that announced itself. The behavior itself is asserted
/// against a live restored member by `hl-engine`'s
/// `a_restored_member_that_exits_cleanly_reports_its_code_on_both_isas`, alongside the signal half of the
/// same distinction -- so this file no longer stands in for an observation nobody was making.
#[test]
fn a_restored_member_reports_its_exit_from_the_shared_process_exit_hook() {
    let native = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/native");
    let identity = std::fs::read_to_string(native.join("linux_abi/syscall/process/identity.c"))
        .expect("read the process identity syscalls");
    let hook = identity
        .split_once("static void process_last_thread_exit(int status) {")
        .expect("process_last_thread_exit is the shared process-scoped exit hook")
        .1
        .split_once("\n}")
        .expect("process_last_thread_exit has a body")
        .0;
    assert!(
        hook.contains("ckpt_restored_member_exit_code(status)"),
        "the shared process exit hook no longer reports a restored member's status: {hook}"
    );
    assert!(
        identity.contains("case 94:"),
        "exit_group is no longer served from the file that carries the exit hook"
    );

    let stream = std::fs::read_to_string(native.join("linux_abi/sink_stream.h")).expect("read the checkpoint stream");
    assert!(
        stream.contains("if (g_ckpt_member_announced != self || reported == self) return;"),
        "the member exit report is no longer guarded to one report from the process that announced it"
    );
    assert!(
        stream.contains("g_ckpt_member_announced = getpid();"),
        "the announcement no longer names the process that made it, so a forked child inherits it"
    );
}
