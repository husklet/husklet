# CPU interval-timer publication audit

Audit date: 2026-08-05. Rust baseline: `3714e8eda` plus the current lane.
The retained `../engine` tree was read-only.

## Retained implementation studied

- `../engine/src/core/target/x86_64.c::legacy_set_alarm` constructs a one-shot
  `itimerval`, calls host `setitimer(ITIMER_REAL)`, and rounds the returned
  remaining duration up to seconds. The host owns timer storage, expiry, and
  signal interruption; there is no engine lock or timer object to tear down.
- `../engine/src/translator/guest/x86_64/legacy.c` dispatches guest `alarm` via
  the injected `set_alarm` callback and returns its errno/result.
- `../engine/src/translator/guest/x86_64/{legacy.h,abi.h}` define and install
  that callback boundary.
- `../engine/src/linux_abi/host_signal.h::{setitimer,getitimer}` supplies the
  host ABI and the `ITIMER_REAL`, `ITIMER_VIRTUAL`, and `ITIMER_PROF` identities.
  Host signal delivery supplies `SIGALRM`, `SIGVTALRM`, and `SIGPROF` ordering.

The retained path has no architecture-specific CPU-timer implementation beyond
the x86-64 legacy callback. POSIX host branches live in `host_signal.h`; the
host kernel serializes set/get/expiry and process teardown. Partial I/O is not
involved. Invalid timer values/identities report syscall errno, and expiry is
observed at the existing guest signal boundary.

## Rust ownership and lifecycle

`hl-runtime::AlarmRegistry` owns `ITIMER_REAL` scheduler tokens and the two CPU
timers. `setitimer` reads the prior state, changes deadline/interval under the
CPU-timer mutex, and publishes afterward. `getitimer` reads the same serialized
state. Scheduler-boundary polling advances an expired periodic deadline (or
disarms a one-shot) before enqueueing `SIGVTALRM`/`SIGPROF`; signal enqueue and
waiter interruption therefore remain outside the timer lock.

Fork creates a new generation-qualified `ProcessId` and no CPU-timer entry, as
Linux requires. Exec retains the same process identity and registry, so timers
survive exec. Last-thread and group exit call `AlarmRegistry::remove`; removal
publishes disarmed state before a retained handle can be discarded. PID reuse
gets a distinct publication object. STOP/CONT and ordinary signal handling do
not replace timer state. Timer delivery remains through `TaskRegistry`, after
the CPU accounting boundary has published its charge.

## Capability matrix

| Capability | Rust owner | State |
| --- | --- | --- |
| Real-time set/get/cancel and periodic rearm | `AlarmRegistry::{replace,current,arm,expire}` | implemented |
| CPU-time set/get and expiry | `AlarmRegistry::{replace_cpu,current_cpu,poll_cpu}` | implemented |
| Process-lifetime CPU accounting | `hl_task::CpuAccount` | implemented at `3714e8eda` |
| Lock-free armed/deadline/generation observation | `CpuTimerPublication::snapshot` | implemented |
| Generation-safe exit and PID reuse | `AlarmRegistry::remove`, publication `Arc` identity | implemented |
| Fork does not inherit timers; exec preserves timers | process identity/lifecycle composition | implemented |
| Native continuation consumes publication and invalidates on timer change | x86 scheduler/native admission | remaining integration gap |

Publication is a bounded seqlock-style snapshot. Writers are serialized by the
existing timer mutex, mark the generation odd before changing deadline fields,
and release an even generation afterward. Readers perform at most three
attempts and report an armed deadline on instability. Generation exhaustion is
terminal odd state, preventing wraparound from validating a stale continuation.
