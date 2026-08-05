# AArch64 certificate integration audit

## Scope and exact trees

This read-only integration audit used baseline `cf15cdd33`, retained-engine
oracle `/Users/x/dd/engine`, and these candidate tips:

| Candidate | Tip | Merge base with baseline | Shape |
| --- | --- | --- | --- |
| `agent/a64-projection-cert` | `b0ee7bd54` | `a0ff93cd4` | documentation only; its code change was reverted |
| `agent/a64-ingress` | `43c1eb2d3` | `ad5dc3b42` | bounds/permission/incarnation/authority checks on each guard |
| `agent/a64-ingress-cert` | `a21de7906` | `ad5dc3b42` | ingress authentication, read-only member fast path |
| `agent/arm-dirty-coalesce` | `ad2a377c0` | `7ac681960` | dirty-owner journal coalescing |
| dirty-path re-audit | `2593629d8` | n/a | documentation; audits baseline `cf15cdd33` |

The candidates do not form a cherry-pick series. `git merge-tree` reports
content conflicts for both certificate tips together, and for either
certificate tip combined with dirty coalescing. The conflicts include
`guard.c`, `executor.c`, CPU layout/initialization, and AArch64 trace tests;
they are semantic conflicts rather than mechanical context drift.

## Retained C/assembly oracle

The retained tree was never modified. The complete relevant ownership path was
followed through:

- `src/translator/guest/aarch64/translate.c`:
  `emit_a64_soft_guard_begin`, `emit_a64_soft_guard_end`,
  `aarch64_soft_tlb_miss`, `aarch64_soft_tlb_span`,
  `aarch64_soft_prepare_bounce`, and `aarch64_soft_bounce_commit`;
- `src/translator/guest/aarch64/dispatch.h`: the `R_SOFTMISS`,
  `R_SOFTSPAN`, and `R_SOFTCOMMIT` transitions;
- `src/translator/guest/aarch64/cpu.h`: task-owned interval, delta,
  protection, bounce, and pending-write fields;
- `src/translator/cache.c`: `map_invalidate_source_ranges`,
  `stw_before_translated`, `stw_after_translated`, `stw_mapping_begin`, and
  `stw_mapping_end`.

The retained task owns one soft-TLB tuple for its lifetime. A hit checks the
complete half-open interval and permissions before adding the host delta.
Misses spill exact PC/address/width/access and return through the dispatcher.
Discontinuous cross-span writes use bounded bounce storage, block signals while
copying, restore the signal mask, and publish only after successful commit.
Mapping mutation holds the mapping/JIT lifecycle gates, parks translated peers,
invalidates source ranges, and clears/refreshes cached ownership before backing
can retire. POSIX stop-the-world signalling and macOS host-range probing are
host-specific; emitted AArch64 interval hits are host-neutral. Faults and
permission failures never publish a successful write.

## Rust mapping and integration decision

| Required property | Baseline owner | Candidate result | Decision |
| --- | --- | --- | --- |
| Half-open bounds and overflow rejection | generated `guard.c` selectors | both certificate tips add checked bounds | conceptually required |
| Read/write permission | generated `guard.c` | ingress checks both; ingress-cert deliberately admits reads only | prefer read-only first |
| Mapping incarnation | `run_aarch64` active view | both carry it | required |
| Run authority | direct token / mapping epoch and execution admission | both carry it | required but not sufficient alone |
| Lease generation | Rust `ProjectionLease` lifetime, not CPU layout | neither candidate carries an independently comparable generation | unresolved |
| Write-owner transition | `memory_*`, dirty journal, projection reconciliation | ingress clears on cache owner switch; ingress-cert excludes writes | do not merge with coalescing mechanically |
| Fork/mutation/teardown retirement | execution gate plus lease | tested indirectly, no certificate-specific generation/retirement proof | unresolved |
| Direct-chain/IBTC ingress | trace entry layout | ingress-cert adds authentication at the shared body entry | promising, needs exact entry-path proof |

`agent/a64-projection-cert` is coherent and cherry-pickable only as historical
documentation: it explicitly reverted the attempted implementation. The two
code tips are alternative experiments. `agent/a64-ingress-cert` is the safer
base for future work because it constrains the optimization to reads and makes
trace ingress the authentication boundary. It must not be called merge-ready:
its CPU certificate contains bounds, permissions, incarnation, and authority,
but no independently authenticated lease generation. `agent/a64-ingress`
widens the optimization to writes and overlaps dirty-owner state transitions,
so it must follow, not precede, a proved read-only lifecycle design.

Recommended order:

1. land/rebase the documentation audits;
2. define a run-scoped, nonzero, rollover-safe lease generation at the Rust/C
   boundary and prove clear-on-fault, return, mutation, fork, and teardown;
3. rework the read-only ingress design on top of the current baseline and prove
   direct entry, direct chain, IBTC, permission change, incarnation change, and
   stale-generation rejection;
4. integrate dirty coalescing independently and re-run its full-journal and
   owner-switch cohort;
5. only then consider authenticated writes, with pre-store reservation and
   post-store publication evidence.

No production implementation was made in this lane. Adding a field without a
defined Rust lease-generation owner and rollover/retirement protocol would
manufacture an authority token rather than authenticate backing lifetime.
