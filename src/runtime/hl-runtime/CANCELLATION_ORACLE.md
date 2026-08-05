# Blocking cancellation oracle

This audit covers process-stop cancellation of guest threads blocked in time
and signal syscalls.

## Retained C implementation

The read-only retained implementation was inspected in:

- `../engine/src/linux_abi/thread.c`: `thread_target_signal` and
  `thread_exit_others` publish termination to every registered CPU, broadcast
  the condition variable of a futex-parked thread, and send `THREAD_INT_SIG` to
  interrupt any other blocking host syscall. Teardown repeats this over the
  complete thread registry until peers unregister. The registry mutex owns
  thread identity and lifetime; it is not held while waiting for peers.
- `../engine/src/linux_abi/syscall/time.c`: nanosleep interruption returns
  `EINTR` with the remaining interval and reaches the dispatcher signal
  boundary rather than retaining a private, unreachable host wait.
- `../engine/src/linux_abi/syscall/signal.c`: pause, `rt_sigsuspend`, and
  `rt_sigtimedwait` block only through the registered thread signal machinery;
  a directed termination reaches the same waiter and preserves mask restore
  and pending-signal ordering.

The load-bearing invariant is one interruption identity per executing guest
thread. Both ordinary guest signal activity and engine cancellation target that
identity. A syscall must not substitute a private token that the execution
owner cannot reach.

## Rust divergence and mapping

`ThreadRun` already owns a caller cancellation capability. `cancel_all` requests
it and also sets the native interrupt token. However, nanosleep and the signal
wait family created private `hl_sync::Interruption` values inside the syscall.
After the scheduler observed cancellation it retired the waiter-owned run and
joined the waiter pool, but the worker could remain asleep until the original
guest deadline (for example 3600 seconds). Exit publication therefore never
ran.

`RuntimeProcessSyscalls` now accepts the caller-owned `BlockingWait` capability.
The engine router binds the current `ThreadRun` cancellation to it. Nanosleep,
clock nanosleep, pause, `rt_sigsuspend`, and `rt_sigtimedwait` all use that same
interruption while retaining their task-signal subscriptions. Unit evidence
verifies the injected identity is passed to the sleep port and the complete
signal-wait cohort remains green.
