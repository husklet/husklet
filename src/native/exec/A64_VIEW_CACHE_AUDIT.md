# AArch64 projected-view cache audit

This audit selects the next generic AArch64 performance mechanism without
changing production behavior.  The Husklet source revision is
`c80934c86a997ace195c660ef1c2fa6b8f4eb38a`; the retained read-only oracle is
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`.  The retained checkout has an
unrelated deleted packaging README and an untracked `.claude/` directory, so
its revision identifies source rather than claiming a clean checkout.

## Selection evidence

The exact benchmark source `tests/bench/combined/main.c` and retained
`../engine/tests/perf/combined_bench.c` both have SHA-256
`ec97f6f5c598f6fc229231dbf4751fb298ebaf1ae04c530d8aecbc7a1ec926af`.
The pinned measurements in `PERFORMANCE.md` show that the AArch64 memory phase
is hundreds of times slower than retained C.  Runtime counters narrow the hot
domain further:

- vector pair loads and stores execute 17,149,757 of 17,150,872 guards
  (99.9935%);
- 151,095 fallback boundaries contain 150,420 native operand-cache hits but
  only 666 resolver callbacks (0.44%); and
- admitting more vector operations while retaining the complete per-access
  guard made guest time 9.95% slower.

Consequently the next high-value mechanism is a generation-qualified,
authority-bound projected-view cache with a short flag-free hit path and one
shared cold resolver.  More opcode admission, more projection slots, and a
one-base range certificate are contradicted by the measured workload.

## Retained implementation audit

The complete retained path studied was:

- `../engine/src/translator/guest/aarch64/cpu.h`, `struct cpu`: owns one
  thread's `soft_page`, exclusive `soft_limit`, `soft_delta`, permissions,
  miss metadata, cross-page span state, and bounce buffer.  The fields live
  for the CPU/thread lifetime and are not process-global.
- `../engine/src/translator/guest/aarch64/translate.c`,
  `emit_a64_soft_guard_begin`, `emit_a64_soft_guard_end`,
  `emit_a64_soft_exit_site`, `a64_fold_mem_offset`, and `emit_fold_mem`: emit
  a flag-free interval and permission hit test, add the cached host delta, and
  join cold misses at a shared block exit.  Every access still owns exact PC,
  width, direction, scratch restoration, and fault metadata.
- The same file, `aarch64_soft_tlb_miss`, `aarch64_soft_tlb_span`,
  `aarch64_soft_prepare_bounce`, `aarch64_soft_bounce_commit`, and
  `aarch64_soft_span_copy`: resolve a miss, reject protection failures,
  preserve discontinuous cross-page semantics with a bounded bounce, block
  signals across a bounced store, publish SMC ranges after success, and
  restore the prior signal mask on commit.
- `../engine/src/translator/guest/aarch64/dispatch.h`, the `R_SOFTTLB`,
  `R_SOFTSPAN`, and `R_SOFTCOMMIT` dispatch cases: retry only after a valid
  resolution, route unresolved accesses to the architectural fault path, and
  complete bounced writes before another guest boundary.
- `../engine/src/translator/cache.c`, `stw_register`, `stw_unregister`,
  `stw_mapping_begin`, and `stw_mapping_end`: own registry identity, locking,
  and teardown.  Mapping mutation holds the JIT and thread-registry locks,
  stops translated peers, clears every registered CPU cache before retired
  snapshots or backings can be reclaimed, refreshes the conservative VMA hull
  after publication, then releases the gate.  Registration seeds the current
  generation; unregister removes the CPU pointer while holding the registry
  lock.

The hit path itself takes no lock and performs no host call.  Its lifetime is
safe only because mutation and teardown are serialized by the stop-the-world
registry.  Miss resolution observes the immutable logical-VMA snapshot.  A
cross-page partial store is never published as wholly written: discontinuous
storage is validated first, copied through the bounded bounce, then committed.
Protection failure remains a guest fault; allocation or registry failure does
not become a permissive hit.

The emitter is AArch64-specific.  Linux and macOS differ in the direct-host
range probe (`hl_host_range_mapped` is skipped on macOS), while the generated
hit sequence is host-OS neutral.  POSIX signal masking is part of the cold
discontinuous-store path.  Cache invalidation and refresh are enabled through
the guest ABI's `G_SOFT_TLB_CLEAR` and `G_SOFT_TLB_REFRESH` hooks.

## Husklet mapping and gaps

| Retained capability | Current Husklet owner | Status |
| --- | --- | --- |
| Per-thread cached interval, delta, permissions | generated `hl_native_aarch64_cpu` `memory_*` and `read_views` | divergent: four-view linear selector is emitted at every access |
| Flag-free hit test and delta add | `src/arch/aarch64/guard.c` `legacy_begin`, `read_cache`, `write_cache` | divergent: saves/restores NZCV and may scan four views |
| Cold bounded resolver | `src/arch/aarch64/projection.c` `hl_a64_projection_resolve` | implemented for a borrowed projection, but exits through Rust per miss and is not a shared in-block stub |
| Authority and generation authentication | `src/executor.c` direct token/admission plus generated `read_token`/`read_incarnation` | implemented for one synchronous projection lease |
| Exact post-store dirty publication | `guard.c` `hl_a64_guard_write_begin`/`hl_a64_guard_written` and `projection.c` `flush_dirty` | implemented; must remain per successful store |
| Exact per-access provenance | AArch64 frontend/trace provenance and fault reconstruction | implemented; cannot be coarsened to a page or trace |
| Mutation-time invalidation before backing reclamation | executor mutation admission and projection lease ownership | missing for a persistent per-thread last-view cache |
| Thread registry and fork/teardown repair | executor admission/fork code and fault thread registry | divergent: no registry currently binds cached projected backing lifetime to every native CPU |
| Discontinuous cross-page bounce and atomic commit | no equivalent projected-view cache path | missing |
| Shared cold miss stub | guard fallback plus Rust run loop | missing |

The current projection is a bounded borrowed capability.  Caching its host
delta beyond the authenticated run without a registry-mediated lease would
permit use-after-retirement of host backing.  Merely adding `last_first`,
`last_last`, and `last_delta` fields, or choosing `memory_*` before the existing
four-view scan, would therefore weaken the ownership contract even if it made
the benchmark faster.

## Required experiment

No production change is accepted by this audit.  A valid A/B candidate must
first add one coherent mechanism that:

1. binds the cached interval to executor identity, projection authority,
   mapping incarnation, and an explicit live lease;
2. invalidates every admitted CPU before mapping replacement, token retirement,
   fork-child repair, cache rollover that changes authority, or destroy can
   reclaim backing;
3. retains a flag-free fast hit for pair-vector accesses regardless of base
   register, exact per-access provenance, pre-store journal reservation,
   post-store exact-range publication, and executable-write epoch exit;
4. sends overflow, permission mismatch, adjacent discontinuous views, stale
   identity, and cold misses through one bounded resolver without converting a
   valid partial operation into an all-or-nothing write; and
5. proves mutation races, stale tokens, fork, cross-page faults, discontinuous
   stores, SMC, and teardown before timing the unchanged combined benchmark.

Instrumentation must be typed per executor and disabled by default.  At
minimum it must count hit, cold miss, stale-identity rejection, cross-span,
resolver retry, and access form without changing emitted control flow in the
uninstrumented A/B binaries.  The benchmark must consume and validate the
guest checksum, report native diagnostics, alternate baseline/candidate on a
pinned CPU, and use at least five samples after warmup.  Generated code must
execute through the real engine; a standalone emitter microbenchmark is
optimizer-invalid evidence for this mechanism.

## Attempted measurement and resource bound

The host was Linux AArch64 with 18 logical CPUs.  At audit start it had about
12 GiB available RAM, 20 GiB free swap, 111 GiB free on `/Users`, and 6.4 GiB
free in `/tmp`; load average was 3.83/5.21/6.82.  An exact-tree release run was
started with:

```text
CARGO_BUILD_JOBS=18 cargo run --locked -p testing --bin testing --release -- \
  bench combined --isa arm64 --jobs 1 \
  --results target/testing/a64-view-audit.tsv
