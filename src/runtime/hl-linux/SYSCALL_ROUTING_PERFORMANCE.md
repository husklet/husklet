# Syscall routing performance audit

## Retained oracle

The read-only retained implementation was audited at
`../engine/src/linux_abi/syscall/dispatch.c` (`service`, `service_local` and the
ordered family calls), `../engine/src/linux_abi/number.c`
(`normalize_syscall_number`), `../engine/src/linux_abi/seccomp.c`
(`seccomp_gate` and `hl_linux_seccomp_apply`), and the syscall entry includes in
`src/core/target/aarch64.c` and `src/core/target/x86_64.c`. The family fragments
included by `dispatch.c` were inventoried as `binding.c`, `aio.c`, `event.c`,
`fs.c`, `guest_copy.c`, `helpers.c`, `inotify.c`, `io.c`, `mem.c`, `misc.c`,
`net.c`, `proc.c`, `ptrace.c`, `rare.c`, `signal.c`, `sysv.c`, and `time.c`.

The CPU record owns the raw register frame for the synchronous service window.
`service` publishes the in-service state before checking pending signals, keeps
the original number and aliased argument registers alive for restart, evaluates
thread-owned seccomp state before normalization, and routes traced or untrusted
tasks through their respective gates. `service_local` normalizes x86 legacy
numbers with a compiled switch, snapshots all six arguments, polls filesystem
cache generation, performs namespace invalidation, and calls the ordered family
handlers. Blocking, partial-result, cancellation, signal delivery, errno, and
teardown semantics remain owned by those family handlers. AArch64 numbers are
already canonical; x86-64 maps raw numbers through the compiled switch and
rewrites its legacy-only forms before common dispatch. Dormant seccomp and ptrace
state add predicted gates but do not change ordinary-call ownership.

## Rust comparison

| retained capability | Rust owner | result |
|---|---|---|
| register frame decoding and result publication | `syscall/frame.rs`, engine execution routing | implemented |
| AArch64 canonical number selection | `syscall/table.rs` | implemented |
| x86 raw-to-canonical selection | `syscall/table.rs` | implemented |
| legacy x86-only operations | `X86_LEGACY_SYSCALLS` | implemented |
| unsupported versus reserved distinction | `Disposition` | implemented |
| family ownership and typed delegation | `syscall/ports.rs` | implemented |
| policy before operation dispatch | `dispatch_seccomp`, runtime seccomp control | implemented |
| constant-time compiled number selection | retained `number.c` switch | previously divergent |

Before this change, every ordinary Rust syscall linearly scanned
`CANONICAL_SYSCALLS`, including deep, frequent identity operations. The macro
that remains the single source of truth now emits both the public definition
arrays and compiled architecture-specific match dispatch. This changes only
selection complexity; the returned operation, translation, family, legacy
precedence, unsupported boundary, and reserved behavior are unchanged.

## Evidence

The ignored `route_lookup_benchmark` alternates architectures and a fixed mix of
identity and unsupported numbers for 20,000,000 lookups. Release binaries from
the exact baseline `38417e9680b123aef723157de8f5cf4f7bf8e034` and this candidate
were run as alternating pairs. Baseline times were 1,305,544,917--1,353,525,500
ns (median 1,319,490,958 ns); candidate times were
38,335,500--41,629,125 ns (median 40,552,625 ns), a **32.54x** median speedup.
All checksums were zero. Warning-strict `hl-linux` tests passed 165/165 with the
benchmark excluded by default.

The same content-bound AArch64 combined guest
(`a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9`)
then ran through release engine binaries pinned to CPU 17 with native execution
and diagnostics required (`--divisor 1000 --phase syscall`, seven repeats).
Baseline Rust was 4,295 us and the candidate was 3,441 us, a **19.88%** reduction.
The candidate still takes 3.45x host-native time, so routing is improved but is
not the remaining whole syscall gap. Both runs reported the same 499 syscall
exits, 501 native runs, 12 builds, 527 hits, 2 fallbacks, 9,512 completed guest
instructions, and the phase checksum `20000`. Retained C was 988--1,004 us and
host native was 948--997 us across the two admitted runs.

## Dormant-policy identity admission

The next full-boundary profile used integrated commit
`d9749b7a5d2e5f26732079aefc1c00a3f7bb6dbe`. Retained `service` checks the
monotonic global `g_seccomp_active` gate before entering local dispatch; when it
is false, identity calls proceed without policy storage or a lock. Rust already
made the equivalent `ever_active` flag monotonic, but `RuntimeSyscallRouter`
still locked the omnibus mutable `RouterDependencies`, invoked the seccomp
trait, and only then returned its immutable per-thread `getpid`/`gettid` value.
That serialized unrelated dependency state on every identity call.

`RuntimeSyscallRouter` now receives the concrete process `SeccompControl` as an
admission capability. Only identity operations use its acquire-load fast gate.
If the gate is false, immutable task identity is returned without locking the
ports. Strict or filter installation publishes the flag with release ordering
before taking the policy lock, and the flag never becomes false again, so an
installation cannot race a later bypass and rollback or task retirement cannot
reopen it. Every non-identity call and every call after first activation retains
the existing locked trait-dispatch path. Frame decoding, architecture number
selection, result publication, restart state, tracing, exit ownership, errno,
and signal order are unchanged.

On CPU 17, the same content-bound guest and exact matrix settings used above ran
nine repeats against clean release binaries. Baseline Rust was 3,256 us with
12,201 us wall time; candidate Rust was 3,055 us with 10,847 us wall time. This
is a **6.17%** guest-time and **11.10%** wall-time reduction. Both reported 499
syscalls, 501 native runs, 12 builds, 527 hits, two fallbacks, 9,512 completed
instructions, and checksum `20000`. The candidate remains 3.49x host-native;
the remaining gap is outside the dormant-policy ports lock removed here.
