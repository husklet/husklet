# Task ownership boundary

`hl-task` owns generation-qualified process and thread identities, process-tree
lifecycle, job control, signal queue attachment, namespace membership, and the
bounded snapshot of that state. It does not own guest-memory mutation, host
process creation, signal-frame encoding, descriptor cloning, or whole-engine
checkpoint coordination.

## Retained C oracle

The retained engine implements these capabilities through several independent
mechanisms rather than one registry:

- `src/linux_abi/syscall/proc.c` owns clone/fork/exec/wait dispatch, host PID and
  process-group translation, process attributes, clear-TID/robust-exit ordering,
  and child reaping. The inspected paths include `bound_fork_prepare`,
  `bound_fork_complete`, `fork_child_hooks`, `svc_proc`, the `setpgid`/`getpgid`/
  `getsid` cases, `PR_SET_PDEATHSIG`, `PR_SET_CHILD_SUBREAPER`, exec teardown,
  and `wait4` handling.
- `src/linux_abi/thread.c` owns the live guest-thread registry, per-thread CPU
  attachment, exec owner handoff, peer-thread termination, TID liveness, and
  after-fork lock repair. The inspected paths include
  `thread_process_owner_register`, `thread_exec_owner_handoff`,
  `thread_process_owner_wait`, `thread_tid_blocks_signal`, `thread_tid_alive`,
  `thread_tid_list`, and `thread_exit_others`.
- `src/linux_abi/signal.c` owns process-wide dispositions, per-thread masks,
  standard-signal coalescing, realtime FIFO queues, sender metadata, async host
  wakeups, default stop/continue/termination effects, and signal-frame delivery.
  The inspected paths include `sigq_push`, `sigq_pop`, `sigq_flush`,
  `host_sig_pend`, `host_sigh_si`, pending selection, and fatal group delivery.
- `src/linux_abi/linux_abi.c` owns the descriptor/OFD part of fork as a
  prepare/validate/arm/parent-or-child transaction. The inspected paths are
  `hl_linux_abi_fork_prepare`, `hl_linux_abi_fork_host_completed`,
  `hl_linux_abi_fork_parent`, and `hl_linux_abi_fork_child`.
- `src/linux_abi/checkpoint.c` owns whole-tree discovery, safepoint rendezvous,
  per-process commit, manifest-last publication, process-tree re-fork, PID map
  reconstruction, group/session reconstruction, and thread-group restore. The
  inspected paths include `ckpt_dump_self`, `ckpt_coordinate_and_exit`,
  `ckpt_restore_pgrp`, `ckpt_fork_children`, `ckpt_restore_proc_run`, and
  `ckpt_restore_tree`.

The C engine works because real host processes and sessions supply much of the
process model. Its process attributes are mostly process globals, its thread
registry has a separate mutex, and signal state mixes atomics, TLS, a queue lock,
and host handlers. The Rust engine instead makes the guest-visible model explicit
and per-runtime. That difference is intentional; behavior and ordering remain
the migration contract.

For child waits, retained `proc.c::svc_proc` case 260 probes rusage first, calls
host `wait4`, then performs rusage/status copyout and reap bookkeeping; a mapping
race that faults during copyout cannot undo that host reap. Retained
`rare.c::svc_rare` case 95 similarly probes first and preserves `WNOWAIT`, while
the host wait helpers make the host reap the irreversible ownership transition.
Rust maps that ordering to `PreparedWaitSelection`: selection reserves the event
against concurrent consumers, `commit` atomically consumes an ordinary exit and
folds the child's CPU usage into its parent, and only then does `hl-runtime`
attempt guest copyout. A copyout `EFAULT` therefore cannot resurrect the child;
a `WNOWAIT` commit only releases the reservation and leaves the event observable.

## Rust ownership

`registry.rs` owns the single lock that makes process, thread, session, group,
wait-event, and namespace transitions atomic. Its children are capabilities of
that owner, not peer registries:

| Module | Capability and invariant |
|---|---|
| `registry/state.rs` | generation-checked slot access, allocation, release, and lock admission |
| `registry/job/` | sessions, process groups, child transitions, wait selection, and reap |
| `registry/exec.rs` | reversible single-threading and process-image state transition |
| `registry/signal/` | lifecycle coordination around signal queues, delivery reservations, and plans |
| `registry/namespace.rs` | namespace membership and user-namespace map mutation |
| `registry/cancellation.rs` | runnable/blocked-thread cancellation and signal-pending flags |
| `registry/robust.rs` | robust-list registration transfer at thread exit |
| `registry/tid.rs` | clear-TID staging and final ownership transfer |
| `registry/mutation.rs` | bounded process/thread attribute mutations that do not yet form a separate entity |
| `registry/checkpoint/` | pointer-free task image validation and reconstruction under registry ownership |

Signal coordination outside the main lock is now one `signal::Coordination`
owner. It contains activity notification, dequeue reservations, and forced
deliveries. This makes its independent locking and lifetime explicit instead of
presenting three unrelated fields on `TaskRegistry`.

## Required later splits

- `registry/mutation.rs` still combines thread lifecycle, credentials, limits,
  prctl-like process attributes, and thread names. Split it only when those
  values have invariant-bearing owners; creating one module per setter would
  recreate the removed pseudo-registry layer.
- Signal values and queue invariants belong to `signal.rs`, while delivery changes
  task lifecycle. A future split should expose a narrow signal-state transaction
  to the registry; it must not make registry internals `pub(crate)`.
- Task checkpoint code is only the task-domain image. Whole-engine freeze,
  manifest publication, external resource binding, process-tree recreation, and
  resume ordering belong to `hl-checkpoint`/`hl-runtime`, matching the retained C
  coordinator.
- The Rust registry currently models process groups and sessions in memory. Host
  adapters must still reproduce the C engine's real host process-group, terminal,
  parent-death, subreaper, and wait behavior; an in-memory transition alone is
  not parity evidence.
