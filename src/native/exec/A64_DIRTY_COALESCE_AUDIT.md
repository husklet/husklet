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

## Baseline re-audit at `cf15cdd33`

This lane re-audited the exact baseline instead of duplicating the active
`agent/a64-projection-cert`, `agent/a64-ingress`, or
`agent/a64-ingress-cert` branches. The retained oracle remained read only.
The exact C entry points followed were `translate.c::{emit_a64_soft_guard_begin,
emit_a64_soft_guard_end,aarch64_soft_tlb_miss,aarch64_soft_tlb_span,
aarch64_soft_prepare_bounce,aarch64_soft_bounce_commit}`, `dispatch.h`'s
`R_SOFTMISS`, `R_SOFTSPAN`, and `R_SOFTCOMMIT` transitions, `cpu.h`'s
task-owned soft-TLB/bounce fields, and `cache.c::{stw_before_translated,
stw_after_translated,stw_mapping_begin,stw_mapping_end,
map_invalidate_source_ranges}`. Rust/C owners followed were
`guard.c::{write_cache,hl_a64_guard_begin_mode,hl_a64_guard_written}`,
`projection.c::{mergeable,flush_dirty,hl_a64_projection_resolve}`,
`trace.c::{hl_a64_trace_loop_preflight,hl_a64_trace_certificate_check}`, and
`executor.c::{run_aarch64,active_view_publish,run_view_publish}`.

The retained CPU owns one tuple for its task lifetime. Registration and
mapping mutation use registry/JIT locks to park peers and clear tuples before
backing retirement; generated hits allocate and lock nothing. Misses record
PC, address, width, and access before dispatch. Cross-span writes block
signals, use bounded bounce storage, restore the prior mask, and commit only
after the copy. Permission failures and synchronous faults publish nothing;
teardown unregisters before reclamation. POSIX STW signalling and the macOS
host-range probe are host branches; emitted AArch64 hits are host-neutral.

Rust pins projected storage with the run-scoped `ProjectionLease` and
authenticates mapping/authority at admission. Its fixed 16-record journal is
CPU-owned and allocation-free. Stores reserve archive capacity before
mutation, record only after success, and force an epoch exit for executable
owners. Owner-qualified overlap/touch coalescing and full-journal preflight
are already present. Rust validates records before reconciliation,
executable/exclusive invalidation, and reservation commit; drop rolls back.

| Capability | Retained C | Rust at `cf15cdd33` | Status |
| --- | --- | --- | --- |
| Writable-view identity | task tuple retired under STW | lease-pinned bounds plus incarnation/authority | implemented |
| Successful/faulting write | post-store commit/no fault commit | post-store exact interval/no fault record | implemented |
| Repeated owner accumulation | coarse current tuple | bounded owner-qualified merge | implemented |
| Capacity/executable write | coarse invalidation | pre-mutation epoch/overflow; exact epoch exit | implemented |
| Cross-span discontinuity | signal-masked bounce | dispatcher fallback | divergent but safe |
| Authenticated certificate | registry-cleared tuple | dormant `certificate_valid/delta` | missing by design |
| Fork/teardown retirement | registry clear/removal | lease plus instance execution gate | implemented for live path |

Mechanically, no code assigns a nonzero `certificate_valid` or
`certificate_delta`; both are cleared at run entry. Setting those dormant
words would not authenticate bounds, permissions, mapping incarnation,
authority identity, or lease generation and could enable cross-page or stale
host access. A coherent certificate therefore belongs to the separate
ingress/lifecycle work and requires mutation, fork, direct-chain, permission,
fault, and teardown tests. It is not a safe dirty-journal-only edit.
