# x86 writable-view fallback profile

## Scope and retained oracle

This report profiles the alternating writable-view path at `ca6b873ac`, which
contains `cc4845ca8` (`native: cache x86 writable projections`). The retained
C oracle was inspected read-only at
`/Users/x/dd/engine` (`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`). The complete
corresponding domain and entry points are recorded in
`X86_WRITE_CACHE_AUDIT.md`; this follow-up mechanically checked the hot-path
ownership and fallback behavior against:

- `src/translator/guest/x86_64/emit.c`: `emit_memory_guard`,
  `emit_soft_guard`, `emit_soft_store_observe`, and
  `emit_soft_store_commit`;
- `src/translator/guest/x86_64/translate.c`: `rm_load_access`, `rm_store`,
  `rm_store_after_guard`, and the scalar, SIMD, x87, atomic, and string
  callers;
- `src/translator/guest/x86_64/rep_runtime.c`: scalar and pinned `MOVS` and
  `STOS`, including partial completion;
- `src/translator/guest_memory.c`: data resolution and pin lifetime;
- `src/linux_abi/logical_vma.c`: `hl_logical_vma_resolve`,
  `hl_logical_vma_resolve_data`, `hl_logical_vma_pin_data`, and
  `hl_logical_vma_unpin`; and
- `src/core/target/x86_64.c`: direct access admission and executable-alias
  observation.

The retained engine's immutable logical-VMA snapshot provides lock-free
binary-search resolution. A pin takes a backing reference under the ledger
mutex and releases the mutex before accessing bytes. The ordinary direct
mapping is an identity access; an indirect mapping resolves or pins before
mutation. A store is observed only after success, and REP preserves partial
progress. Mapping publication owns generation change and retired-snapshot
lifetime. The AArch64-host x86 lowering is the optimized implementation; other
hosts use the interpreter path. There is no fixed per-store dirty journal in
the retained hot path.

Husklet's corresponding owners are the Rust `ProjectionLease`, x86
`view_publish`, `frontend/memory.c::emit_write_cache`, the scalar/vector/RMW
emitters, `projection.c::flush_dirty`, and `NativeX86::writes`. Mapping
identity, backing lifetime, permission admission, pre-mutation capacity
failure, post-success publication, executable invalidation, and REP partial
completion are implemented. The material divergence is the bounded exact
dirty journal described below.

## Finding

The cached writable-view selector removes resolver callbacks but does not
remove dispatcher exits for a sustained alternation. Each transition away
from an active dirty owner appends four words to `dirty_records`:

```text
view_first, view_last, dirty_first, dirty_last
```

`HL_X86_DIRTY_CAPACITY` is 16. `emit_write_cache` tests `dirty_count` before
changing the active owner and deliberately falls through to the ordinary miss
path when it is full. The dispatcher can resolve the already-cached view, but
`projection.c::flush_dirty` then sets `dirty_overflow`; the run must return to
Rust so `NativeX86::writes` can conservatively publish and begin a fresh lease.
Thus a two-view loop can execute entirely from published views while still
crossing translated code, C dispatcher, Rust lease publication, and re-entry
roughly once per 16 owner transitions.

This directly explains a count on the order of 60 million fallback boundaries:
the count is journal saturation, not 60 million missing projections. The
diagnostic `boundary_fallback` counts both internally serviced projection
misses and final unsupported-instruction exits. `operand_callbacks` and
`operand_cache_hits` must be reported alongside it; the current two-view path
should have no callback after both views are published.

The focused regression in `test/x86_continue.c`,
`alternating_writable_views_stay_native`, runs only two loop iterations. It
expects three archived records, including a duplicate record for the first
view and exact same range. It therefore proves correct owner attribution but
cannot reach the 16-record saturation threshold. `test/x86_projection.c`
explicitly alternates until it expects `dirty_count == 16` and
`dirty_overflow == 1`, cementing the current expensive behavior rather than
testing sustained native progress.

## Required generic correction

