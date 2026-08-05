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

## Rejected authenticated slot-zero fast path

A second, independent experiment started from parent
`66321702ad2278f1e13c2344e0f19ff4c8c7a398` and produced candidate
`f7b3d3bd32b3846733047943abbbcf0a67a493b0`. The retained C oracle remained
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. Typed instrumentation preceding
the experiment observed 43,460,082 selector decisions in three memory-phase
invocations: slot zero served 40,498,839 (93.186292%), the other three slots
served 2,954,352 (6.797852%), and only 6,891 (0.015856%) were cold misses.

The candidate appended an authenticated slot-zero projection to the CPU ABI.
The executor published it from the entry-authenticated read token and
incarnation only for a bounded, valid view-zero shape. Generated accesses
still checked overflow, lower and upper bounds, and read permission before
consuming the projected delta. Every rejected guard fell through to the
existing four-view selector. This shortened the common guard from about 32 to
20 executed words, but added 40 static words overall: the generated corpus
grew from 923 to 963 words, and both pair-read and scalar-read bodies grew by
20 words.

The warning-strict `aarch64_trace`, `aarch64_single`, `aarch64_cycle`, CPU
layout tests, and `git diff --check` passed on the candidate. The focused Rust
executor suite reported 60 passed, two ignored, and one pre-existing x86
strsearch differential failure (flags 5 versus 2053); no AArch64 candidate
test failed.

The exact A/B/C comparison used the same temporary memory-phase manifest in
all three trees: divisor 20, five measured repetitions, and checksum `36526`.
The harness also performed its normal cold invocation and one warmup. Runs
were sequential parent, candidate, then null control, without explicit CPU
affinity, using a development-profile `cargo run`:

```text
CARGO_BUILD_JOBS="$(nproc)" \
  CARGO_TARGET_DIR=/tmp/husklet-a64-slot0-target \
  cargo run -q -p testing --bin testing -- bench combined --isa arm64
```

The null control used the candidate's source and emitted fast-path code but
forced `slot0_valid` to zero. Every invocation reported identical native
diagnostics:

```text
hl-native: runs=900 builds=229 hits=4898 fallbacks=16 sites=7 services=22
hl-native-detail: fills=16 site_collisions=0 shared_collisions=0 branch=140 syscall=0 fallback=3626 yield=884 completed=58122035 operand_callbacks=2780 operand_cache_hits=833
```

| Tree | Wall cold | Wall min/median/p90/max | Guest min/median/p90/max |
|---|---:|---:|---:|
| parent | 1837 ms | 1796/1800/1803/1803 ms | 1575846/1577503/1581363/1581363 us |
| candidate | 1813 ms | 1789/1808/1892/1892 ms | 1566947/1583490/1658488/1658488 us |
| null control | 1821 ms | 1816/1852/1906/1906 ms | 1593583/1621808/1678209/1678209 us |

The candidate recovered most of the overhead exposed by disabling its
projection, but it did not beat the parent: guest median regressed by 0.38%
and wall median by 0.44%, while variance and static code size increased. The
retained-C result recorded in `PERFORMANCE.md` for the named memory/divisor-20
workload is 6839 us median, making these development-profile Rust medians about
230.66x and 231.54x retained C respectively; the gap did not improve. The
candidate was therefore rejected and its production branch and build artifacts
were deleted.

These approximately 1.58-second guest results must not be compared blindly
with the earlier 179819-us parent result in the preceding section. The earlier
experiment used release binaries, five alternating parent/candidate pairs,
one measured repetition per invocation, and CPU-17 affinity under the
performance governor. This experiment used development-profile binaries, five
samples within one invocation, sequential A/B/C ordering, and no explicit
affinity. Only the direction within each internally consistent experiment is
evidence; their absolute times are not interchangeable.

## Accepted targeted acquire

