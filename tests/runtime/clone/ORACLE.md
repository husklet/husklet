# Thread clone and robust-exit oracle

> **Historical ownership:** `hl-execution` and `ffi/linux/execution` references
> preserve the deleted Rust-engine audit. Production is the selected C closure.

This workload was migrated from `tests/runtime/legacy/source/clone.c`. QEMU
user-mode returns `ENOSYS` for AArch64 `set_robust_list`; that oracle-only
capability refusal is accepted while the clone, futex, and clear-TID lifecycle
remain mandatory. When registration succeeds—as it must in Husklet—the exact
`WAITERS|OWNER_DIED` transition remains mandatory. The workload emits no
output; its exact golden is therefore the zero-byte `golden/empty.bin`. Distinct nonzero exit statuses
identify failed clone, TID publication, futex synchronization, clear-child-TID,
or robust-owner-death contracts.

The parent yields between unsuccessful gate wakes. Without that scheduling
point it can exhaust the legacy 100-attempt bound before the newly published
child is scheduled far enough to park, then exit only the calling thread and
leave the child blocked forever. The wake-count assertion itself is unchanged.

The x86-64 clone syscall is inlined at `_start`: unlike AArch64, x86-64 returns
from an out-of-line syscall wrapper by popping its return address from the new
child stack, where no call frame exists. Husklet traps before that host detail,
but the native oracle needs the ABI-correct inline form. The AMD64 oracle runs
directly on the Linux host because QEMU x86 user-mode does not reliably order
its emulated thread futex/clear-TID lifecycle; direct execution exercises the
authoritative Linux kernel behavior instead.

## Retained C engine audit

Read-only implementation files and entry points studied:

- `../engine/src/linux_abi/syscall/proc.c`: syscall case 220 (`clone`) thread
  routing to `spawn_thread`, namespace refusal, process-clone branch, parent and
  child TID publication, clear-child-TID registration, and fork/vfork lifecycle;
  syscall case 99 (`set_robust_list`) and the futex syscall marshalling path.
- `../engine/src/linux_abi/thread.c`: `spawn_thread`, `thread_main`, thread
  registry publication/removal, futex wait/wake bucket operations,
  `futex_wake_addr`, `robust_guest_to_host`, `robust_pin`,
  `robust_copy_from`, `robust_handle_death`, `futex_robust_exit`, and
  `thread_exit_cleanup`.
- `../engine/src/linux_abi/syscall/guest_copy.c`: guest-pointer folding used by
  robust-list registration and traversal for fixed-address/non-PIE guests.

The C thread clone owns a new `struct cpu`, guest TID, host pthread, inherited
signal mask, architectural register snapshot, optional TLS, guest stack, and
clear-child-TID address. The parent publishes the child TID only after thread
creation succeeds. The child begins with clone return value zero, its supplied
stack and TLS, and an empty robust-list registration: robust state is
per-thread and is never inherited. The live-thread registry maps guest TID to
host pthread and interruptible wait state under a mutex. Resource limits return
`EAGAIN`; invalid clone relationships return `EINVAL`; invalid TID addresses
return `EFAULT`, with staged state rolled back rather than publishing a partial
thread.

Futex wait first compares the aligned 32-bit word under the bucket lock, queues
only on equality, releases the lock while blocked, and returns `EAGAIN`,
`EINTR`, or `ETIMEDOUT` without losing wakeups. Wake holds the same bucket lock,
publishes grants, broadcasts, and returns the number actually released. The
workload uses these rules for the ready/gate handshakes and specifically
requires one real waiter before allowing child exit.

On thread exit, the C engine walks at most 2048 robust-list nodes. It masks the
PI bit from links, folds guest addresses before reads, tolerates inaccessible
or malformed nodes, avoids processing `list_op_pending` twice, atomically
replaces the exiting owner's TID with `FUTEX_OWNER_DIED` while preserving
`FUTEX_WAITERS`, and wakes the shared futex key. Robust cleanup precedes
clear-child-TID. Clear-child-TID silently tolerates an unmapped address, else
stores zero and wakes both private and shared key spaces so libc join variants
cannot remain parked. Final teardown unregisters the thread and frees its CPU
and host-thread resources only after these guest-visible obligations.

The retained ownership rules are guest-ISA neutral. The fixture supplies the
AArch64 versus x86-64 clone argument order and syscall numbers; child stack and
TLS register installation are architecture-specific in `spawn_thread`.
Threading uses pthreads on POSIX hosts. The C process-clone branch contains
Linux/macOS host-fork repair and readiness-adapter resets, but this workload's
`CLONE_THREAD` path never enters it. Windows process creation is a separate host
adapter and does not alter the Linux guest thread ABI asserted here.

## C-to-Rust capability matrix

| C capability | Rust owner | Status exercised here |
|---|---|---|
| Linux clone flag and argument decoding | `hl-linux` clone ABI and syscall table | both guest ISAs, thread clone flag family |
| transactional thread identity allocation and lifecycle | `hl-task::TaskRegistry` clone-thread staging/commit/rollback | new TID and final retirement |
| register, stack, TLS, return-value inheritance | `hl-runtime/src/thread/clone.rs` and `hl-execution` snapshots | supplied stack, child return zero, parent TID result |
| runnable host-thread construction and publication | `hl-engine/src/ffi/linux/execution/{clone.rs,threads.rs}` | parent/child concurrent execution |
| parent/child TID copyout and clear-TID registration | `hl-runtime` thread clone plus `hl-task` TID state | parent observes published TID; exit clears it |
| futex comparison, blocking, wake count and keying | `hl-sync` plus `hl-runtime` futex port | ready/gate wait-wake handshakes |
| per-thread robust-list registration | `hl-runtime/src/process/time.rs` and `hl-task` robust state | 24-byte head accepted and owned by child |
| robust-list traversal, OWNER_DIED update and wake ordering | `hl-runtime/src/robust/exit.rs`, lifecycle assembly, and engine memory adapter | `WAITERS|OWNER_DIED` after child exit |

This workload does not prove process clone/fork/vfork, clone3, pidfd, namespace
flags, resource-limit failures, signal interruption, timed futex waits, PI futex
handoff, malformed robust lists, checkpoint/restore, or exec-time sibling
retirement. Those remain separate compatibility cohorts.
