# Task clone, exec, and teardown oracle audit

This audit covers the task-registry lifecycle assertions corrected alongside it.
The retained C engine at `../engine` was read only.

## Retained implementation studied

- `src/linux_abi/syscall/proc.c`: syscall cases 93 (`exit`), 94
  (`exit_group`), 96 (`set_tid_address`), 178 (`gettid`), 220 (`clone`), 221
  (`execve`), and 435 (`clone3`). The exec commit calls
  `thread_exec_owner_handoff` and `thread_exit_others` before destroying the old
  address space.
- `src/linux_abi/thread.c`: `spawn_thread`, `thread_trampoline`,
  `thread_register`, `thread_unregister`, `thread_after_fork`,
  `thread_exec_owner_handoff`, `thread_exec_owner_complete`,
  `thread_process_owner_wait`, `thread_exit_others`, `futex_robust_exit`, and
  `futex_wake_addr`.

## Ownership and lifecycle

The C process owns a main `cpu`; each `CLONE_THREAD` allocates another `cpu`,
starts one detached host pthread, and registers the guest TID and pthread in the
mutex-protected live-thread table. The trampoline owns and frees the child CPU.
It unregisters the thread, accounts its departure, processes its robust list,
clears and wakes `clear_child_tid`, and completes a pending non-leader exec owner
handoff. A fork retains only its calling thread and rebuilds process-private
locks and live-thread identity in the child.

Exec has a commit boundary after pathname/image validation. At that boundary a
non-leader caller assumes leader identity, all other threads are marked exited,
futex waiters are broadcast, blocking host syscalls are interrupted, and exec
waits for peers to unregister before old memory and close-on-exec descriptors
are destroyed. Failure before commitment leaves the old group intact. Successful
exec resets per-image state, caught signal handlers, TLS/register state, robust
list ownership, address space, and translation caches. `exit` terminates only the
caller; `exit_group` performs process-wide teardown and terminates the host
process. Partial blocking operations may return `EINTR`; clone resource or pids
limits return `EAGAIN`, allocation returns `ENOMEM`, and unsupported namespace
flags return `EINVAL`.

The architecture split is in clone resume/register setup and cache reset hooks;
the task identity and teardown ordering above is shared. Host-specific thread
interruption uses the engine-only host signal on macOS. The Rust task registry
owns the same logical identities with generation-qualified `ThreadId` values:
`begin_clone_thread`/`commit_clone_thread` own publication,
`PreparedTaskExec::publish` retires peers and transfers a non-leader caller into
the stable leader slot, and `finish` permanently releases retired slots.
Runtime execution remains responsible for host-thread interruption, robust-list
walking, clear-TID wakeup, and joining the registry transaction to memory,
descriptor, signal, and execution teardown.

## Finding

The failing tests contradicted the allocator contract. They required a retired
or rolled-back numeric slot to be reused immediately, but `allocate_thread`
deliberately selects the least-used free slot so unused task numbers precede
recycling, as Linux does. Generation invalidation and capacity release are the
real lifecycle invariants. Exec tests now constrain capacity so recycling is
required and therefore still prove release. Clone rollback now proves the stale
generation is absent and rejected while locating the newly committed thread by
its generation-qualified identity instead of a vector position.

## Initial-process publication audit

The retained initialization path was also inspected read-only in
`src/core/target/aarch64.c::container_init`,
`src/core/target/x86_64.c::container_init`, and
`src/linux_abi/container/state.c::{acct_container_reset,acct_claim_self}`. Both
architectures establish the container-init host identity before guest execution;
the shared state owner then publishes the init's live-task accounting slot.
Failures during root, namespace, credential, or volume preparation return before
`run_loaded`, so no guest can observe a partially prepared init. The state is
process-owned, inherited only after fork, and the accounting slot is released by
the registered exit path. Initialization does not block, cancel, or produce a
partial syscall result; architecture differences are limited to surrounding
option and root preparation, while macOS selects host-service mechanisms for
shared accounting.

In Rust, `TaskRegistry` owns process, thread, session, process-group, user/UTS
namespace, wait, signal, and generation state under one mutex. `InitReservation`
owns the four reserved identities and launch inputs outside visible slots.
Commit publishes the complete graph and initial-user-namespace owner while
holding that mutex; drop clears only the private reservation. Generations remain
consumed, matching the registry's stale-identity contract. Checkpoint rejects the
private transaction, and restore always starts without one. Rootfs, host process,
descriptor, memory, and execution preparation remain with their respective
runtime owners and are deliberately absent from `hl-task`.
