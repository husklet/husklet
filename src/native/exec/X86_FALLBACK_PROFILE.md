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

No production edit is included here because implementing only the emitted
selector would create different journal semantics between scalar/vector/RMW
stores and REP/dispatcher paths. The complete family-wide change requires a
single audited owner and a clean diagnostics-off C/Rust/native A/B run.
