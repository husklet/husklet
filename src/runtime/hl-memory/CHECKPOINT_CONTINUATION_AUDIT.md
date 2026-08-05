# Checkpoint continuation audit

## Oracle and ownership

This slice was audited against retained engine revision `7b7bddddf` without
editing `/Users/x/dd/engine`:

- `src/core/dispatch.c::run_guest` admits checkpoint work only at a fully
  spilled dispatcher boundary.
- `src/translator/cache.c::{stw_dispatch_safepoint,stw_before_translated,
  stw_after_translated,stw_checkpoint_arm,stw_checkpoint_wait,
  stw_checkpoint_cpus,stw_checkpoint_end}` publishes the checkpoint gate and request before
  waiting for registered CPUs. It interrupts every peer, preserves the JIT to
  registry lock order, and keeps the registry stable through capture.
- `src/linux_abi/checkpoint.c` owns image capture/restore and sanitizes
  host-transient execution state.
- `src/linux_abi/thread.c::{thread_register,thread_unregister,
  thread_target_signal,thread_exit_others}` owns thread inventory, directed
  wakeup, cancellation, and teardown around checkpoint boundaries.
- `src/linux_abi/signal.c::maybe_deliver_signal` owns delivery only after the
  architectural image is coherent. Checkpoint polling does not reorder signal
  frame construction or partial syscall results.

The Rust owner is `CheckpointActivity`. It serializes admission, freeze, exit,
termination, and thaw with one mutex/condition variable. `ProjectionLease`
retains one admission and the mapping transaction for the lifetime of raw host
projection authority. Mapping invalidation, scheduler generations, signals,
cancellation, CPU timers, and native instruction budgets remain separate
domains.

## Capability matrix

| Capability | Retained C | Rust before this slice | This slice |
|---|---|---|---|
| Request visibility | Atomic request/gate before acknowledgement wait | Only mutex-protected `frozen` | Monotonic atomic request epoch |
| Admission identity | Registered CPU and dispatcher acknowledgement | Counted anonymous admission | Token captures epoch while admission mutex is held |
| Freeze ordering | Request, interrupt, then wait | Set frozen, then wait | Epoch advances before frozen wait begins |
| Exit/termination | Interrupt and retire registered threads | Freeze/terminal state under mutex | Both invalidate existing tokens before publication |
| Thaw | Releases gate; old acknowledgement is stale | Clears frozen | Does not revive an old token |
| Signal/cancellation | Separate dispatcher/thread owners | Separate task/scheduler owners | Unchanged and explicitly not represented by token |
| Mapping transition | Same retained STW request family | Independent mapping transaction | Still missing; token is checkpoint-only |
| Fault/dirty publication | Fully spilled before capture | Lease drop commits or rolls back | Unchanged |

## Contract

`CheckpointContinuation` is lock-free evidence that no checkpoint, address
space exit, or termination request occurred after its projection admission.
It is deliberately necessary but insufficient for continuing native work. A
consumer must additionally validate mapping-transition, scheduler-generation,
signal, cancellation, timer, executable-write, and fault state.

The epoch advances while holding the activity mutex, before setting the freeze
state or waiting for admissions. This closes the admission race: an admission
that wins the mutex captures the old epoch; the subsequent freezer invalidates
it before waiting. An admission that observes the freeze cannot complete until
thaw and therefore captures the later epoch only after the request is over.

## Evidence

Fail-first tests cover invalidation of a live public `ProjectionLease` before a
concurrent freeze can complete, continued blocking until lease release, no
token revival after thaw, and exit/termination invalidation. This slice does
not exercise native continuation and makes no performance claim.
