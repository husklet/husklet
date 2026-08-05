# AArch64 contiguous dirty publication audit

## Retained C oracle and Rust mapping

The read-only oracle was `/Users/x/dd/engine`. The complete relevant path was
`src/translator/guest/aarch64/translate.c` (`emit_a64_soft_guard_begin`,
`emit_a64_soft_guard_end`, `aarch64_soft_prepare_bounce`, and
`aarch64_soft_bounce_commit`), `dispatch.h` (`R_SOFTTLB`, `R_SOFTSPAN`, and
`R_SOFTCOMMIT` ordering), `cpu.h` (soft-TLB span, bounce and vector-dirty
ownership), and `src/translator/cache.c` (mapping stop-the-world invalidation).
The retained fast path caches one mapping-qualified interval. Sequential stores
inside it do not repeatedly republish its invariant owner; executable/bounce
publication occurs only after successful stores, and discontinuous or faulting
accesses enter the cold path.

Husklet's stronger exact owner is `guard.c` plus `projection.c`, the executor's
generation-qualified run views, and `ProjectionLease::publish_written_ranges`.
The generated guard records an exact successful-store interval only after the
host instruction. A first store establishes `dirty_view_first/last`,
`memory_written`, and the executable bit. A disjoint store archives that tuple
before starting another. An overlapping store merges exact bounds. Mapping,
checkpoint and backing lifetime remain retained by the projection lease.

Before this change, the already-specialized contiguous path extended
`dirty_last` and then redundantly reloaded and rewrote the unchanged view owner,
written bit and executable bit. The fast continuation now extends only the
exact interval end, while retaining both completed/merged diagnostics. It is
reachable only after `dirty_first != UINT64_MAX`, `address == dirty_last`, and
the active `memory_first/last` exactly matches `dirty_view_first/last`. The last
qualification is load-bearing: cached run views may be adjacent in guest space
while owning different projections. Such a transition archives the prior exact
record and establishes the new owner. A fault cannot reach the continuation
because this emitter runs after the guest store. No range is widened, no view is
inferred, and coarse publication is not used.

## Verification and performance

The focused AArch64 single-store executable initializes a qualified interval,
performs the adjacent store, and proves exact end extension plus unchanged view,
written and executable ownership. Further regressions make distinct views
ascending-adjacent, descending-adjacent, and guest-overlapping, and prove the
prior owner/range is archived before the new exact interval is established.
View mismatch branches directly to archival; only address non-contiguity enters
the range-relation logic, so its comparisons never consume view-comparison
flags. The suite also retains the existing pre-store
journal-capacity refusal and cross-window fault tests. The warning-strict native
executor dirty-journal tests passed, as did the standalone warning-strict
`aarch64_single.c` executable.

One release runner and guest drove both the scalar-conversion parent and this
candidate with typed native diagnostics, divisor 100, and three repetitions.
The parent median was 1,805,477 us; the final flag-safe, view-qualified candidate
median was 263,392 us (261,143--263,452), an 85.41% reduction. Checksum
23,027,281,045 was identical. Candidate counters were exact across all three
repeats: 43,392,048 completed instructions, 16,158,994 exact guards, 5,375,202
committed stores, 5,373,557 merged stores, zero journal overflows and 1,630 guard
fallbacks. They intentionally differ from the unqualified prototype: adjacent
distinct views now archive separate records rather than incorrectly merging by
guest address alone, allowing the complete workload to remain native.

A CPU-17 matrix using the same candidate measured host-native at 5,292 us,
retained C at 5,465 us, and Rust at 971,404 us, all with the same checksum.
Host load was high (28.78 one-minute), so these absolute figures locate the
remaining 183.56-times native gap rather than claim a stable release result.
