#![cfg(feature = "native-test-hooks")]

//! A container is a PID namespace, and every process in it needs a namespace-local identity.
//!
//! The engine virtualized only the init. `hl-aarch64 --rootfs $R bin/sh -c 'echo $$; sh -c "echo \$\$"'`
//! printed `1` and then `2171607`; `docker run ubuntu sh -c '...'` prints `1` and `7`. `getpid()` itself
//! returned the host pid, `/proc/self/status` agreed, its `PPid` named a process with no guest existence,
//! and `ls /proc` listed `1` beside `1867410`. That is host placement leaking into guest-visible runtime
//! metadata on the most ordinary path in the system.
//!
//! The mapping machinery was already here for checkpoint restore, which keeps guest pids stable across a
//! re-fork. The only restore-specific thing about it was when it got turned on.

/// A forked child's `getpid()` is the next namespace-local pid, its parent is the init rather than a host
/// pid, and the parent's view of it agrees with its own.
#[test]
fn a_forked_child_is_named_by_the_namespace_and_not_by_the_host() {
    for isa in [1, 2] {
        hl_native::pid_namespace_test(isa, 0)
            .unwrap_or_else(|status| panic!("ISA {isa} did not give a forked child a namespace pid: {status}"));
    }
}

/// The closed direction: a live host process outside the container has no guest rendering, so no host-shaped
/// pid can reach the guest through /proc, cgroup membership, or peer identity.
#[test]
fn a_host_process_outside_the_container_has_no_guest_pid() {
    for isa in [1, 2] {
        hl_native::pid_namespace_test(isa, 1)
            .unwrap_or_else(|status| panic!("ISA {isa} rendered a non-member host pid to the guest: {status}"));
    }
}

/// A container is one PID namespace served by several engine launches: the spec tree's top process, and one
/// more for every exec session. Each launch prepared a registry private to itself, so each seeded guest 1
/// for its own top and then allocated the same 2, 3, 4 for its forks -- two live processes in one container
/// answering one guest pid. That is also the name a capture files an image group under (`proc.<guest pid>`),
/// so two members claimed one group, the second `GROUP_BEGIN` collided, and the whole capture was refused:
/// `proc.3` duplicated in six of six recorded runs of the Continue-later journey.
#[test]
fn two_launches_of_one_container_never_issue_the_same_guest_pid() {
    for isa in [1, 2] {
        hl_native::pid_namespace_test(isa, 2)
            .unwrap_or_else(|status| panic!("ISA {isa} gave two live container processes one guest pid: {status}"));
    }
}
