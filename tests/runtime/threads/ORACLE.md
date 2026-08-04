# Threads compatibility oracle audit

## Retained implementation studied

The read-only oracle was `../engine/src/linux_abi/thread.c`, especially
`thread_process_owner_register`, `thread_exec_owner_handoff`, the `fbk_*`
waiter bookkeeping, `futex_table_init`, `futex_private_table_after_fork`,
`futex_key`, `futex_op`, `futex_wake_addr`, `futex_robust_exit`, and the guest
thread trampoline. The syscall call sites studied were
`../engine/src/linux_abi/syscall/proc.c`: `set_tid_address`, `futex`,
`set_robust_list`, exit/exit_group, clone/clone3, fork repair, and exec owner
handoff. Architecture argument normalization was checked in
`../engine/src/translator/guest/x86_64/legacy.c` and
`../engine/src/linux_abi/syscall/nonpie_args.h`.

The C engine owns one CPU record per guest thread. Clone publishes sentry file
table inheritance before the host pthread can enter the dispatcher, installs
TLS/child-tid state, and rolls all reservations back on failure. Exit walks the
per-thread robust list, clears `clear_child_tid`, wakes joiners, releases sentry
bindings, and only then retires the CPU. Exec serializes a non-leader handoff to
the process owner. Fork rebuilds process-private futex locks and removes waiter
records belonging to vanished peer threads.

`thread_register`, `thread_unregister`, and `thread_target_signal` resolve only
the currently live integer TID and clear the registry entry before it can be
reused. Rust strengthens that ownership with a generation-qualified `ThreadId`:
a retained interruption token for an exited thread must never resolve for a new
thread that later receives the same guest-visible number. Tests for this rule
must exhaust unused task slots first because the Rust allocator intentionally
delays numeric TID reuse while a never-used slot remains available.

Futex identity is `(process,address)` for private operations and canonical
shared-object offset for shared mappings. A fixed bucket table owns locks,
condition variables, exact per-address waiter counts, bit masks, wake grants,
and a round-robin cursor. The value check and sleep transition occur under the
same bucket lock as wake selection, preventing lost wakeups. Wake returns the
number actually selected; bitset masks filter eligibility; requeue moves
identity while holding the relevant locks; waits preserve mismatch `EAGAIN`,
timeout `ETIMEDOUT`, interruption, alignment `EINVAL`, and guest-copy `EFAULT`.
PI owner words and robust owner death preserve Linux ownership and handoff
semantics. The host-specific distinction is process-shared pthread primitives
for cross-process futexes versus reinitialized private tables after fork;
x86-64 additionally normalizes clone argument order and fork/vfork registers.

## Rust ownership comparison

| Retained C capability | Rust owner | State |
|---|---|---|
| Thread/process identity, bounded slots, transactional clone and rollback | `hl-task::TaskRegistry` and `registry::{mutation,state}` | implemented |
| Thread exit, process exit, cancellation and waitable lifecycle | `hl-task::TaskRegistry` and `registry::{activity,cancellation}` | implemented |
| Exec preparation/publication and single surviving thread | `hl-task::registry::exec`, `hl-runtime::TaskExecParticipant` | implemented |
| Per-thread robust-list registration and snapshot | `hl-task::registry::robust`, `hl-task::RobustListRegistration` | implemented |
| Robust owner-death walk joined to memory, task, and futex | `hl-runtime::process` robust exit adapter | implemented |
| Futex parsing, guest-address validation, timeout and errno projection | `hl-linux` futex ABI and `hl-runtime::futex::{result,syscall}` | implemented |
| Bounded waiter queues, exact wake counts, bitsets, requeue and interruption | `hl-sync::futex::{model,lifecycle,multiple}` through `SafeRuntimeFutex` | implemented |
| PI ownership, handoff, timeout, deadlock and permission errors | `hl-sync::futex::pi` | implemented |
| Shared mapping canonical futex identity | memory-backed `SafeRuntimeFutex` key resolution | implemented |
| C sentry ring/table binding and exhaustion behavior | descriptor/process adapters in `hl-runtime` and container composition | divergent implementation, compatibility cases retained |
| C host-pthread and Windows clone repair mechanics | Rust task/execution platform adapters | deliberately replaced; target checks remain acceptance evidence |

The 67 cases in `test.yaml` are acceptance evidence for this whole capability
matrix, not case-specific engine policy. Expected files are copied byte-for-byte
from the retained registration. Each case keeps its original compiler/linker
flags, environment, target pair, exit status, and stable case identifier.
