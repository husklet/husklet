# Scheduler continuation oracle audit

## Retained-C ownership studied

The read-only oracle inspection covered `../engine/src/core/dispatch.c::run_guest`,
`../engine/src/linux_abi/thread.c::{thread_register,thread_unregister,
thread_target_signal,thread_exit_others}`, and
`../engine/src/linux_abi/signal.c::{host_sig_pend,maybe_deliver_signal,
raise_guest_signal_info}`. `run_guest` owns one CPU for the host thread's full
lifetime, registers it before guest execution, clears and observes `cpu->irq`
only at fully spilled dispatcher boundaries, handles the translated reason,
then delivers pending process- or thread-directed signals. Teardown unregisters
the CPU before its storage can cease being a signal target.

The retained thread registry is mutex-owned. Signal publication first sets the
pending bit and `cpu->irq`, then observes a published blocking wait and wakes it
only when the signal is actionable. Exec/group teardown marks peers exited,
sets their interrupt, wakes blocking waits, and waits for registry retirement.
The architecture-specific dispatcher hooks differ, but both architectures
return to this same lifetime and signal boundary; host signal-number mapping is
confined to the signal implementation.

## Rust mapping and capability matrix

| Capability | Rust owner | State |
| --- | --- | --- |
| Exact thread/process/generation ownership | `ThreadRun`, `SetState::ownership` | implemented |
| Sole-runnable admission under the scheduler lock | `ThreadSet::continuation` | implemented |
| Lock-free continuation check | `SchedulerContinuation::is_current` | implemented |
| A queued state change denies admission before waiting for the set lock | `ContinuationEpoch::request` pending counter | implemented |
| Peer runnable, control, retirement, and replacement invalidation | `ThreadSet`, `ThreadStage`, `PreparedImage` transitions | implemented conservatively |
| Signal and cancellation invalidation | epoch requests plus interrupt/cancellation atomics | implemented |
| Mapping/checkpoint invalidation | `hl-memory` request and checkpoint continuations | implemented separately |
| CPU timer/accounting admission policy | runtime task/time domains | remaining prerequisite |
| Borrowed native refill callback | native executor ABI | remaining prerequisite |

The pending counter is necessary in addition to an epoch: an invalidator can
publish its request before blocking on `SetState`, and admission fails while
that request remains queued. Saturation fails closed. Completion advances the
epoch before releasing the pending marker, so an activation can never be
revived by a completed or overlapping state transition.

Focused tests cover exact sole-runnable issuance and invalidation by peer
publication/resume, interrupt, cancellation, process control, and retirement.