The exact parent for this experiment was
`d549362dc` and the retained read-only C oracle was
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. The complete retained paths
reviewed again were `src/translator/guest_memory.c`, including
`hl_guest_memory_bind`, `hl_guest_memory_resolve_exec_span`, and the data-pin
entry points, and `src/linux_abi/thread.c`, including the GNA/GRO writer
locks, generation readers, `gna_hit`, `gna_prefix`, `gro_hit`,
`hl_linux_bus_fault`, file-map publication, and exec teardown. Their common
direct translated-access path does not execute a global read barrier for each
operand; generation-qualified readers acquire only the state whose
publication they consume.

In Husklet, `MemoryLedger` owns logical mappings and their generation.
`Coordinator::project_contiguous` retains checkpoint admission, the mapping
transaction lock, and projected host storage for the complete native call.
`ProjectionLease` owns additional views and either publishes successful dirty
ranges or rolls back reservations. In the native owner,
`src/executor.c::run_view_publish` writes every bounded view, count, and
incarnation before release-storing `cpu->read_token`. The CPU record has one
execution owner; fallback promotion republishes the payload before native
re-entry. Mapping mutation, fork repair, and teardown cannot retire the
storage through the retained projection lease.

Previously `read_cache` and `write_cache` loaded `read_token` normally and
then executed `DMB ISHLD`. The accepted sequence instead forms the token
address and uses `LDAR` on that exact release-published object. This preserves
the C11/AArch64 release/acquire edge that orders the immutable payload while
avoiding a global load barrier on every guarded access. Range, overflow,
permission, incarnation, fault, write-owner, dirty-publication, and
executable-write checks are unchanged. The change emits AArch64 code only;
x86-64 and host-specific mapping adapters are unchanged.

Both trees were warning-strict release builds. The candidate's focused native
suite passed 91 tests with no failures and two ignored tests. The guest source
hash was
`ec97f6f5c598f6fc229231dbf4751fb298ebaf1ae04c530d8aecbc7a1ec926af`;
the compiled guest hash was
`a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9`.
Baseline and candidate engine hashes were respectively
`5af07d5edc3e22f6821dae70232a843d096046db459992a95edbdb5d594b97ec`
and
`1f2c8dc19568f518ffd62ae1fe57a57cb4c0ca9d29efc099e2546d23807e615e`.
One candidate runner drove both engines, eliminating runner differences.

Each row was a separate release-engine invocation pinned to CPU 17. Order
alternated by pair, and engine options were passed through the typed command
surface:

```text
taskset -c 17 testing benchmark run \
  --provider rust-engine --arch arm64 --binary combined_arm64 \
  --engine <baseline-or-candidate> --repeats 1 \
  --engine-option HL_NATIVE_EXECUTION=1 \
  --engine-option HL_NATIVE_DIAGNOSTICS=1 -- \
  --divisor <value> --phase <phase>
```

The required float-SIMD workload was neutral. Its seven baseline samples were
13,355,503, 13,703,441, 13,587,209, 13,637,522, 13,628,082, 13,665,672,
and 13,651,953 microseconds. Candidate samples were 13,656,664, 13,604,059,
13,620,877, 13,576,944, 13,639,683, 13,717,050, and 13,695,242
microseconds. Medians were 13,637,522 and 13,639,683 microseconds: a 0.016%
candidate difference within noise, with two of seven pair wins. Every row had
checksum `115136405225`.

The read-dominant memory control demonstrated the affected path. Baseline
samples were 31,927, 32,004, 31,893, 33,266, 31,582, 31,820, and 31,883
microseconds. Candidate samples were 31,492, 31,256, 31,532, 32,429, 31,264,
31,166, and 31,136 microseconds. Candidate won all seven pairs; medians moved
from 31,893 to 31,264 microseconds, a 1.972% improvement. Every row had
checksum `7190` and identical causal diagnostics: 5,962,557 full guards, 819
guard fallbacks, 645 operand callbacks, 174 operand-cache hits, 11,968,016
completed instructions, and matching run, build, hit, dirty, relocation, and
IBTC counters.

At final capture the host had 14 GiB available RAM, 17 GiB free swap, 94 GiB
free on the repository filesystem, 7.1 GiB free in `/tmp`, and load average
0.48/3.27/6.02. The targeted acquire is accepted: it materially improves the
read-heavy control without changing observable results, diagnostics, emitted
instruction count, or the required float-SIMD workload.
