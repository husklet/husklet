# Native memory performance

## 2026-08-03 AArch64 memory profile

The pinned `combined-bench --phase memory --divisor 20` comparison remains
dominated by translated memory mechanics, not translation publication or Rust
callbacks. The retained C engine completes the phase in 6.564 ms guest time;
the current Rust/native path is approximately 2.58 s, about 393 times slower.
Checksums agree.

Ranked evidence:

1. **Per-access bounds and dirty-journal code is the largest demonstrated
   cost.** The native guard performs checked end construction, projection range
   and permission tests, flag preservation, and delta application for every
   access. Stores additionally reserve journal capacity before mutation and
   merge or append the exact range after success. The rejected authenticated
   16-byte vector experiment removed almost all fallback boundaries (151,095 to
   30) but increased the three-run guest median from 2.582 s to 2.839 s and wall
   median from 2.634 s to 3.140 s. Keeping more instructions native therefore
   made the phase 9.95% slower in guest time and 19.2% slower in wall time when
   each access retained the full guard/journal sequence.
2. **The 151k boundaries are mostly native read-cache misses, not expensive
   Rust projections.** The scalar-store checkpoint reports 151,095 fallback
   boundaries, 150,420 native operand-cache hits, and only 666 resolver
   callbacks. Resolver callbacks are 0.44% of the boundaries. Increasing the
   view cache from four to eight entries previously reduced callbacks by 42%
   without a measurable speedup. Optimizing the resolver or adding view slots
   cannot close the current gap.
3. **Authenticated scalar specialization reaches too little hot work to move
   the total.** Scalar reads/stores reduced callbacks from 686 to 666 and gave
   less than a 1% timing change. Stable descriptor identity prevents
   re-registration from discarding equivalent translations, so token churn is
   no longer the explanation.
4. **Translation and publication are secondary at steady state.** Pinned
   repetitions have identical counters. The scalar-store run builds 121 blocks
   and records 151,815 cache hits while completing about 35.1 million guest
   instructions. There is no repeat-to-repeat cache-thrash signature.
5. **Slice admission is visible but not proportional to the gap.** The pinned
   run enters native execution 536 times. Even assigning all non-execution work
   to those entries cannot explain the retained engine's roughly 393x guest
   advantage while the emitted access sequences remain large.

The retained C implementation explains the contrast. It runs guest-addressed
loads and stores directly inside long translated regions, handles exceptional
mapping cases through its soft TLB/fault boundary, folds hot loop backedges,
and aggregates SMC writeback. It does not execute a multi-view lookup and full
dirty-journal decision tree around every ordinary access. Importing that
ambient-address-space assumption is unsafe because a Rust projection is a
bounded host capability: applying its delta to an unchecked guest address can
reach unrelated host memory.

## Next generic optimization: trace range certificates

The next bounded experiment should hoist repeated access guards, not add
another opcode family. During trace formation, recognize a group only when all
memory operations:

- use one base register with statically known immediate offsets;
- do not use writeback, pair, vector, exclusive, atomic, or register offsets;
- cannot observe a write to that base between the certificate and the final
  access; and
- have checked minimum and maximum byte offsets representable without overflow.

Emit one entry certificate that computes the complete guest envelope, proves
lower bound, end nonoverflow, upper bound, and combined permissions against the
authenticated active descriptor, then applies the descriptor delta. Operations
inside the certified region may omit their repeated range/permission guards.
Every host access must retain its own provenance entry, so a legitimate host
fault still reports the exact guest PC, direction, and width.

For stores, the entry certificate may reserve enough journal capacity for the
maximum number of disjoint ranges, but it must not publish dirty state. Each
store must still commit its exact range only after the host store succeeds.
Contiguous post-store commits may use the existing extension path; unwritten
holes must never be published. Executable writes retain the ordinary epoch exit
and invalidation. Token authentication, descriptor generations, fork repair,
and mapping mutation remain entry prerequisites and cache identity components.

