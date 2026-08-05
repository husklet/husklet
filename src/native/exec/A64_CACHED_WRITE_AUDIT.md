# AArch64 cached writable-view audit

## Retained C oracle

The read-only oracle was `/Users/x/dd/engine` at
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. The complete write/projection
path studied was:

- `src/translator/guest/aarch64/translate.c`: `emit_a64_soft_guard_begin`,
  `emit_a64_soft_guard_end`, `emit_a64_soft_exit_site`, `emit_fold_mem`,
  `aarch64_soft_tlb_miss`, `aarch64_soft_tlb_span`,
  `aarch64_soft_prepare_bounce`, `aarch64_soft_bounce_commit`, and the SMC
  range queue. One CPU owns its cached interval and host delta. A hit performs
  no lock or callback; a discontinuous store validates every span, blocks host
  signals, writes a bounded bounce, and scatters only after success.
- `src/translator/guest/aarch64/cpu.h`: the CPU owns the cached interval,
  permissions, miss metadata, bounce state, signal-mask storage, and SMC
  ranges for its thread lifetime.
- `src/translator/guest/aarch64/dispatch.h`: `R_SOFTMISS`, `R_SOFTSPAN`, and
  `R_SOFTCOMMIT` retry/fault ordering. A failed resolution becomes an
  architectural fault; a bounced store is committed before another guest
  instruction can observe it.
- `src/translator/guest_memory.c`: bind, executable-span resolution, data pin,
  unpin, read, and write entry points. Pins own borrowed storage only until
  unpin.
- `src/translator/cache.c`: `stw_register`, `stw_unregister`,
  `stw_mapping_begin`, `stw_mapping_end`, checkpoint admission, and fork
  repair. Mapping mutation stops admitted CPUs and clears cached intervals
  before snapshots or backing can retire.
- `src/linux_abi/thread.c`: GNA/GRO generation readers and writer locks, BUS
  transition ordering, and file-map publication. Generation readers retry
  concurrent mutation and conservatively fail closed rather than authorize a
  stale write.

The retained ordinary hit path does not maintain Husklet's exact per-store
journal. Executable and discontinuous writes are nevertheless published only
after success. Linux and macOS differ in the direct host-range probe; the
generated AArch64 hit sequence and stop-the-world lifetime rule are shared.

## Husklet ownership and correction

Husklet retains stronger authority. `ProjectionLease` owns checkpoint
admission, the mapping transaction, projected storage, generation, backing,
and write reservations. `run_view_publish` release-publishes at most four
immutable views and their exact publication identities. `guard.c` must reserve
journal capacity before a host store and commit the exact range only after the
store succeeds. Mapping mutation, fork, and teardown cannot reclaim the
storage while that lease remains live.

Before this change, `write_cache` handled an alternation between already
projected writable views by overwriting `memory_first`, `memory_last`, delta,
permissions, write policy, and write index, then retrying the guard. If the old
active view owned completed writes, that mutation happened before
`hl_a64_guard_write_begin` could archive them. Guest bytes were correct while
the returned exact journal could lose or misattribute the preceding owner.

The cached selector now keeps only the selected immutable-view index in x9,
then enters one common activation path. A nonempty old interval first checks
the 16-record capacity and records its exact view and written range. Only then
does activation replace the active view fields and retry the original guard.
Overflow restores NZCV and x9 and exits for epoch service before the guest
store. An empty journal skips archival. Selected-view publication still comes
from the token-acquired immutable payload, and no pointer survives the run
lease.

All current native writer families use this central sequence: scalar integer
and vector stores (1--16 bytes), integer/vector pairs (up to 32 bytes), ordered
stores (1--8 bytes), AdvSIMD structures (up to 64 bytes), and 64-byte DC ZVA.
Exclusive and atomic read-modify-write families remain deliberately declined
to fallback; this change does not partially admit them.

## Verification and measurement limits

On Linux AArch64, warning-strict direct executables built the complete native
source set plus both entry assemblies. `aarch64_single` passed. The complete
`aarch64_trace` executable passed after applying only a temporary test-harness
correction from a pre-existing seven-instruction build limit to the nine
instructions that its adjacent assertion already expects; that unrelated
one-line correction is not part of this commit. The new alternating-write
assertions prove three successive view switches retain the previous exact
owners and ranges while the final interval remains live.

No runtime speed claim is made. At verification time `/tmp` had only 1.1 GiB
free and the shared repository target occupied 54 GiB while other lanes owned
build execution. A clean diagnostics-off retained-C/Rust/native benchmark
could not be built without either violating build ownership or risking the
host's remaining disk. The emitted cache-hit activation is centralized rather
than duplicated four times, but static size is not a substitute for an engine
A/B. The existing scalar-conversion benchmark remains the required clean-tree
performance gate after integration.
