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
## Slot-zero exact A/B measurement

The exact comparison used parent `3a982426e1a9c4c75319844b903703ac8250af1c`,
candidate `817818b5a71cb3c1faf008dc3176f50afb87849a`, and retained C
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. Each Rust tree was built alone
with `CARGO_BUILD_JOBS=18 cargo build --locked --release -p testing --bin
testing`. The resulting `testing` SHA-256 values were respectively
`9ab6d567e4886c8d414cec4f9f7b1bbe83f3f8f5adec8d823992fbb2d3062334` and
`f28f37528bd21269916b95fd36791c429b62107c1b6c1e3258920061f0ceec65`.
The combined guest source hash was
`ec97f6f5c598f6fc229231dbf4751fb298ebaf1ae04c530d8aecbc7a1ec926af` in
both repositories. The retained C engine and guest hashes were
`9c2701b36a46050909b12498eb0b47673f301bcb35d57e552d343b029cf3a67a`
and `07756eb451ec3c063a6ffed129db76b8702d5a37be8ba9cbf79eb77944d052ee`.

The Linux AArch64 host had 18 logical CPUs. Runs were pinned to CPU 17 with
the `performance` governor; CPU 0 reported 2 GHz. At start, load was
2.86/4.80/5.45, available RAM was 15 GiB, swap was 3.7/28 GiB used, `/Users`
had 108 GiB free, and `/tmp` had 12 GiB free. During the run `/tmp` remained
above 7.3 GiB; final load was 1.15/3.73/4.94 and no zombies were observed.

Each row used one warmup followed by one measured repetition of the memory
phase at divisor 20, alternating parent then candidate, with:

```text
taskset -c 17 env \
  HL_COMPAT_ENGINE_OPTIONS='HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1' \
  target/release/testing bench combined --isa arm64 --jobs 1 --results <path>
```

The temporary one-repetition manifest hash was
`ac605af602c70db30b844507e5f51609bf6b174d295bbe71bbef14520219adcb`;
the checked-in manifest and golden files were restored after measurement.
Both binaries reported `Execution { native: true, diagnostics: true }`, output
checksum `36526`, and the same nonzero diagnostics on every measured row:

```text
hl-native: runs=900 builds=229 hits=4898 fallbacks=16 sites=7 services=22
hl-native-detail: fills=16 site_collisions=0 shared_collisions=0 branch=140 syscall=0 fallback=3626 yield=884 completed=58122035 operand_callbacks=2780 operand_cache_hits=833 x86_public_exits=0 x86_public_syscalls=0 x86_syscall_vector_dirty=0
```

| Pair | Parent guest/wall | Candidate guest/wall |
|---:|---:|---:|
| 1 | 174287 us / 205 ms | 225443 us / 262 ms |
| 2 | 203011 us / 235 ms | 177993 us / 208 ms |
| 3 | 179819 us / 210 ms | 230632 us / 266 ms |
| 4 | 178573 us / 208 ms | 179696 us / 208 ms |
| 5 | 183240 us / 214 ms | 239317 us / 276 ms |

Parent guest time was min/median/max 174287/179819/203011 us with 6.10% CV;
candidate was 177993/225443/239317 us with 13.97% CV. Parent wall time was
205/210/235 ms with 5.58% CV; candidate was 208/262/276 ms with 13.63% CV.
Paired guest deltas were +51156, -25018, +50813, +1123, and +56077 us.

The pinned retained C engine was run directly on CPU 17 after one warmup:

```text
hl-engine-linux-aarch64 combined-bench-aarch64 --divisor 20 --phase memory
```

Its five guest samples were 6734, 6889, 6678, 6647, and 6805 us, all with
checksum `36526`; min/median/max were 6647/6734/6889 us and CV was 1.45%.

No performance claim is accepted. The Rust ranges overlap, one candidate pair
was faster, another was effectively equal, and candidate variance was high.
The median direction suggests a possible regression, not a demonstrated
improvement. The candidate was reverted; a future implementation requires a
lower-noise measurement that demonstrates benefit before acceptance.