This is narrower than a guest-address-space aperture and does not weaken the
memory boundary. It is also the first candidate directly supported by the
measurements: it removes repeated emitted work whose cost increased when
fallbacks were eliminated, while leaving the rare resolver and publication
paths alone. Implementation requires a trace data-flow proof and adversarial
base-clobber, arithmetic-boundary, partial-store, fault, SMC, stale-token, and
fork tests; it should not be attempted as a local emitter shortcut.

### Rejected single-range runtime activation

The generated single-range activation was profiled on the unchanged AArch64
`combined-bench --phase memory --divisor 20` workload before being removed
from production. Temporary counters covered every translated trace and every
runtime guard without changing the workload. Of 130 translated traces, only
one satisfied the one-base/two-access planner, with two candidate members. At
runtime it entered once, missed authentication once, hit zero times, and
executed zero authenticated members. The same run executed 17,150,872 legacy
guards and committed 8,536,145 stores. The mixed read/write entry certificate
cost 155 emitted AArch64 instructions. It therefore could not have caused the
previous alternating-run timing improvement; that result was host-load noise.

The uninstrumented activated candidate measured 2.631 s guest and 2.678 s wall
for five repetitions, versus the prior pinned 2.582 s guest result. Because it
had zero runtime hits in the measured phase, neither difference is evidence of
a performance change. Production again emits no certificate entry and marks no
guard member authenticated. The bounded planner, pure authentication contract,
CPU scratch fields, and dormant guard seam remain available for a future design
with demonstrated coverage. A clean three-repeat post-removal run measured
2.537 s guest and 2.592 s wall with the same 36,526 checksum and unchanged
native counters, further demonstrating the host-load spread rather than a
certificate effect.

Dynamic guarded-access counts identify the actual hot domain:

| Form | Runtime guards | Share |
|---|---:|---:|
| non-writeback vector pair loads | 8,467,198 | 49.37% |
| non-writeback vector pair stores | 8,682,559 | 50.63% |
| scalar pairs | 393 | <0.01% |
| vector singles | 261 | <0.01% |
| scalar unsigned immediate | 250 | <0.01% |
| scalar register-offset | 211 | <0.01% |

Vector pairs account for 17,149,757 of 17,150,872 guards, or 99.9935%. A
temporary safe extension admitting non-writeback vector pairs improved static
coverage but produced only six certificate entries, four hits, two misses, and
eight authenticated members over the entire phase. It was discarded. The hot
libc vector-copy trace contains ten vector-pair operations spread across bases
`x1`, `x3`, `x4`, and `x5`, plus vector singles. A one-base certificate cannot
represent it. Four independent full view selectors would add roughly four
times the current entry cost and are not justified without a stronger trace
proof and a cheaper selector.

The retained C comparison was repeated against
`../engine/src/translator/guest/aarch64/translate.c` functions
`emit_a64_soft_guard_begin`, `emit_a64_soft_guard_end`, `emit_fold_mem`, and
`a64_fold_mem_offset`, plus chained-body ownership in
`../engine/src/translator/cache.c`. C folds vector pairs and singles directly,
uses a mapping-mutation-invalidated per-page software TLB, shares the cold miss
resolver, and lets direct chains enter with live registers. It does not require
multiple accesses to share one base before avoiding a complete per-access
multi-view lookup.

The next structural experiment should therefore target the generic projected
page/view lookup itself: a generation-qualified, authority-bound last-view or
software-TLB certificate with a short flag-free hot check and one shared cold
resolver. It must cover pair-vector accesses regardless of base register while
retaining exact per-access provenance, pre-store capacity reservation,
post-store dirty publication, executable-write exits, mutation/fork retirement,
and chain-entry authentication. Multiple range certificates or broader
register-offset admission should follow only if runtime counters show that the
page/view mechanism still leaves material guard cost.
