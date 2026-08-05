# Futex wait/wake performance audit

The retained C engine under `../engine` was inspected read-only.

## Retained implementation and call graph

- `src/linux_abi/thread.c`: `futex_table_alloc`, `futex_table_init`,
  `futex_private_table_after_fork`, `futex_key`, `futex_shared_register`,
  `futex_shared_unmap`, `fbk_of`, `fbk_wait_register`,
  `fbk_wait_unregister`, `fbk_wait_grant`, `fbk_park`, `fbk_unpark`,
  `fbk_match`, `fbk_parked`, `futex_wake_bucket`, `futex_wake_op_apply`,
  `futex_lock_pi`, `futex_unlock_pi`, `futex_op`, `futex_wake_addr`,
  `robust_handle_death`, and `futex_robust_exit`.
- `src/linux_abi/syscall/proc.c`: syscall 98 validates the Linux operation,
  pointer, timeout, private/shared identity, bitset, and wake counts before
  entering `futex_op`.
- `src/linux_abi/syscall/mem.c`: mapping publication supplies stable shared
  backing identity so aliases and fork peers address the same wait queue.

The C engine owns 256 process-shared buckets plus a private table. Each bucket
owns a process-shared mutex/condition variable, bounded per-address accounting,
and bounded FIFO grant slots. Wait compares and registers under the same bucket
lock used by wake, closing the lost-wakeup race. Wake grants no more than the
requested count, bitsets filter eligible waiters, shared mappings retain
cross-process identity, and private state is reset after fork. PI ownership,
robust-owner death, clear-TID, timeout clock choice, interruption, `EAGAIN`,
`EINTR`, `ETIMEDOUT`, and exact wake counts remain separate lifecycle paths.
The common logic is host-independent; pthread process-shared primitives are the
POSIX mechanism and architecture differences remain in guest word access.

Rust `hl-sync::FutexTable` owns the analogous hash buckets, keyed queues,
waiter election, wait queues, private/shared identities, requeue, waitv, PI,
snapshot, fork reset, and robust wake behavior. `hl-runtime::FutexPort` joins it
to memory atomicity, task interruption, and Linux errno conversion.

## Finding and change

The Rust keyed FIFO used `Vec`. Waking the ordinary FIFO prefix repeatedly
called `Vec::remove(0)`, shifting every remaining `Arc` on each selection and
making a bulk wake quadratic. The C grant scan is bounded and never shifts a
tail. The Rust queue is now `VecDeque`: FIFO prefix removal is constant time,
while arbitrary bitset selection preserves the prior order and behavior.
Registration, election, notification, locking, limits, errors, and teardown are
unchanged.

## Exact evidence

Base and candidate derive from `38417e968` in isolated worktrees. A release
diagnostic selects 4,096 FIFO waiters, 32 rounds. Seven alternating pairs:

```text
base ns/waiter:      255 255 255 256 255 256 256
candidate ns/waiter:  64  63  64  64  64  66  64
median:              255 -> 64 ns (-74.9%, 3.98x)
```

This diagnostic isolates the queue operation rather than host scheduling, so
native and retained C wall times are not directly comparable. Algorithmically,
native/C use bounded kernel/fixed-slot selection; the candidate removes Rust's
quadratic tail shifting and restores linear bulk selection.
