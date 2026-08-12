# Native slice admission audit

> **Historical replacement-engine audit:** `ExecutionMachine` and its native
> slice admission path were deleted. Production selection is C-only; this file
> remains evidence for any future replacement proposal.

## Retained oracle

The read-only comparison covered
`../engine/src/linux_abi/syscall/dispatch.c::service` and `service_local`,
`../engine/src/core/dispatch.c::run_block`, `block_return`, and `run_guest`, and
`../engine/src/translator/guest/aarch64/stubs.c::emit_prologue` and
`emit_spill`. The retained run loop owns one `struct cpu` for the task lifetime.
A synchronous syscall publishes its complete architectural state before
`service`, which reads and updates that same record directly. Returning from the
service resumes the same CPU and cache owner. Signal selection occurs only at a
published boundary; restart restores the aliased argument/number registers
before resumption. The AArch64 and x86-64 frontends have distinct register
layouts, but neither reacquires the CPU owner merely to distinguish the ISA and
then reacquires it again to read the program coordinates.

## Rust ownership comparison

`ExecutionMachine` owns its CPU snapshot and cache epoch behind one mutex.
`Scheduler::native_slice` previously called `handle_syscall` once to distinguish
x86-64 from AArch64, released the state owner, and on AArch64 immediately called
it again to read `pc` and `sp`. No operation, signal, epoch change, or ownership
transfer was permitted between these adjacent admissions, so the second lock
could not observe useful new state. The scheduler now captures one typed
`NativeCoordinates` value under one admission. x86-64 still routes directly to
its native executor; AArch64 retains the same `pc`, `sp`, executable-token,
mapping-generation, suppression, budget, signal, restart, and publication
checks.

## Exact A/B evidence

Baseline commit `e1f11f306bb2c3862cf4e1a4daa167b94981af1a` and the candidate
were built warning-strict in separate target directories. A content-bound
AArch64 guest derived from `combined/main.c` used `SYS_getuid` so the phase
exercised the ordinary non-identity task port instead of the immutable
`getpid`/`gettid` shortcut. Its SHA-256 was
`612ff3035b6a04041042a67e44c6e9963179c7c6bc5e0f9844a5c10483bea4b7`.
Nine repeats ran on CPU 17 with `--divisor 1000 --phase syscall`.

| revision | guest us | wall us | checksum |
|---|---:|---:|---:|
| baseline | 506,395 | 681,265 | 5,010,000 |
| candidate | 157,883 | 192,226 | 5,010,000 |

Guest time fell **68.82%** and wall time fell **71.78%**. Both revisions
reported 499 syscalls, 501 native runs, eight builds, 519 hits, two fallbacks,
six branch exits, and 9,522 completed instructions. Retained C was 843--868 us
and host-native was 847--958 us across the admitted runs. The remaining large
gap is not attributed to this admission anymore: source profiling shows that
the `getuid` handler constructs a full `TaskRegistry::snapshot`, scanning all
configured slots and cloning unrelated process/thread state to read one process
credential. That is a separate task-observation domain and is intentionally not
combined with this native-boundary change.
