#![cfg(not(windows))]
//! The engine resolves a process's start-time identity token on every guest `open()`, `close()` and
//! descriptor inspection: it stamps fdvis ownership, classifies engine-private descriptors, and keys the
//! container registry's birth records. Reading it from `/proc/<pid>/stat` each time made that file open,
//! read and field parse the dominant cost of a guest `open()`, so the calling process's own token is
//! memoized.
//!
//! The memo is only sound because of three properties, and each is pinned below. A process's start time
//! is immutable for its lifetime, so a self answer never goes stale. The memo is retired by a fork epoch
//! that a `pthread_atfork` child handler bumps, so a forked child -- which inherits the parent's
//! memoized bytes through copy-on-write -- still re-reads and reports its own identity rather than the
//! parent's. And a peer pid is never served from the memo, because a remembered start time paired with a
//! recycled peer pid is exactly the stale-identity defect the token exists to detect, and every peer
//! caller uses it to decide membership, descriptor privacy, or teardown.
//!
//! The fork property has two reachable shapes and both are pinned, because they fail differently. When
//! the CALLER supplies the pid (scenario 2) a stale memo is caught by the pid comparison. When the
//! caller asks the memo *who it is* (scenario 5, `hl_host_process_self_identity`) there is no supplied
//! pid to compare against, and a child that inherits an unretired memo answers with its parent's pid
//! outright -- which would file the child's engine-private descriptors into the parent's registry row.

fn run(scenario: u32) {
    hl_native::process_identity_token_test(scenario)
        .unwrap_or_else(|status| panic!("process identity scenario {scenario} failed with status {status}"));
}

#[test]
fn a_process_reports_its_own_recorded_start_time_and_repeats_it() {
    run(1);
}

#[test]
fn a_forked_child_reports_its_own_token_not_the_inherited_one() {
    run(2);
}

#[test]
fn a_peer_pid_is_observed_fresh_and_never_answered_from_the_caller_memo() {
    run(3);
}

#[test]
fn an_absent_pid_and_an_absent_destination_both_fail() {
    run(4);
}

#[test]
fn a_forked_child_asked_for_its_own_identity_reports_the_childs_pid_and_start_time() {
    run(5);
}
