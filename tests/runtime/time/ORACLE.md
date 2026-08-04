# Time compatibility oracle audit

This category is the one-to-one migration of the 40 retained time fixtures formerly under
`tests/runtime/legacy/oracle/tests/compat/time`. Each YAML case preserves its source bytes, both
guest targets (except the intentionally AMD64-only non-PIE signal-fold case), compiler flags,
empty argv/environment, exit status, and deterministic stdout contract. The old manifest maps
directly by case id to `runtime/time/<case>` and `expected/shared/<name>.out` to
`golden/<name>.out`; no registry row is needed to reconstruct that mapping.

## Retained C implementation studied

- `../engine/src/linux_abi/syscall/time.c`: `svc_time`, `engine_clock_gettime`,
  `engine_dynamic_cpu_clock`, `engine_sleep_until`, the `gtimer_*` catalog/worker, and syscall
  cases 101, 113--115 and 107--111.
- `../engine/src/linux_abi/syscall/event.c`: `svc_event` timerfd cases 85--87 and
  `kqueue_rebuild_after_fork`.
- `../engine/src/linux_abi/syscall/io.c`: timerfd read accounting and OFD sharing in
  `fd_carry_virt` paths.
- `../engine/src/linux_abi/syscall/rare.c`: getitimer/setitimer cases 102--103 and
  clock_settime case 112.
- `../engine/src/linux_abi/syscall/nonpie_args.h`: guest-pointer folding for clock, sleep,
  itimer, POSIX-timer, and timerfd arguments.

No retained assembly entry owns time semantics: both guest architectures reach these C handlers
through the shared syscall dispatcher, so there is no time-specific assembly file to port.

The C POSIX-timer catalog is process-global dynamic storage guarded by `g_gtimer_lk`. A single
poll worker owns host timer events; slots retain allocation identity, clock, sigevent, absolute
deadline, interval, generation-like delivery accounting, and overrun state. Delete disarms and
removes a queued tagged signal before slot reuse. Fork discards the inherited worker/poll handle
and lazily recreates them. Timerfd state is keyed by descriptor but shares deadline, interval,
expiration count, and locking across duplicated open-file descriptions; close tears down the
last owner, while fork rebuilds host kqueue state. Sleep loops use fixed deadlines, distinguish
real interruptions from internal wakes, return `EINTR` with the remaining relative duration,
and preserve Linux validation order (`EINVAL`, `EBADF`, then `EFAULT` where applicable).
Clock ids are translated explicitly, including dynamic process/thread CPU clocks and TAI;
unknown clocks are rejected rather than aliased. The macOS branches emulate absolute sleeps,
timerfd, and POSIX timers through host clocks/kqueue, while Linux can use native descriptor
mechanisms. The syscall-number and guest-pointer tables cover both AArch64 and AMD64; the sole
fixture target exception is the AMD64 non-PIE address-fold regression.

## Capability map

| Retained C capability | Rust owner | Migration assessment |
|---|---|---|
| realtime, monotonic, raw, coarse, boottime, TAI and CPU clock reads | `hl-runtime/process/time.rs`, `ClockPort`, `CpuClockPort`; validation in `hl-linux/signal` | Implemented with typed clock identities and staged guest writes |
| relative/absolute nanosleep, deadline recomputation, interruption remainder | `hl-runtime/process/time.rs::sleep_with`, `RuntimeSleepPort`; `hl-linux/futex/time.rs` | Implemented; interruption is an explicit outcome |
| gettimeofday/time and guest structure marshalling | `hl-runtime/process/time.rs`; `hl-linux` marshaller | Implemented with bounded staged copies |
| getitimer/setitimer ownership and signal delivery | `hl-runtime/process/itimer.rs`, process alarm registry | Implemented as process-owned state, not ambient host state |
| POSIX timer allocation, arm/disarm, periodic expiry, overrun, delete race | `hl-runtime/process/timer.rs::TimerRegistry` | Implemented with bounded slots, mutex ownership, generation checks, and tagged pending-signal removal |
| SIGEV_SIGNAL/SIGEV_THREAD targeting | `TimerRegistry` plus task/signal delivery ports | Implemented through guest task ownership; no host signal is injected |
| timerfd clocks/flags, OFD identity, blocking reads, poll readiness and checkpoint | `hl-runtime/event/syscalls.rs`, `hl-event::TimerFd`, descriptor operations/catalog | Implemented with OFD identity and explicit resource registration |
| fork/exec/close lifetime | runtime descriptor, process, event, and checkpoint composition | Implemented by owned catalogs; unlike C, no process-global mutable timer table |
| Linux errno and pointer-validation ordering | `hl-linux` time/event ABI plus runtime error mapping | Implemented and exercised by the error-surface fixtures |
| host-specific clocks and waits | application-supplied clock/sleep/timer ports | Implemented behind narrow ports; host policy does not enter Linux ABI types |

The Rust split replaces the C global tables and host-signal worker with process-owned registries,
descriptor/OFD identities, bounded catalogs, and consumer-owned ports. The acceptance category
therefore remains intentionally broad: it checks clock identity and ordering, CPU time, sleep and
wait interruption, interval timers, POSIX timers, timerfd lifecycle/accounting, validation order,
calendar libc agreement, and non-PIE signal delivery as a single Linux time domain.

## Evidence and declared divergence

An 18-worker direct cross-build produced 39 static AArch64 ELFs and 40 static AMD64 ELFs using
the exact per-case flags. Parallel QEMU execution matched 77 of 79 applicable golden contracts.
Both QEMU executions of `runtime/time/posixtimer` reported `capacity=0` where the engine contract
requires at least 64 simultaneously live POSIX timer ids (`capacity=1`). That provider limitation
is represented explicitly as `!broken`; it is not hidden by weakening the source or golden. All
other cases remain active. The legacy map is exactly the former 40 manifest data rows, with every
old source and golden represented once in this folder; the legacy category directory is deleted.