```

It was cancelled during dependency compilation when concurrent work reduced
`/tmp` free space to 4.2 GiB, below the manager's 6 GiB guard.  No benchmark
sample ran and no timing claim is made.  Existing exact-tree measurements rank
the mechanism; a reproducible before/after result must wait for adequate
scratch space rather than contend with active guest builds.

## Slot-zero authenticated fast-path compaction

The follow-up audit used Husklet revision
`8ecaa7022c9bcf3596931adb3fe7f1820c648dcb`. Typed per-executor evidence
rejects checking `memory_*`/active-view identity before the run-view cache:
active fallback produced zero hits while run-view slot zero served 93.1863% of
authenticated accesses. An active-first candidate would add hot-path work and
was not implemented.

The safe reduction is narrower: defer `read_count <= 4` until slot zero misses.
The ownership chain was traced through
`hl-memory/src/mapping/projection.rs::project_contiguous`,
`ProjectionLease::project_additional`,
`hl-engine/src/native/executor.rs::run_aarch64_inner`, and
`native/exec/src/executor.c::{run_aarch64,run_view_install,run_view_publish,
run_view_resolve}`. A `ProjectionLease` holds mapping/checkpoint admission, the
mapping transaction lock, every host projection, and write reservations for the
whole synchronous `hl_native_run` call. C execution admission excludes mapping
reset, fork repair, and destroy while generated code runs.

`run_view_publish` establishes the exact release/acquire chain. It release-
stores zero to `read_token`, clears count, validates `count <= 4` and every
view's bounds, incarnation, permissions, host-span overflow, and delta; copies
the slots; stores count and incarnation; then release-stores the nonzero mapping
incarnation to `read_token`. `guard.c::read_cache` loads token, rejects zero,
executes `dmb ishld`, and requires equality with `read_incarnation` before
reading count or slots. A matching live token therefore proves slot zero and
count were validated. The upper bound remains before slots one through three.

No public API can construct matching live token around malformed slot data.
`run_aarch64` unconditionally clears token/count before request validation and
only `run_view_publish` republishes them. Projection views remain borrowed for
that call and Rust callers cannot access private native scratch inside
`run_aarch64_inner`. Direct C callers may supply an old CPU image, but the same
clear precedes execution. Resolver promotion passes through `run_view_install`
and the same publication function. Fork, replacement, authority rotation,
rejected fault publication, and return either cannot overlap the execution
lease or clear identity before the next entry. No host delta persists.

This moves exactly two emitted words—the count reload and `cmp count,#4`—off
the 93.1863% slot-zero path. Cold slots, resolution, faults, writes, dirty/SMC
publication, and cross-span behavior are unchanged. Tests execute a slot-zero
hit, prove forged count five cannot reach matching slot one, and pin the
deferred two-word location structurally. Instrumentation remains typed and
per-executor through `operand_cache_hits`/`operand_callbacks`; no global or
always-on hot-path counter was added.
