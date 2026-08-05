# Signal poll performance audit

The retained C engine under `../engine` was inspected read-only at commit-time
alongside this change.

## Oracle and ownership

- `src/linux_abi/signal.c`: `sigq_push`, `sigq_pop`, `sigq_drop_tag`,
  `maybe_deliver_signal`, `raise_guest_signal_info`, and the host signal entry
  paths. `g_pending` is the process-wide one-bit-per-signal readiness index;
  each CPU owns its thread-directed `tpending`. The indexed FIFO rings retain
  siginfo and realtime ordering. Queue mutation is serialized by `g_sigq_lk`;
  host handlers only publish atomic pending bits and never take the queue lock.
- `src/linux_abi/syscall/signal.c`: `thread_kill`, `svc_signal`, restart tests,
  `rt_sigtimedwait`, mask changes, and pending-set reporting. Reads combine the
  process and thread masks before inspecting queue details. Interrupted calls
  preserve `EINTR`/`SA_RESTART`; selected realtime instances remain FIFO.
- `src/linux_abi/thread.c`: `thread_register`, `thread_unregister`,
  `thread_target_signal`, `thread_exit_others`, `thread_trampoline`, and
  `spawn_thread`. The live-thread table owns TID-to-CPU identity under
  `g_threg_m`; directed signal publication sets only the addressed CPU's
  pending state and interrupts a blocked host operation. Exec retires peers
  before old process state is destroyed.
- `src/linux_abi/syscall/proc.c`: scheduler policy validation and exec/exit
  callers. Architecture-specific state is confined to CPU/signal-frame setup;
  macOS uses the engine-only host interruption signal. Signal priority,
  coalescing, masks, and lifecycle ordering are shared Linux behavior.

Rust `TaskRegistry` owns the analogous process/thread queues under one registry
lock. `PendingSignals` retains one FIFO per signal, standard-signal coalescing,
realtime FIFO order, and source-tag cancellation. Registry signal delivery,
wait, checkpoint, exec, and exit paths are the consumers.

## Finding and change

The Rust readiness checks scanned 64 `VecDeque`s for each thread queue and again
for the process queue. Pending-mask reporting additionally allocated and copied
every queued `SignalInfo`. The C oracle separates the readiness mask from the
payload queues. Rust now maintains the same derived occupied/front-synchronous
bit indexes on enqueue, dequeue, source removal, flush, and restore. Selection
uses bit priority operations; snapshots remain the checkpoint representation.
No ownership, locking, delivery priority, standard coalescing, realtime FIFO,
mask, error, or teardown behavior changes.

## Exact A/B evidence

Base and candidate were built from `38417e968` with warnings denied in isolated
worktrees. The ignored release benchmark performs two million blocked-pending
polls, alternating base then candidate for seven pairs:

```text
base ns:      184 185 187 182 183 201 186
candidate ns: 120 118 118 121 117 153 118
median:       185 -> 118 ns (-36.2%, 1.57x)
```

Warning-strict `cargo test --locked --offline -p hl-task` on the candidate:
131 passed, 0 failed, 1 performance diagnostic ignored.