Do not merely enlarge the array: it divides the exit count by a constant and
increases every CPU state. The coherent mechanism is an exact interval journal
that coalesces a completed interval with an existing record only when both
have the same projection owner and overlap or are adjacent. This preserves the
`publish_written_ranges` contract: every byte in the union is proven written,
while repeated writes to the same address consume no new capacity. Disjoint
ranges remain separate, and a genuinely full journal still fails before the
guest mutation.

The correction must be shared by emitted `emit_write_cache` archival,
`projection.c::flush_dirty`, and the REP capacity preflight. Tests must cover:

1. more than 16 repeated alternations between two exact ranges without an
   internal fallback or overflow;
2. overlapping and adjacent same-owner intervals coalescing exactly;
3. disjoint same-owner intervals remaining distinct;
4. identical guest ranges under distinct projection owners remaining
   distinct; and
5. 17 genuinely disjoint intervals preserving pre-mutation capacity failure.

The follow-up implementation applies identical overlap/adjacency coalescing to
the emitted scalar/vector/RMW transition and dispatcher
`projection.c::flush_dirty`. The sustained translated test now performs 64
two-view iterations (128 stores and 127 owner transitions), remains native,
and retains two archived exact records without overflow. The projection test
also preserves a full-journal case whose different owner cannot coalesce and
therefore still fails closed.

The REP bulk preflight remains a separate gap. `rep_dirty_full` sees only the
full count and prospective owner, not whether the current interval can merge
with a record. It can therefore request an epoch earlier than necessary. That
path was not weakened here: REP performs large bulk operations, so its exit
rate is not the scalar alternating-store amplification measured by this lane.
Giving it identical coalescing requires factoring one bounded journal
reservation operation shared by preflight and post-success publication; doing
only a post-write merge would violate the pre-mutation capacity contract.

## Exact integrated-tree measurement

Root integrated this stack as `a8a49f1cb2fb6e279a68b01ccf7d5896885ac185`.
The release engine and runner were built from that clean detached tree into
`/Users/x/dd/husklet-targets/x86-coalesce-a8a49f1`, outside `/tmp`. A single
CPU-17 diagnostic proof reported `native-verified`, checksum `7190`, 1,489
runs, 118 builds, 66,810 hits, four final fallbacks, 73,604 completed native
instructions, 1,236 operand callbacks, and 29 operand-cache hits.

The diagnostics-off comparison then used the same clean-repro x86 guest,
retained C runner, divisor 100, and memory phase as the `ca6b873ac` baseline.
Seven cycles rotated QEMU, C, and Rust order on exclusive CPU 17. Every one of
the 21 rows returned checksum `7190`:

| provider | samples (microseconds, sorted) | median |
|---|---|---:|
| retained C | 1580, 1700, 1811, 1831, 1874, 1886, 1902 | 1831 |
| Rust native | 23833, 24033, 24084, 24610, 25374, 25534, 25644 | 24610 |
| QEMU | 5960, 6451, 6512, 6609, 6637, 6704, 7160 | 6609 |

Rust is 13.441 times retained C in this run, down from the exact `ca6b873ac`
ratio of 15.430 times: the relative gap ratio fell 12.89%. The Rust median
itself fell from 25,892 to 24,610 microseconds, a 4.95% improvement. This
memory phase is REP-heavy, so the measurable improvement is intentionally
bounded by the still-conservative REP preflight described above.

Content identities were:

- Rust engine: `44b2bb9f7b537bd16753a53d2189b3e4f536da776d4436d7dc7f592eb4b4e045`;
- testing runner: `9e770dfff59994088dc3b4161cbb3b2dd0fc677c4bb85913b4897c6aa1c9bde8`;
- guest: `bda1b267655938e7be77cd2ec0450c7095650437e4a5e7be10db81da3a973b9d`;
  and
- retained C runner: `0633ed0f914f666f2127ca1b86a4def69eea65c59f7942f8afd66d2b7a6ebc62`.

Raw evidence is retained in the durable target's `evidence/diagnostics.csv`
and `evidence/timing-direct-quiet-3/` directory.
