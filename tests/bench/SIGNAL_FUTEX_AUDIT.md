# Task, signal, and futex performance audit

## Retained C oracle

The read-only oracle was `/Users/x/dd/engine` at commit
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`.

The signal call graph studied was:

- `src/linux_abi/syscall/signal.c`: `thread_kill`, the `kill`, `tkill`, and
  `tgkill` cases in `signal_op`, and the `rt_sigreturn` boundary;
- `src/linux_abi/signal.c`: `maybe_deliver_signal`, `build_signal_frame`,
  `sigreturn_frame`, and `do_sigreturn` (with architecture-specific frame code
  in `src/translator/guest/{aarch64,x86_64}/signal.c`);
- `src/linux_abi/thread.c`: `thread_target_signal`, `thread_wait_publish`,
  `thread_wait_clear`, `thread_register`, and `thread_unregister`.

The C engine owns one fixed live-thread registry guarded by `g_threg_m`.
Thread-directed delivery scans that registry under the lock, sets pending and
interrupt state on the selected CPU, and wakes only its published blocking
primitive. It does not serialize or clone process state. A signal frame is
built only at a dispatcher boundary, and `rt_sigreturn` restores it before the
next delivery. Registry entries are installed at guest-thread entry and cleared
at teardown; signal-versus-wait ordering is publish, recheck, then park.

The futex call graph studied was:

- `src/linux_abi/syscall/proc.c`: the `futex` syscall admission and key choice;
- `src/linux_abi/thread.c`: `futex_op`, `futex_wake_bucket`, `fbk_park`,
  `fbk_unpark`, `fbk_wait_register`, `fbk_wait_grant`, PI lock/unlock, robust
  exit, clear-child-tid wake, and fork repair;
- `src/host/sync.c`: host futex park/wake on Linux.

Private and shared futex tables have distinct lifetimes. Bucket mutexes own the
value-check/registration/wake order, fixed waiter slots carry exact wake grants,
and condvars are transport rather than authority. Waits preserve mismatch,
timeout, interruption, spurious-wake, bitset, requeue, PI, robust-exit, and
cross-process key semantics. Only the private table is repaired after fork.

## Rust ownership comparison

The Rust signal path studied was
`hl-runtime/src/signal/send.rs` -> `hl-task::TaskRegistry` ->
`hl-engine/src/ffi/linux/execution/scheduler.rs::signal_boundary` ->
`hl-runtime/src/signal/{boundary,frame}.rs`. Frame installation is a
transaction across execution-owned CPU/memory publication and task-owned
pending/frame state. That is intentionally more defensive than C and remains
unchanged.

Before this change, every `tkill` and `tgkill` called
`TaskRegistry::snapshot()`. The snapshot is checkpoint-shaped: it clones every
live process and thread, process arguments, credentials, pending queues, signal
frames, namespaces, generations, waits, sessions, and groups. Glibc `raise()`
uses `tgkill`, so the cost occurred for every iteration of the signal benchmark
even though delivery needs only four values: target thread, target process,
sender credentials, and target credentials.

The accepted ownership is a compact `SignalThreadTarget` read under one task
registry lock. The guest thread number maps directly to its slot, making the
lookup O(1); the live entry supplies the current generation. Linux policy remains
in `hl-runtime`; `hl-task` only resolves
task identity and owned credentials. Generation-qualified `ThreadId` and
`ProcessId` still flow into enqueue, so a recycled slot cannot receive the
signal. Queueing, interrupt acknowledgement, frame transactions, masks,
restart semantics, and teardown are unchanged.

Authorization and enqueue remain separate registry transactions. Credentials
can therefore change in between, but this does not widen the pre-existing race:
the old checkpoint snapshot released the same registry lock before authorization
and enqueue. The compact projection is internally coherent and enqueue still
validates the generation-qualified target under its own lock.

The Rust futex path studied was `hl-runtime/src/process/time.rs` ->
`hl-runtime/src/futex_port.rs` -> `hl-sync/src/futex.rs` and
`hl-sync/src/lib.rs`. It owns generation-qualified private/shared keys,
compare-and-register atomicity through the memory port, bounded bucket/key and
waiter counts, exact wake election for wait vectors, interruption registration,
PI ownership, robust exit, and fork reset. No futex change is included in this
lane because the measured benchmark exercises synchronous signal frames, and a
signal-path mechanism was independently identified before editing.

## Baselines and verification

The pinned cross-engine checkpoint in `PERFORMANCE.md` measured the folder-owned
`combined/main.c` signal phase with in-guest timing. Retained C versus Rust
native medians were 23,969 versus 3,166,848 microseconds on ARM64 (132.123x),
and 15,261 versus 2,701,050 microseconds on AMD64 (176.990x). Checksums matched
and Rust emitted nonzero typed native diagnostics.

This lane uses the same source with `--divisor 20 --phase signal`; startup and
compilation are excluded by the guest's own phase timer. Exact base, artifact
hashes, before/after samples, and test commands are appended when the isolated
run completes.

Acceptance compared exact base
`0f3927e8babd4520ed26559eee36dc49e74db47f` (A) with exact candidate
`962ef79740ab736e04039e611fca5366ebc78df6` (B). The independently built
release `testing` artifacts were:

- A: `d3f136b0e426ec79fdebad693add78a9395bc993e01889630bf3a3dd20658867`;
- B: `fd1e4debe111f60ce7c13b0d943b3e29fe899a23800d92da8c4574800d2c00e7`.

Both used the identical ARM64 guest
`a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9`.
The host had no competing build or benchmark and load average was 1.55 before
the run. Every process was pinned to logical CPU 17. Pair order alternated AB,
BA, AB, BA, AB; each cell contained one warm-up, one cold observation, and
three scored in-guest samples.

| pair | order | A median us | B median us | reduction | speedup |
|---:|:---:|---:|---:|---:|---:|
| 1 | AB | 2,798,909 | 1,455,412 | 48.00% | 1.923x |
| 2 | BA | 2,741,372 | 1,510,962 | 44.88% | 1.814x |
| 3 | AB | 2,829,953 | 1,572,437 | 44.44% | 1.800x |
| 4 | BA | 2,767,030 | 1,631,826 | 41.03% | 1.696x |
| 5 | AB | 2,809,562 | 1,632,063 | 41.91% | 1.721x |

The median of pair medians is 2,798,909 microseconds for A and 1,572,437 for
B: a 43.82% reduction, or 1.78x speedup. B was faster in all five pairs.
Every one of the 50 observed executions reported identical native counters:
`runs=110006 builds=19 hits=110040 fallbacks=4 sites=4 services=17`, with
`branch=11 syscall=110002 fallback=4 yield=0 completed=605052` and identical
guard/dirty counters. All ten cells passed with the expected checksum.
Snapshot amplification was therefore a large generic cost, while signal-frame
and service-boundary work remain.

Commands:

```text
nix develop --command cargo test --locked --offline -p hl-task -p hl-runtime --lib --no-run
nix develop --command cargo test --locked --offline -p hl-runtime --lib process_syscalls::tests::kill_exact_thread -- --exact
nix develop --command cargo test --locked --offline -p hl-task --lib registry::test::signal_thread_target_is_generation_qualified -- --exact
taskset -c 17 timeout 45s <A-or-B-testing> bench combined --isa arm64 --jobs 1
```

For the benchmark only, the folder argument list was temporarily narrowed to
`--divisor 20 --phase signal` and restored before commit. The combined
warning-strict runtime/task run reported all 603 `hl-runtime` tests passing.
Three unrelated `hl-task` lifecycle tests already fail on this base because
their expected slot counts differ from current behavior; the focused task and
signal tests below are the acceptance evidence for this bounded change.
