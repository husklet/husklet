# AArch64 dirty-record coalescing audit

## Retained-C oracle

The read-only oracle was `/Users/x/dd/engine` at
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. The complete write-observation
path inspected for this lane was:

- `src/translator/guest/aarch64/translate.c`: translated load/store entry
  points and the successful-store publication boundary;
- `src/translator/guest/aarch64/cpu.h` and `dispatch.h`: soft-TLB ownership,
  dispatcher state, and translated-block exits;
- `src/translator/guest_memory.c`: guest-view resolution, permission checks,
  partial failure, and write observation;
- `src/translator/cache.c`: translated-code invalidation and cache lifetime;
- `src/linux_abi/thread.c`: task ownership, teardown, and signal interaction.

The retained implementation owns one current soft-TLB view. It publishes
self-modifying-code and bounce-buffer effects only after a successful store;
failed stores publish nothing. Its state is task-owned, and dispatcher/cache
locks do not span guest memory access. Architecture-specific address recovery
is confined to AArch64 translation and dispatch. There is no retained exact
per-store journal, so the Rust journal is an implementation mechanism rather
than guest-visible behavior.

## Rust capability mapping

`guard.c` owns generated successful-store accounting, cached-view switches,
and epoch exits. `projection.c` owns dispatcher-side view changes and final
journal flushing. `trace.c` owns admission when an existing journal is already
full. `executor.c` consumes the bounded records after native execution.

Before this change all archive paths appended. Alternating between two cached
owners therefore exhausted the 16-record journal even when every archived
interval exactly overlapped an earlier interval for that owner. The resulting
epoch exits were bookkeeping pressure, not a semantic boundary present in the
retained implementation.

All three admission/publication paths now use the same rule: an active exact
interval may reuse a record only when the owner bounds are identical and the
dirty ranges overlap or touch. Coalescing expands only the dirty interval and
never changes its owner. Otherwise the bounded append/overflow behavior is
unchanged. Generated code archives a completed old interval before attempting
a store through a different owner; the possibly faulting new store therefore
cannot publish itself early. Guest register x9, NZCV, and the selected cache
slot survive the cold bounded scan.

## Evidence

The regression starts with all 16 records occupied, including records for two
alternating owners. Two stores switch owners before a syscall. The old code
could not archive the first completed interval and exited at the capacity
boundary; the candidate merges both completed intervals, reaches the syscall,
does not set overflow, and retains 16 records. The existing alternating-owner
case also verifies that a changed owner extent is not falsely merged.

On Linux AArch64, direct native executables compiled with
`-std=c11 -Wall -Wextra -Werror -O2` passed:

- `aarch64_trace` with the repository's pre-existing SIMD trace-limit typo
  corrected locally from 7 to 9 for the run and reverted afterward;
- `aarch64_single` without test changes.

Performance timing is intentionally deferred to a coordinated quiet window.
At `7ac681960`, the testing benchmark runner re-enables diagnostics whenever
native execution is selected, so authoritative diagnostics-off A/B must invoke
the two clean `hl-engine` binaries directly after one diagnostics-on provenance
row. The comparison target is the recorded 36.297x baseline.
