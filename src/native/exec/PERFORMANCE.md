# Native memory performance

## 2026-08-05 bounded AArch64 trace admission

The retained oracle audit covered
`../engine/src/translator/guest/aarch64/translate.c` (`translate` and its
direct-branch, indirect-branch, return, exception, and system decode paths),
`../engine/src/translator/guest/aarch64/stubs.c` (`emit_ibranch_ip2_ready` and
`emit_ibranch`), `../engine/src/translator/guest/aarch64/dispatch.h`
(`G_IBTC_FILL`), and `../engine/src/core/dispatch.c` (`run_guest`). The process
owns retained translation and cache state; the JIT lock protects build and
publication, generated readers are lock-free, dispatcher return follows the
architectural-state spill, and teardown removes published reachability. The C
engine implements the relevant instruction families rather than an equivalent
rejection-phase counter. Rust owns bounded build refusal in
`src/arch/aarch64/trace.c::hl_a64_trace_cache_direct` and attributes it outside
asynchronous fault handling in `src/executor.c::run_aarch64`.

Temporary word-only instrumentation on exact base `6f62a08ba` identified the
six small calls-row entry declines as `51000640`, `b90067f2`, `54000720`,
`6b01027f`, `a906a3eb`, and `510006b3`. These are already implemented integer,
memory, or conditional forms. The shared cause was speculative admission: a
32-word source window could exceed bounded code, provenance, or guard metadata
capacity and discard a valid prefix. The cache builder now retries progressively
shorter prefixes while retaining a one-word refusal boundary for genuinely
unsupported instructions. A 32-store regression covers provenance and guard
pressure and requires a nonempty bounded cached prefix.

On the same CPU-17-pinned calls row with typed native diagnostics and
`--divisor 60000`, the candidate reduced entry rejections from six to one,
removed the `other` family, increased completed native instructions from 197 to
55,131, and reduced the one-sample guest phase from 2,728 us to 1,106 us. The
remaining entry decline is a system form and belongs to system-instruction
admission rather than trace capacity. This is a bounded A/B diagnostic, not a
broad performance claim.

## 2026-08-04 projected-guard diagnostic benchmark

This measurement used commit `6e7d7b365acf96dc3297d64f88cbe84eef0e0ce1`,
whose tree was identical to the isolated verification commit `dae0f6645`. The
release `testing` executable had SHA-256
`09b60dc41a9bbe12d4cc87280d17331dc03f6b7b745ad977e27cd8530885961c`.
The combined benchmark inputs were:

| Input | SHA-256 |
|---|---|
| `tests/bench/combined/main.c` | `ec97f6f5c598f6fc229231dbf4751fb298ebaf1ae04c530d8aecbc7a1ec926af` |
| checked-in `test.yaml` | `7785ed330f229be8208cb244235795d57bfaa48ede6dfd14deb4a53dc8ee1312` |
| checked-in golden output | `5a792b8c5753e677ba5935c420473e4bbf3cb9d1b541e69ffa1ecd43dddb8ab3` |
| compiled guest artifact | `a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9` |

The runner was pinned to CPU 17 with `taskset`, used `HL_BENCH_JOBS=1`,
selected the AArch64 target and Alpine 3.20 image, and obtained native execution
and diagnostics through the typed manifest value
`Execution { native: true, diagnostics: true }`. Each row comprised one cold
invocation, one discarded warm-up, and five measured invocations. The TSV
format retains aggregate minimum, median, percentiles, and maximum rather than
the five individual wall-time samples.

The checked-in golden file contains the bytes `PHASE ` followed by a newline.
That sequence cannot occur in valid output such as `PHASE compute...`, so the
original full run stopped after its valid cold invocation. The recorded full
run temporarily used the same marker without its newline. That temporary
manifest had SHA-256
`4e36e4382a7d0fb42c6bb6cada84091643330f0beabd817d7424f4c50a1f3e79`,
the marker had SHA-256
`2da68fe0147872049a3dd60f4015b7fe1fb00bbe74750509e998ebc81416a181`,
and the resulting TSV had SHA-256
`a9aee8d12a9dbcee4f5d06d2a64f5db9f3988e1517d64e083f6010b79d39bdc0`.
The fixture was restored after measurement. A separate temporary memory-only
manifest had SHA-256
`caf0a97d01f92a8635959df55250fa857916762fd2f461d09e19138016da1b90`;
its TSV had SHA-256
`040c104d132bc2c23debef9b32deb821871bd5db4098382fd0e09853c9250d99`.

The full instrumented Rust row reported wall times of 110,175 ms cold, 111,183
ms minimum, 112,082 ms median, and 112,293 ms at p90, p99, and maximum. Phase
medians were:

| Phase | Median (us) | Phase | Median (us) |
|---|---:|---|---:|
| atomics | 1,636,349 | branch | 28,713 |
| calls | 3,955,585 | compute | 21,327 |
| compute cold | 6,775 | crypto | 58,388,347 |
| file | 157,897 | float SIMD | 13,850,480 |
| integer division | 50,755 | malloc | 9,575,034 |
| memory | 184,855 | mmap | 185,256 |
| pipe | 519,994 | signal | 2,899,250 |
| string | 8,796,632 | syscall | 234,584 |
| TLB | 417,312 | | |

Every one of the seven invocations, including cold and warm-up, produced the
same diagnostics: 139,490 runs, 6,144 builds, 154,787 block-cache hits, 1,149
fallbacks, 207 sites, 541,573 services, 352 fills, 5,105 branches, 135,001
syscalls, 4,670 typed fallback exits, 3,340 yields, and 220,489,167 completed
guest instructions. It recorded 2,841 operand callbacks and 891 operand-cache
hits. Of 29,062,284 guard outcomes, none used the fast path, 29,057,714 used
the full path, and 4,570 fell back. Stores reserved 14,550,655 dirty entries,
overflowed 175 times, committed 14,550,655 entries, and merged 14,544,592 of
them. No slim exit occurred.

The memory-only row reported 264 ms cold, 186 ms minimum, 225 ms median, and
236 ms at p90, p99, and maximum. Its memory phase was 151,936 us minimum,
182,602 us median, and 185,539 us at p90, p99, and maximum. All seven
invocations again agreed: 900 runs, 229 builds, 4,898 block-cache hits, 16
fallbacks, 7 sites, 22 services, 16 fills, 140 branches, zero syscalls, 3,626
typed fallback exits, 884 yields, 58,122,035 completed guest instructions,
2,780 operand callbacks, and 833 operand-cache hits. It recorded zero fast
guards, 29,021,179 full guards, 3,626 guard fallbacks, 14,536,779 dirty
reservations and commits, 3 overflows, 14,534,044 merges, and zero slim exits.

The retained-C control used source at
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`; its benchmark source was
byte-identical to the Rust fixture. The available retained engine artifact
`build/unit-audit/linux-production/hl-engine-linux-aarch64` had SHA-256
`9c2701b36a46050909b12498eb0b47673f301bcb35d57e552d343b029cf3a67a`,
and `perf/combined-bench-aarch64` had SHA-256
`07756eb451ec3c063a6ffed129db76b8702d5a37be8ba9cbf79eb77944d052ee`.
Because Ninja was unavailable, their binding to that retained source revision
could not be rebuilt or verified. The C results are therefore a
non-authoritative, artifact-bound control, not exact-source evidence.

The C memory phase samples were 7,352, 6,535, 6,531, 6,711, and 6,544 us,
with a 6,544 us median; the Rust standalone memory-phase median was 27.90
times larger. C full wall samples were 0.389, 0.369, 0.374, 0.363, and 0.363
seconds, with a 0.369-second median; the Rust full median was 303.745 times
larger. Selected C phase medians and Rust/C ratios were memory 6,582 us and
28.085x, crypto 35,750 us and 1,633.24x, floating point 6,825 us and
2,029.37x, malloc 14,913 us and 642.06x, string 10,410 us and 845.02x, and
calls 6,762 us and 584.97x.

This experiment makes no optimization or speed claim. Its diagnostic emission
adds counter instructions directly to the hot guard and dirty-journal paths,
so the timings characterize the instrumented cost decomposition rather than a
production baseline. The evidence establishes that this instrumented revision
never selected its projected fast guard, executed about 29 million full guards,
and merged almost every committed dirty range; it does not by itself establish
the improvement obtainable from any proposed replacement.

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

## AArch64 monotone cycle-admission audit

The retained chain and budget domain was audited before changing generic cache
relocation. The read-only oracle was `../engine/src/core/dispatch.c` (dispatcher
lookup, translation, chaining, and service ordering),
`../engine/src/translator/guest/aarch64/stubs.c` (`emit_prologue`, full/GPR
spill, typed syscall exits), and
`../engine/src/translator/guest/aarch64/translate.c` (block-entry IRQ polling,
budget/checkpoint boundaries, direct and conditional chain emission). The local
implementation comparison covered `src/arch/aarch64/entry.S`, `stub.c`,
`trace.c`, `executor.c`, and `cache/relocation.c`.

| Capability | Retained behavior | Rust-owned native behavior |
|---|---|---|
| Host ABI | Host SP, callee-saved GPRs, and q8-q15 survive guest execution | Complete in `entry.S`; x18 remains platform-reserved |
| Guest spill | Typed exits publish GPR, vector, NZCV, SP, PC, and reason state | Complete; syscall-shaped x19/x20 loop test covers re-entry |
| Service ordering | SVC exits at its current PC; service completes before PC advances | Complete through the typed syscall boundary |
| Entry polling | IRQ/budget state is checked at bounded block entry | Complete in the trace entry guard |
| Dedicated self-loop | Proven finite loops may run under preflight bounds | Complete; conditional self-loop path retains exact budget/interrupt tests |
| Generic multi-block chains | Optimized edges must not hide every dispatcher boundary in a cycle | Previously unconstrained; generic resolved edges now use bounded cycle admission |
| Epoch/fork teardown | Mutation invalidates incoming links before retiring executable identity | Existing relocation invalidation and generation checks retained |

Admission walks resolved edges with a fixed 64-node, nonrecursive frontier.
It performs no heap allocation and uses at most 1 KiB of stack. Reaching the
source proves the candidate closes a cycle; exhausting the frontier
conservatively rejects the optimization. The original typed exit therefore
remains valid even for a graph wider than the proof bound. Pending
wildcard-epoch links are checked only after they bind to the live target entry,
and IBTC-site relocation uses the same admission rule. The dedicated self-loop
optimization is excluded because its independent preflight proves and charges
a bounded iteration count.

Admission adds no emitted instructions to admitted chains. Its structural
worst case scans the resolved-edge table for each of 64 frontier nodes and
performs a 64-entry duplicate check; saturation retains the dispatcher edge.
A direct two-entry cache
test proves the closing edge remains unpatched, while the execution test proves
exact budget exhaustion, interrupt visibility, syscall return, and callee-saved
state across a multi-block loop.

### Synthetic publication cost

`test/relocation_bench.c` publishes synthetic directed cycles into a fresh
64 MiB test cache. It measures publication only; generated code is never
executed. Each cohort was run five times against the pinned pre-change
relocation object and the bounded-frontier candidate. Times below are median
nanoseconds with the full observed minimum–maximum spread.

| Nodes | Pre-change | Bounded frontier | Pre patched/unpatched | Candidate patched/unpatched |
|---:|---:|---:|---:|---:|
| 2 | 28,500 (27,167–36,708) | 39,417 (35,666–46,833) | 2 / 0 | 1 / 1 |
| 16 | 242,625 (232,208–308,875) | 286,541 (273,833–293,791) | 16 / 0 | 15 / 1 |
| 64 | 456,750 (420,833–546,625) | 552,250 (517,791–2,543,833) | 64 / 0 | 63 / 1 |
| 65 | 395,000 (367,750–466,584) | 557,833 (544,625–655,708) | 65 / 0 | 64 / 1 |

The 65-node closing edge requires a 65th frontier identity; the fixed 64-node
frontier saturates and conservatively retains that typed edge. The patched
counts prove every candidate cohort retains exactly one dispatcher boundary,
whereas the prior implementation closes every cycle. The largest median delta
observed here is 162,833 ns per fresh 65-block publication cohort. This is
publication-path evidence only, from a dirty shared tree using otherwise pinned
native objects; it is not an execution-hot-path or end-to-end timing claim.

## Native cache probe audit at 75% occupancy

The retained lookup domain was audited in full in
`../engine/src/translator/cache.c`: `map_idx`, `map_host`, `map_body`,
`map_put`, source-range and cache-generation invalidation, epoch tombstones,
generation wrap, and retired W^X cache reclamation. The local owners are
`cache/private.h` (`hl_native_cache_find_identity`) and `cache/cache.c`
(`home`, `insertion`, lookup, reset, invalidation, and publication).

| Capability | Retained cache | Native cache |
|---|---|---|
| Hash/probe | Multiplicative hash and linear probing | Same algorithm, with ISA-selected shift |
| Lookup identity | Guest PC plus live generation | Guest PC, generation, memory mode, authority, and instruction epoch |
| Deletion | Epoch-tagged tombstone preserves probe chains | Entry tombstone preserves probe chains |
| Wholesale reset | O(1) generation advance; physical clear on wrap | Same generation lifecycle |
| Precise invalidation | Compact live-index scan over source ranges | Compact `live[]` scan over source ranges |
| Executable lifetime | W^X arena plus retained generations | Arena ownership and generation invalidation |

`test/cache_probe_bench.c` uses 4096 slots at 75% occupancy. Publishing 32-byte
blocks and arena setup occur before timing. Each repeat performs 4096 lookups;
probe counts are independently reconstructed from the private table's exact
generation, state, home, and termination rules. Five-repeat median nanoseconds
and full spread follow. “Pinned A/B” use native archives from the cycle lane;
their cache source is identical.

| Cohort | Probes / 4096 queries | Pinned A median (spread) | Pinned B median (spread) |
|---|---:|---:|---:|
| Distributed hit | 4,096 | 25,792 (25,541–27,458) | 97,625 (83,250–353,083) |
| Distributed absent miss | 32,617 | 69,292 (68,791–110,375) | 211,333 (201,084–264,417) |
| Same-home adversarial hit | 5,244,928 | 9,665,708 (9,197,583–11,187,750) | 37,830,416 (34,773,667–55,382,834) |
| Tombstone-crossing miss | 12,587,008 | 21,185,291 (20,527,042–22,403,959) | 70,154,333 (69,104,500–71,440,916) |

Probe counts are stable and authoritative: distributed hits take exactly one
probe, while same-home and tombstone cohorts scale with their deliberately long
linear chains. Timing differs materially between archives despite identical
cache code and has wide spread in pinned B, so it is build/host evidence rather
than evidence of a cache regression. The adversarial mechanism is dominant only
under deliberately colliding keys; no representative workload distribution was
measured. Therefore this audit does not justify a production hash-table change.

## x86-64 guest on AArch64 host paired dispatcher-spill audit

This architecture name describes the guest ISA, not the emitted host ISA.
`src/arch/x86_64/run.c` calls `hl_x86_a64_emit`, emits AArch64 instruction
words, and enters them through `src/arch/x86_64/entry.S` on an AArch64 host.
The matching retained implementation was audited in
`../engine/src/core/dispatch.c` (`run_block`, `block_return`, and `run_guest`),
`../engine/src/translator/guest/x86_64/emit.c` (`emit_prologue`,
`emit_spill_gpr`, `emit_spill`, and typed runtime exits), and
`../engine/src/translator/guest/x86_64/translate.c` (translation entry and the
x86-guest/AArch64-host trampoline and exit call graph). Both implementations
previously emitted sixteen scalar AArch64 `STR` instructions to publish the
sixteen contiguous x86-64 guest GPRs at a dispatcher return.

The Rust-owned emitter now publishes adjacent register pairs with eight
AArch64 `STP` instructions. Compile-time assertions require the CPU register
array to remain sixteen contiguous `uint64_t` values at a 16-byte-aligned
offset. All generated offsets are 0 through 120 bytes and therefore fit the
signed, scaled seven-bit `STP` immediate. The focused test independently
checks every emitted opcode, register pair, base register, offset, final
`RET`, and the exact post-execution value of every guest GPR.

This does not change state ordering. Budget accounting and reason selection
precede the spill, and the dispatcher and checkpoint code observe the CPU
record only after the translated block returns. While translated code is
active, asynchronous signal reconstruction is keyed by the interrupted host
PC and provenance; the CPU spill was already incrementally visible across
sixteen scalar stores. Pairing adjacent stores neither creates an earlier
checkpoint boundary nor weakens the signal reconstruction contract. Entry ABI
save/restore and teardown ownership are unchanged.

The fail-first test was linked to the previously built archive:

```sh
archive=$(find target/debug/build -path '*/out/libhl_native_execution.a' -print | tail -1)
cc -std=c11 -Wall -Wextra -Werror \
  -I src/native/exec/include -I src/native/exec/src -I src/native/cpu/include \
  src/native/exec/test/x86_budget.c "$archive" -lpthread -o /tmp/hl-x86-budget
/tmp/hl-x86-budget
```

It failed at the new structural assertion:

```text
x86_budget:69: tail[pair] == expected
```

The exact candidate sources were then compiled warning-strict and the focused
test passed with no output:

```sh
sources=$(find src/native/exec/src src/native/exec/cache -type f -name '*.c' -print)
cc -std=c11 -Wall -Wextra -Werror \
  -I src/native/exec/include -I src/native/exec/src -I src/native/cpu/include \
  $sources src/native/exec/src/arch/aarch64/entry.S \
  src/native/exec/src/arch/x86_64/entry.S \
  src/native/exec/test/x86_budget.c -lpthread -o /tmp/hl-x86-budget-new
/tmp/hl-x86-budget-new
```

The emitted dispatcher-return spill is structurally reduced from sixteen
memory instructions and 64 code bytes to eight memory instructions and 32
code bytes. This is code-size and memory-operation evidence only. No wall-time
speed claim is made without an isolated, pinned retained-C/pre-change/candidate
benchmark with identical guest counters and checksums.

### Dispatcher reason-profile prerequisite

The existing bounded diagnostics ABI exposes `boundary_branch`,
`boundary_syscall`, `boundary_fallback`, `boundary_yield`, and `completed`, but
it cannot yet support an authoritative x86 dispatcher-return profile. In
`src/arch/x86_64/run.c`, diagnostics sample
`cpu->executed - executed_before` immediately after native entry. Ordinary
translated blocks do not add their `cpu->scratch[0]` completion count to
`cpu->executed` until later, so the diagnostic `completed` value omits those
instructions. The reason switch also runs before the dispatcher converts
post-return interrupt, executable-write epoch, operand fault, and budget
conditions into their public exit kinds. `boundary_branch` consequently mixes
an actual dispatcher branch return with the last reason left by code that may
have followed a live chain, while interrupt and fault have no independent
buckets.

The retained dispatcher in `../engine/src/core/dispatch.c` likewise exposes no
pinned per-reason counter suitable for a like-for-like comparison. Therefore
no guest-instructions-per-return ratio or branch/syscall/fault/interrupt/chain
mix is reported here: deriving one from the current fields would be false
precision. The prerequisite is a bounded, diagnostics-only return record made
after completion charging and public-exit classification, with distinct
native-entry, live-chain, dispatcher-branch, syscall, fallback/fault, yield,
interrupt, and epoch counters. A retained-C comparison needs equivalent
temporary counters at `run_block` return and the reason switch. Counter work
must be measured separately because even conditional hot-path increments can
perturb dispatcher-heavy timing. Until that instrumentation exists and passes
five checksum-identical repetitions, the paired spill remains structural
evidence and no further `entry.S` optimization is justified.

The proposed budget/cache sweep was stopped at its chain-workload prerequisite.
A warning-strict direct build of the exact shared native sources succeeded, but
`x86_chain` failed its interrupt-at-entry contract at line 71: the program and
executed fields were unchanged while `scratch[0]` was nonzero. Because that
workload did not preserve its existing architectural assertion, its cache
hits, block counts, and timings are inadmissible; five timing repetitions were
not run. The paired GPR spill cannot produce that field change because
`finish_execution` writes `scratch[0]` before the spill and paired offsets are
compile-time bounded to the 0--120-byte register array. The focused
`x86_budget` state and encoding contract still passes. Chain-path measurement
must resume only after the failing baseline is assigned and repaired or proved
to be a stale test expectation.

That first failure was a stale test entry contract. Published `entry` owns the
BTI and initial budget load; published `body` is a live-chain target that
assumes the source block already holds the remaining budget in x26. The three
test-only calls from C supplied no live translated-register state, so they now
enter through `entry`. This matches the retained x86 guest translator, whose
`emit_prologue` establishes host state before its published body and whose
direct-chain emitter alone branches to a body with live registers. The focused
interrupt-at-entry assertions then pass. The exact full `x86_chain` workload
subsequently stops at line 76 because relocation invalidation does not restore
the first tail to `RET`; that separate generic cache/cycle prerequisite remains
outside this x86-only lane, so cache/timing sweeps remain inadmissible.

The complete follow-up proved those remaining failures were stale x86 test
assumptions as well. The paired spill makes the direct relocation site thirteen
words from the emitted end: one relocation word followed by three accounting
words, eight paired GPR stores, and `RET`. Test-local relocation helpers now
derive direct and conditional sites from that contract. Invalidation restores
the exact typed fallback branch recorded in `relocation.expected` (`b +1` for a
direct or taken edge and `b +2` for conditional fallthrough), not an invented
`RET`. Bounded graph cycle admission requires exactly one dispatcher fallback
in the two-block cycle; publication order may determine which edge it is.

The nested-call invalidation range `0x9010..0x9016` overlaps two translated
blocks, producing the observed live-block transition 8 to 6 and invalidation
count 3 to 5 without changing generation 2. Its IBTC slot owns target `0x9005`,
which is outside that range and remains live; only targets inside the range are
retired. This matches current `ibtc_invalidate` identity ownership. The retained
C engine uses target identity for its IBTC entries and explicitly retires a
matching target when replacing a translation; it does not justify clearing an
unrelated hashed slot target.

After correcting only these x86 test expectations, the full exact-source
warning-strict `x86_chain` binary passed with no output. No generic cache,
relocation, translation, or production entry behavior changed.

## Rejected active-interval-first guard candidate: native proof failure

On 2026-08-03 an uncommitted experiment reordered the AArch64 projected-memory
read guard so the current active interval preceded the bounded four-view
selector. The retained-C audit covered
`../engine/src/translator/guest/aarch64/translate.c` functions
`emit_a64_soft_guard_begin`, `emit_a64_soft_guard_end`, `emit_fold_mem`, and
`a64_fold_mem_offset`, plus translation-map and chained-body ownership in
`../engine/src/translator/cache.c`. The mechanism matched the retained soft-TLB
shape, but it did not pass the mandatory execution-mode proof and was reverted
before a memory measurement.

The fast proof used the persistent AArch64 `combined-bench` guest with
`--phase syscall --divisor 100000`, one repetition, logical CPUs 2--5, and the
Rust engine's typed `HL_NATIVE_EXECUTION=1` option. The benchmark driver added
`HL_NATIVE_DIAGNOSTICS=1`. The engine terminated with `SIGILL` and empty
diagnostic output, so no nonzero native boundary count existed. The same exact
candidate engine and guest completed in interpreter mode with `us=69`,
`ok=200`, and `wall_us=11260`. Because native mode was not proved, the
five-repeat candidate memory run was not started. There are therefore no
candidate guest/wall medians, active-selector counts, or speed claim.

The retained-C control came from clean detached commit
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. Five sequential repetitions of
`combined-bench --phase memory --divisor 20` on the same CPU affinity produced
guest median `6839 us`, minimum `5823 us`, maximum `7347 us`, wall median
`32796 us`, and checksum `36526` in every repetition. The 1524-us guest spread
is 22.3% of the median and is the observed colocated noise bound for this
control. The guest SHA-256 was
`07756eb451ec3c063a6ffed129db76b8702d5a37be8ba9cbf79eb77944d052ee`;
the retained-C engine was
`00109c1dfe94937284d6e17a012cf1138085e35ab82eed22228b21ea6c1c2269`;
the rejected candidate engine was
`b1e3a6d7640584a89ca24557fd4b7fcc1540f74687ff7cf8860c38384e37d99b`;
and the benchmark driver was
`0a028749717de24db065e8a66041c113db140e1a41792f2cf036281642635603`.

The host was Linux AArch64 under OrbStack, kernel
`7.0.11-orbstack-00360-gc9bc4d96ac70`, with 18 logical CPUs, rustc 1.93.1 and
LLVM 21.1.8; the C tree configured with GCC 15.2.0. Available memory was 21 GiB
before and 19 GiB after, swap use remained 228 MiB of 28 GiB, workspace free
disk changed from 262 to 261 GiB, and `/tmp` free space changed from 19 to
16 GiB because of the clean C build. One pre-existing zombie remained and no
benchmark descendants escaped. The Rust candidate was built from shared dirty
tree HEAD `86f023a243ac2d96203d858d90b0db9635fcabd1`; it was candidate evidence only,
never evidence for that commit. Failure of the native proof rejects the
experiment independently of timing.

Post-revert diagnosis isolated the failure to the experiment rather than the
committed native entry path. A clean detached build of exact HEAD
`86f023a243ac2d96203d858d90b0db9635fcabd1` passed the same typed fast row with
`us=49`, `ok=200`, `wall_us=25001`, seven native runs, thirteen builds,
34 hits, three fallbacks, three sites, and 126 services. Its engine SHA-256 was
`34fec4803d498f5c33f05056b355c89704362c1157216808e55b8f23da6f76ff`.

GDB placed the rejected candidate's `SIGILL` at an emitted `udf #0` immediately
after the active-hit branch and immediately before the native load. That word
was the experiment's new cache-miss branch placeholder. The experiment cleared
`hl_a64_guard.below` after locally patching the active-range branch, but
`trace.c` uses a non-null `below` field to enqueue a deferred guard for
`hl_a64_guard_finish`. The guard was consequently not enqueued and the new
placeholder remained zero instead of being patched to the cold fallback.
Execution reaching a view miss therefore executed the placeholder. The exact
owner was the `guard.c`/`trace.c` deferred-patch admission contract, not the
Linux syscall path, benchmark guest, retained-C engine, or native entry
assembly. Reverting the candidate removed the unpatched instruction; no
application-specific workaround was introduced.

### Corrected deferred-finalization experiment remained below noise

A follow-up made deferred-finalization admission explicit: a guard was pending
when either its legacy range patch or its active-first cache-miss patch existed.
The fail-first test initially did not compile because that contract did not
exist. After implementation, it proved that finalized active-hit and
active-miss code contained no reachable zero-word `UDF`, that a cache-miss
guard was enqueued, and that active and secondary-view reads preserved the
projection authority and fallback behavior. The typed fast native row passed
with the same diagnostics as clean HEAD.

The corrected change was then applied as the only source delta to the clean
detached HEAD tree and measured for five repetitions with the same guest,
affinity, and native diagnostics. Pre-change Rust measured guest median
`2530806 us`, minimum `2481081 us`, maximum `2756202 us`, and wall median
`2571675 us`. The isolated corrected candidate measured guest median
`2499036 us`, minimum `2462651 us`, maximum `2559865 us`, and wall median
`2544539 us`; its engine SHA-256 was
`79ef34c46d75ff3f4c55ce5fbc50746665bb97f6bbd8f8c967050a9c88e60c10`.

Every repetition on both sides retained checksum `36526` and identical native
counters: 536 runs, 113 builds, 1426 hits, 19 fallbacks, 16 sites, 25 services,
seven fills, 70 branch boundaries, 718 fallback-detail boundaries, 517 yields,
34,049,536 completed instructions, 654 operand callbacks, and 55 operand-cache
hits. The candidate median was 1.26% lower, but the pre-change guest spread was
275121 us (10.87% of its median) and the candidate spread was 97214 us (3.89%).
The difference is therefore below observed run-to-run noise and is not a
reproducible performance improvement. The experiment was rejected and reverted
again; no speed claim is made. A separate shared-dirty-tree run had radically
different chaining counters and was excluded from this comparison.

## AArch64 memory cost-class attribution without perf-event access

The pinned five-repeat memory comparison above was reused unchanged for
cost-class attribution: retained C guest median `6839 us` and wall median
`32796 us`; clean pre-change Rust guest median `2530806 us` and wall median
`2571675 us`; checksum `36526` in every run. Both ran the same
`combined-bench --phase memory --divisor 20` guest on logical CPUs 2--5. The
Rust side proved native mode and repeated identical counters in all five runs:
536 native runs, 113 builds, 1426 block-cache hits, 19 ordinary fallbacks,
34,049,536 completed guest instructions, 654 operand callbacks, and 55
operand-cache hits. The retained-C and Rust guest-time ratio was 370.055x and
the complete-process wall ratio was 78.414x.

Linux `perf` 7.0.12 could not open the requested hardware event group on this
OrbStack host. `perf stat` reported `No supported events found` and specifically
that `cycles:u` was unsupported; `perf_event_paranoid` was 2. Consequently no
cycles, retired host instructions, branches, branch misses, L1 data loads or
misses, or cache references/misses are reported. This evidence cannot honestly
separate host branch-misprediction cost from host cache-miss cost.

The existing bounded instrumentation still identifies emitted per-access work
as the dominant measured class. The unchanged runtime profile records
17,150,872 guarded accesses, or 0.503704 guards per completed guest instruction
(1.985295 guest instructions per guard). Only 654 resolver callbacks occurred:
0.003813% of guards. Operand-cache hits were 0.000321% of guards. Translation
was likewise sparse at 3.3187 builds and 41.8802 block-cache hits per million
completed guest instructions. Branch, fallback-detail, and yield boundaries
together were 38.3265 per million completed guest instructions. Those rare
translation, resolver, and dispatcher events cannot scale with the 370x guest
gap, while the complete emitted range/overflow/permission/view-selection and
dirty-journal sequences execute approximately once per two guest instructions.

The measured dominant class is therefore dynamic emitted guard/journal
instruction volume, not Rust resolver callbacks, translation publication, or
dispatcher crossings. This is an inference from exact runtime-frequency and
static emitted-path evidence, not a hardware-counter claim. A runner with
working PMU virtualization is still required to divide that instruction-volume
class into execution, branch-prediction, and cache effects. The next coherent
domain remains the generic projected page/view guard and exact post-store dirty
publication mechanism. Any next experiment must reduce its dynamic emitted
work across the vector-pair-heavy trace, preserve the deferred-finalization and
projection-authority contracts, and beat colocated noise with identical
checksums and counters; another local active-view reordering is not justified.

At attribution time the host had 12 GiB available memory, 580 MiB of 28 GiB
swap in use, 255 GiB free workspace disk, 9.7 GiB free `/tmp`, and one
pre-existing zombie. No performance process escaped.

## Generation-qualified last-view certificate audit

This lane compared the retained soft-TLB owner in
`../engine/src/translator/guest/aarch64/translate.c`
(`emit_a64_soft_guard_begin`, `emit_a64_soft_guard_end`, and
`emit_fold_mem`) with the Rust-owned AArch64 projection path in `guard.c`,
`pair.c`, `projection.c`, `trace.c`, and `executor.c`. The retained entry is
the per-CPU `soft_page/soft_limit/soft_delta/soft_protection` tuple. Mapping
mutation retires it under the process stop-the-world protocol; a miss exits
through one shared block resolver. Pair operations use one total-width guard
and one native pair instruction. The Rust owner instead publishes four
generation-qualified views with release/acquire ordering, selects an exact
active write owner, and commits bounded dirty intervals only after the native
store succeeds.

For a vector-pair read whose first published view hits, the current emitted
hot path executes one effective-address move, three flag-save instructions,
13 token/incarnation/count setup instructions, 16 first-view selection and
projection instructions, and the native pair load: 34 instructions before
any stolen-register restoration. Its static body also contains the other
three view selectors and the active-interval fallback. A vector-pair store
executes the effective-address move, the 19-instruction active interval guard,
the pre-store dirty-capacity decision, the native pair store, and the
post-success exact dirty merge/publication. The empty-journal case is about
63 executed instructions including the native store; disjoint or merging
ranges execute more. By contrast the retained C soft path uses its hull
bypass plus one cached interval/protection/delta tuple and has no equivalent
per-store projection journal. These counts explain why the 17,149,757 dynamic
vector-pair guards dominate the measured memory phase.

The existing dormant Rust certificate seam is not sufficient for a generic
last-view certificate. `certificate_valid` and `certificate_delta` carry no
guest-page/envelope identity, mapping incarnation, permission authority, or
fork generation. `run_aarch64` clears both fields at entry, production pair
emitters request `HL_A64_GUARD_LEGACY`, and no chain-entry authenticator
publishes the exact active owner. Treating the validity bit as persistent
would therefore allow an address from another page or a retired mapping to
reuse a host delta. No emitter change was made.

| Required capability | Current evidence | Decision |
|---|---|---|
| Guest page or exact interval key | Dormant seam stores only valid and delta | Blocked |
| Mapping-incarnation retirement | Four-view cache checks token/incarnation; seam does not retain either | Blocked |
| Authority authentication | Pure range checker accepts expected and active authority, but emitted seam does not retain it | Blocked |
| Permission and cross-end proof | Legacy guard proves both per access | Must remain until entry authentication owns an exact envelope |
| Fork and mutation invalidation | Run entry clears the seam; no authenticated chain-entry publication exists | Blocked |
| Fault provenance | Pair retains one exact PC/access/width site | Accepted and must remain |
| Partial/faulting store | Dirty publication follows the native store | Accepted and must not be hoisted |
| Exact successful dirty range | `hl_a64_guard_written` commits the total pair width after success | Accepted and must remain |
| Executable write | Post-store permissions set `executable_written` for epoch handling | Accepted and must remain |

A safe certificate must be an indivisible entry record keyed by guest page or
exact interval, mapping incarnation, authority identity/token, permissions,
and host delta. Trace and every direct-chain ingress must authenticate that
record before member execution; mapping mutation and fork repair must rotate
its generation before retired storage can be reclaimed. A member may then use
the authenticated delta without repeating view selection, but stores must
retain capacity reservation before the host instruction and exact dirty
publication after it. Until that ownership and chain-entry protocol exists,
the fail-first stale-token, rotated-authority, cross-page, faulting-store,
executable-write, mutation, and fork cases cannot be satisfied by a local
pair-emitter patch. This lane therefore stops at a source-backed blocker and
makes no performance claim.

### Rejected empty-journal sentinel arithmetic candidate

A separate bounded experiment targeted only the overwhelmingly common empty
dirty-journal decision in `hl_a64_guard_write_begin` and
`hl_a64_guard_written`. The retained C soft guard has no corresponding
per-store journal; Rust owns the stronger contract: capacity is checked before
the host store, while the exact guest range, selected view identity,
`memory_written`, and executable-write state are published only after the
native store succeeds. The candidate preserved that ordering and replaced
each four-word `UINT64_MAX` materialization plus comparison with one
`adds xzr,x17,#1`; zero is produced exactly when `dirty_first` is the empty
sentinel. It removed four emitted and executed instructions from reservation
and four from post-success publication without changing any branch target.

The fail-first pair test required both optimized sentinel tests in the emitted
vector-store body, then proved the first successful 32-byte vector-pair store
published exactly its guest range and active view owner. The focused pair test
passed after implementation, including fallback-before-store behavior for an
out-of-range and a permission-denied pair. The existing ordered and trace
cohorts also passed, covering full-journal epoch exit, exact dirty state, and
executable-write handling. The candidate's typed fast native proof passed with
seven runs, 13 builds, 34 hits, three fallbacks, 129 completed instructions,
and zero operand callbacks.

Authoritative timing used clean detached commit
`596e7fd27386361374b974c7d17707078a84fb33`, logical CPUs 2--5, the same
AArch64 combined guest, and five repetitions of memory divisor 20. Clean
baseline guest median was `2591959 us` (minimum `2524963`, maximum `3143826`)
and wall median `2658081 us`. The isolated two-site candidate guest median was
`2514874 us` (minimum `2463578`, maximum `2625048`) and wall median
`2565883 us`. Checksums were `36526` throughout and both sides had identical
native counters: 536 runs, 113 builds, 1426 hits, 19 fallbacks, seven fills,
70 branch boundaries, 718 fallback-detail boundaries, 517 yields, 34,049,536
completed instructions, 654 callbacks, and 55 operand-cache hits. Baseline
engine SHA-256 was
`415c21244bac0f785aeab097fef5fdedaa90f7e685645f291c0697e4d1825ca9`;
candidate engine SHA-256 was
`36713e16c42c9f3ef600595f50a5629c57658b5e0b429a04f8d5fe570e750738`.

The apparent 2.97% guest-median reduction is below the baseline's 24.0%
minimum-to-maximum spread and is not a reproducible improvement. The candidate
and its focused test were therefore rejected and fully reverted. This result
also shows that trimming eight instructions from the store path alone does not
overcome colocated timing noise; no speed claim or production change remains.
At measurement time 11 GiB memory and 242 GiB workspace disk were available;
`/tmp` had 1.9 GiB free, swap use was 11 GiB of 28 GiB, one pre-existing zombie
remained, and no benchmark descendant escaped.

## Transactional AArch64 static-density accounting

The retained-code audit used
`../engine/src/translator/guest/aarch64/translate.c::translate_block` for the
block/code-cursor lifetime, `emit_fold_mem` for memory lowering, and
`emit_a64_soft_guard_end` for deferred cold miss stubs. The Rust ownership
matches in `trace.c`: each decoded instruction selects one body emitter, while
the trace builder later finishes each memory guard. Static density therefore
belongs to the trace result rather than an emitter-global or executor-global
counter.

`hl_a64_trace_build_density` now optionally returns bounded aggregate counts
for pair reads, pair writes, other memory operations, control operations, and
all remaining decoded operations. Each family records decoded guest
instruction count, hot emitted words, and deferred guard cold words. Common
prologue, budget, and terminal overhead is reported separately, and the family
plus overhead word counts close exactly to total code size. Additions saturate
and latch a saturation bit. The report is initialized to zero and copied from
a local value only after a successful build, so a rejected build publishes no
partial accounting.

Accounting changes no generated instruction and is disabled for every
production trace/cache call. The focused mixed trace contains one vector-pair
read, one vector-pair write, one scalar load, one arithmetic operation, and
one syscall. It proves every decoded operation is classified exactly once,
each memory guard's deferred cold words return to its originating family,
family plus overhead words equal `code_size / 4`, failed-build accounting is
zero, and accounted and ordinary builds are byte-for-byte identical. The
warning-strict native test passed on AArch64.

This mechanism reports static translated-code density only. It does not claim
that emitted words execute equally often, and it cannot produce an exact
dynamic family mix for the fixed memory phase without diagnostics-only,
per-CPU admission counters and coordinated CPU/public ABI ownership. Existing
build, cache-hit, completed-instruction, and fault-provenance counters cannot
be repurposed for that claim. No per-instruction logging, process-global
mutable counter, schema change, or hot-path instrumentation was introduced.

## Linux W^X arena publication mechanism profile

The retained arena/publication audit covered `../engine/src/translator/arena.c`
(`hl_arena_reserve`, `hl_arena_bind`, `hl_arena_repair`, and
`hl_arena_release`), `../engine/src/translator/cache.c` (`jit_wprot`,
`jit_publish_code`, `code_mapping_reserve`, and `jit_cache_init`), and the
Linux host implementation in `../engine/src/host/linux/host.c`
(`hl_linux_memory_publish`, `hl_linux_memory_code_write`, and
`hl_linux_memory_reserve_code`). The retained translator owns one process-wide
arena mapping and a constant RW-to-RX alias delta. A dual mapping uses a memfd
with separate shared RW and RX aliases, makes the write gate a no-op, and
flushes the executable alias while holding the host mapping-table lock. Its
Linux non-dual fallback is a same-address RWX mapping whose write gate is also
a no-op. macOS instead defaults to one `MAP_JIT` mapping and brackets writes
with `pthread_jit_write_protect_np`; Windows normally uses dual section views
and `FlushInstructionCache`. Fork repair can replace or recouple aliases and
must preserve the mapping handle, executable address where requested, content
bound, and process-private lock lifetime.

The Rust-owned correspondence is `src/native/exec/src/arena.c`, which owns
validated bump allocation, ordered publication, write-window state, counters,
rotation, and teardown, and `src/containers/hl-engine/src/native/executor.rs`,
whose `ExecutableMemory` owns the host mappings. Linux dual allocation is the
same memfd/RW+RX mechanism. Publication flushes the executable alias. Unlike
the retained Linux RWX fallback, the Rust single-alias path starts at
`PROT_NONE` and changes the complete 64-MiB arena between RW and RX with
`mprotect` at each write-window boundary. Thus the measured single-alias path
is the current Rust safety mechanism, not a claimed retained-C timing match.

`test/arena_publish_bench.c` exercises the arena API with both Linux mechanisms.
Mapping creation and destruction are outside the timed interval. Every cohort
has 64 ordered allocations and publications and was repeated five times pinned
to logical CPU 2. Sequential cohorts open and close a write window for each
publication; batched cohorts use one window for all 64. The timed workflow
includes payload writes and their first-touch page faults as well as arena
bookkeeping, executable-alias cache flush, and protection changes. It is
therefore an end-to-end publication-workflow measurement, especially for the
4-KiB and 64-KiB spans, rather than an isolated instruction-cache latency.

| Linux mechanism | Schedule | Span | Median ns | Min--max ns | Publishes / flushes / `mprotect` |
|---|---:|---:|---:|---:|---:|
| dual alias | sequential | 32 B | 6,709 | 6,416--8,125 | 64 / 64 / 0 |
| dual alias | batched | 32 B | 6,666 | 6,625--6,875 | 64 / 64 / 0 |
| dual alias | sequential | 4 KiB | 84,917 | 81,041--130,208 | 64 / 64 / 0 |
| dual alias | batched | 4 KiB | 88,333 | 84,500--90,917 | 64 / 64 / 0 |
| dual alias | sequential | 64 KiB | 1,173,292 | 1,021,917--1,530,667 | 64 / 64 / 0 |
| dual alias | batched | 64 KiB | 1,067,459 | 947,333--1,462,375 | 64 / 64 / 0 |
| single alias | sequential | 32 B | 99,917 | 92,333--163,833 | 64 / 64 / 128 |
| single alias | batched | 32 B | 6,958 | 6,750--10,083 | 64 / 64 / 2 |
| single alias | sequential | 4 KiB | 127,917 | 119,625--200,083 | 64 / 64 / 128 |
| single alias | batched | 4 KiB | 47,583 | 42,542--52,792 | 64 / 64 / 2 |
| single alias | sequential | 64 KiB | 899,917 | 860,166--1,167,583 | 64 / 64 / 128 |
| single alias | batched | 64 KiB | 544,917 | 535,459--1,184,584 | 64 / 64 / 2 |

The exact counters show the dominant coherent synchronization mechanism only
for the single-alias fallback: repeated full-arena protection changes dominate
small sequential publications, while one batched window removes 126 of 128
`mprotect` calls. The arena already exposes that batching contract and the
normal Linux configuration requests dual aliases, where sequential and batched
32-byte medians were effectively identical. Larger-span timing is noisy and
payload dominated, so it does not justify changing flush granularity. No
production source changed: preserving current W^X, publication ordering,
signal/fork repair, and mapping lifetime is preferable to adding another
synchronization scheme.

The measurement host was Linux AArch64 under OrbStack, kernel
`7.0.11-orbstack-00360-gc9bc4d96ac70`, with 18 logical CPUs. The direct build
used `cc -std=c11 -O2 -Wall -Wextra -Werror` against `arena.c`. Before the run,
11 GiB memory was available, 2.4 GiB of 28 GiB swap was used, workspace free
disk was 255 GiB, and `/tmp` free space was 7.5 GiB. The benchmark created and
released each bounded 64-MiB fixture per repeat; no long-lived descendant or
output corpus was created.

## AArch64 native entry and fault-stack blocker audit

The retained boundary audit covered `../engine/src/core/dispatch.c` symbols
`run_block` and `block_return`, including both GCC assembly and Clang naked
forms. It saves host x19--x30, q8--q15, and SP into the CPU record, branches to
translated code without a compiler frame, and restores that image before
returning to the dispatcher. Generated code changes SP to the guest stack only
after the host SP has been saved. Its fully spilled exit restores x0 to the CPU
owner before entering `block_return`. The retained signal path reconstructs
the guest pre-operation register/vector/FP state and redirects the host signal
context to `block_return`; it does not resume C code on the guest stack.

The Rust ownership maps directly to `src/native/exec/src/arch/aarch64/entry.S`
for the same x19--x30, q8--q15, and SP image; `stub.c` for guest SP/FPCR/FPSR,
guest registers and vector transfer; `fault.c` and `fault_darwin.c` for signal
context reconstruction and redirection; `fault/thread.c` for the bounded
512-KiB per-thread alternate signal stack and publication lifetime; and
`executor.c` for publishing the complete fault scope before native entry and
unpublishing it only after the fully restored return. `entry.S` deliberately
does not save guest FPCR/FPSR: the generated prologue loads those CPU-record
values and the spill stores the live guest values. Fault reconstruction reads
them from the interrupted host context. x18 remains untouched as required by
platform ABIs. Fork repair clears active fault publication and reinstalls the
owned alternate stack without changing the saved host-stack contract.

The historical fast-row `SIGILL` with empty diagnostics was reproduced only as
source evidence, not as a current failure. Its documented GDB PC was an emitted
zero-word `UDF` left by the reverted active-interval guard experiment. The
current exact guest (SHA-256
`07756eb451ec3c063a6ffed129db76b8702d5a37be8ba9cbf79eb77944d052ee`)
completed when invoked with typed direct options:

```text
taskset -c 2-5 target/release/hl-engine --guest-isa aarch64 \
  --engine-option HL_NATIVE_EXECUTION=1 \
  --engine-option HL_NATIVE_DIAGNOSTICS=1 combined-bench \
  --phase syscall --divisor 100000
```

It reported `us=48`, `ok=200`, exit status zero, seven native runs, thirteen
builds, 34 hits, three fallbacks, three sites, 126 services, four fills, ten
branch boundaries, four syscall boundaries, three fallback boundaries, and
129 completed instructions. Thus diagnostics were present before claiming a
native-mode result.

GDB stopped the current debug engine at `hl_native_aarch64_return` with native
guest SP `0x37feab0`, CPU `0xfffff6eba690`, and saved host SP
`0xfffff6eb60e0`. The complete return sequence loaded six GPR pairs and four
vector pairs, restored x29/x30, loaded the saved SP, and changed SP back to
`0xfffff6eb60e0` before `ret`. The caller frame pointer was
`0xfffff6eba4c0`. The process subsequently completed normally; guest and host
stack ranges did not overlap.

One pre-existing `/tmp/hl-aarch64-cycle-current-test` binary initially appeared
to provide a minimal exit-139 reproducer. GDB instead proved stale executable
mapping state: its PC was the first generated prologue instruction at
`0xfffff3ddf000`, but the complete containing arena was mapped `rw-p`, so the
fault occurred on instruction fetch before `ldr x9,[x0,#248]` executed. Its SP
and CPU pointer were still valid and distinct. A warning-strict rebuild from
the current exact sources, including `src/native/exec/cache/*.c`, passed with
status zero. That test exercises a separate aligned 4-KiB guest stack, native
cycle yield, pre-block interrupt, syscall re-entry, and live x19/x20 guest
callee-save semantics.

The capability matrix is therefore complete for normal return, generated
guest-stack transfer, callee-saved integer/vector restoration, FP-state
transfer, signal-context return, alternate-stack ownership, and fork reset.
No current stack smash, canary overwrite, absent native diagnostics, or ABI
frame/alignment divergence was reproduced. No production change was made:
removing any save, moving fault work off the alternate stack, or conflating
guest and host SP would weaken a proven generic contract in response to stale
or reverted evidence.

## AArch64 public-exit state-traffic audit

The retained implementation audit additionally covered
`../engine/src/translator/guest/aarch64/stubs.c` functions
`emit_spill_gpr`, `emit_spill`, and `emit_exit_const`, the `vdirty` owner in
`cpu.h`, and vector-touch admission in `translate.c`. Every retained public
exit normally publishes all guest GPR, SP, NZCV, and 32 vector registers before
`block_return`. A plain syscall has one narrower path: if the runtime `vdirty`
flag is zero it omits the 16 paired vector stores. The first conservatively
classified vector-touching instruction in a chained region sets `vdirty`
before modifying vector state, and every full spill clears it only after
republishing all vectors. The test is dynamic because a vector-writing block
can chain into a statically vector-free syscall block. Interrupt, fault,
branch, fallback, cache transition, and signal paths remain fully spilled.

The current Rust AArch64 prologue and spill in `stub.c` are deliberately
symmetric and always full. The prologue performs four scalar state loads, 16
paired 128-bit vector loads, and 26 guest GPR loads, with SP, NZCV, FPCR, and
FPSR installed before guest execution. The spill performs 16 paired vector
stores, 26 GPR stores, and four scalar state stores. Every public exit kind
uses this spill. Signal faults are the exception in mechanism, not
completeness: `fault.c` reconstructs the complete pre-operation state from the
host context and then redirects to the host-only return trampoline. The
fallback stub is already fully spilled and semantics-free.

| Capability | Retained C | Current Rust | Status / required invariant |
|---|---|---|---|
| Host x19--x30, q8--q15, SP | Saved/restored at every native crossing | Same exact entry/return image | Implemented; cannot be slimmed by guest-state knowledge |
| Guest GPR, SP, NZCV | Full on every public exit | Full on every public exit | Implemented |
| Guest FPCR/FPSR | Full spill normally; omitted only on proven vector-clean syscall | Full on every public exit | Implemented, retained optimization missing |
| Guest V0--V31 | Full except runtime-clean syscall | Full on every public exit | Implemented, retained optimization missing |
| Chained vector dirtiness | Sticky runtime `vdirty`, set before first conservative vector touch | No corresponding currency state | Missing prerequisite |
| Syscall return | Clean path skips only vector/FP publication; re-entry fully reloads CPU state | Always full | Divergent performance, same visible semantics |
| Interrupt/yield | Full spill | Full spill | Implemented; asynchronous checkpoint must remain complete |
| Translated fault | Signal context reconstructs full pre-op state, then host return | Same, on owned alternate stack | Implemented; not eligible for slim spill |
| Branch/fallback/epoch | Full spill before dispatcher/cache work | Full spill | Implemented; consumers may inspect or checkpoint all state |
| Fork and signal lifetime | Full CPU image plus repaired host mapping/signal ownership | Execution gate, fault scope, alternate-stack reset, mapping repair | Implemented; dirty currency would have to survive chains and reset coherently |

The fixed host ABI trampoline itself executes eleven state-memory operations
on entry and eleven on return: six GPR pairs (96 bytes), four vector pairs
(128 bytes), and SP (8 bytes) in each direction. That is 232 bytes each way,
464 bytes of fixed host-state traffic per crossing, plus four control/data
instructions total (`mov`/`br` on entry and `mov`/`ret` on return). None is redundant under AAPCS64:
translated code uses the callee-saved registers as guest state, and signal
return depends on the exact saved host image.

The current generated full prologue reads 752 bytes from the CPU record and
the spill writes 752 bytes: 512 vector bytes, 208 GPR bytes, and 32 bytes of
SP/NZCV/FPCR/FPSR in each complete direction. Together with the trampoline, a
full crossing therefore moves 1,968 bytes of state before the small
program/reason epilogue. A retained-style clean-syscall spill would omit 512
bytes, reducing that crossing to 1,456 bytes; the full prologue remains
necessary because the syscall service, especially signal return, may update
the CPU-record vector or FP state before re-entry.

The proved syscall fast row observed ten branch, four syscall, and three
fallback boundaries: 58.8%, 23.5%, and 17.6% of the 17 diagnosed public-exit
events. With all four syscalls vector-clean, the strict traffic upper bound is
a 2,048-byte reduction from 33,456 to 31,408 bytes across that event mix, or
6.12%. This is an upper bound, not a timing claim: the scheduler's seven
reported native runs are not an exact full-versus-slim spill counter, and the
current diagnostics do not identify whether a syscall region touched vector
state.

The one coherent candidate is therefore the retained generic runtime-clean
syscall spill, not removal of host ABI saves or slimming asynchronous exits.
Its acceptance batch must add CPU-schema currency state, conservative
classification covering every scalar/vector FP and SIMD write, set-before-use
ordering on all direct and chained entries, and full-spill clearing only after
publication. Focused tests must cover a vector-free syscall, vector write
chained into syscall, scalar FP/FPCR/FPSR effects, signal return modifying
vector state, interrupt and yield after vector mutation, translated fault,
fallback, checkpoint before and after syscall, fork child repair, direct and
indirect chains, and both guest ISAs on an AArch64 host. Diagnostics should add
full-spill, slim-syscall-spill, and dirty-set counters before performance
measurement. A clean exact-tree five-repeat run must then beat observed noise
with identical exit mix and guest output.

No production or test source changed in this audit. In particular, no guard,
pair, dirty-journal, or guest-memory mechanism was modified. The estimated
6.12% state-traffic ceiling is too small and insufficiently attributed to
justify a schema and emitted-code change before the proposed counters and
correctness gates are reviewed.

## AArch64 chain continuation and dispatcher-return ownership

The retained chain audit covered
`../engine/src/translator/guest/aarch64/stubs.c` functions
`emit_chain_exit_from`, `emit_smc_chain_exit`, and the indirect-branch cache
paths rooted at `emit_ibranch_steal`. A resolved direct edge is a single branch
to the target body and performs no spill, prologue reload, or dispatcher
round-trip. An unresolved edge initially reaches a shared full-spill branch
exit and is patched when its target becomes available. Indirect monomorphic
and shared-cache hits likewise jump directly to a body with live guest state;
only misses fully spill and publish the site for dispatcher fill. After
self-modifying code is observed, retained direct ownership becomes more
conservative: the shared indirect cache revalidates the target and immutable
body identity instead of retaining an unsafe direct edge.

The corresponding Rust owners are `arch/aarch64/direct.c` and
`conditional.c` for relocatable typed branch exits, `indirect.c` for the
per-site and shared IBTC probes, `trace.c::trace_build` for body-offset and
relocation publication, `cache/relocation.c` for patch admission, cycle
retention, invalidation, and typed-edge restoration, and
`executor.c::run_aarch64`/`ibtc_fill` for cold continuation. A direct patch
targets `body_offset`, after the one-time prologue but before the executing
identity and budget/interrupt guard. Consequently live guest registers cross
the edge without CPU-record traffic while every entered block still publishes
its identity and polls the public interruption contract. A monomorphic IBTC
hit uses only stolen x16/x17; the shared hit temporarily preserves guest x9.

| Edge or exit | Hot mechanism | Full 1,968-byte state crossing | Dispatcher ownership and safety reason |
|---|---|---:|---|
| Resolved acyclic direct branch | Patched `b target.body_offset` | No | Generation, memory mode, authority, range, and target epoch matched at patch time |
| Direct edge closing a cycle | Typed branch exit retained | Yes | At least one cycle edge must preserve bounded budget/interrupt progress; 64-node admission saturates conservatively |
| Unresolved direct target | Typed branch exit, pending relocation | Yes once per cold availability interval | Dispatcher translates/publishes target, then resolves the pending edge |
| Invalidated direct target/source | Original typed exit restored before reclamation | Yes until re-resolved | No stale body pointer may survive epoch, source overlap, reset, or fork repair |
| Per-site indirect hit | Patched direct branch | No | Target token and exact site identity were filled by dispatcher |
| Shared IBTC hit | Target/body pair probe then `br body` | No | Target comparison, non-null body, and generation-qualified fill |
| Indirect miss/collision | Full spill with aligned site token | Yes | Dispatcher must translate, validate target identity, and atomically refill both caches |
| Ordinary branch continuation after typed exit | Internal `run_aarch64` loop | Already paid | Complete state permits lookup/build while mutation admission may be released and reacquired |
| Operand fallback resolved from run-view cache | Internal retry | Already paid | Full pre-operation state and exact executing identity are required before retry |
| Operand callback fallback | Public callback or terminal fallback | Yes | Host call cannot run with guest SP/live-only architectural state |
| Epoch/executable-write exit | Public epoch result | Yes | Mutation/checkpoint authority invalidates cached code and projections |
| Signal fault | Alternate-stack reconstruction then host return | Yes-equivalent complete publication | Exact pre-op state, provenance, and host frame restoration are mandatory |

The current fast row diagnosed 34 translated-cache hits, thirteen builds,
four IBTC fills, ten typed branch boundaries, four syscalls, and three
fallbacks while completing 129 guest instructions. The already-internal hot
edges are represented indirectly by completed instructions and cache hits, not
by `boundary_branch`; that counter increments only after a full spill reaches
the native return. Typed branch exits were therefore 58.8% of the 17 diagnosed
full public boundaries and occurred at 77.52 per thousand completed guest
instructions in this deliberately tiny cold workload.

Eliminating all ten typed branch exits would have a mathematical traffic
ceiling of 19,680 bytes, or 58.8% of boundary state traffic, which exceeds the
observed timing-noise thresholds. It is not an achievable optimization bound:
the current diagnostics do not divide those exits among first-target
publication, deliberately retained cycle edges, invalidation restoration,
IBTC miss, or capacity/range rejection. Each of those classes except redundant
rejection has a distinct correctness owner, and the existing relocation and
IBTC mechanisms already remove the hot repeat from the dispatcher.

The only candidate large enough to consider would be broader eager acyclic
successor translation and relocation before first execution. The current
pending-relocation mechanism already performs the safe half of that operation
when a target is published; eagerly invoking translation would expand build
ownership, resolver calls, source lifetime, mutation admission, code-cache
pressure, and cycle analysis. It cannot be justified from ten unattributed
cold exits. Before review, read-only attribution should add counters for direct
cold target, retained cycle, relocation range/capacity, invalidation-restored
edge, per-site indirect miss, shared miss/collision, and successful internal
direct/IBTC continuations. A candidate would then need identical budget and
interrupt behavior on two-entry and saturated cycles, exact invalidation and
fork restoration, authority/memory-mode isolation, bounded eager depth and
code growth, and a five-repeat exact-tree workload improvement beyond noise.

Fallback and epoch paths are not candidates for internal live-state handling.
Their consumers may call host code, mutate mappings, checkpoint, deliver a
signal, or reject the executing authority; all require a complete pointer-free
architectural record and host SP restoration. No production or test source
changed in this audit, and no new chain optimization is recommended until the
typed branch counter is causally partitioned.

## AArch64 guest SIMD and floating-point lowering domain audit

The retained-domain audit covered the SIMD/FP classification and stolen-GPR
analysis at the start of
`../engine/src/translator/guest/aarch64/translate.c`, its optional I8MM and
BF16 host-feature probes and lowerings, scalar FP/GPR conversion handling,
AdvSIMD copy forms, vector-dirty classification, scalar and vector literal
loads, ordinary SIMD/FP verbatim emission, and the complete structure-memory
folding path. It also covered `stubs.c` prologue/full-spill ownership and
`signal.c` FP/SIMD context reconstruction. The retained implementation treats
baseline same-ISA SIMD/FP opcodes as native instructions after proving they do
not name stolen host GPRs. It specially rewrites cross-file forms such as
FMOV/conversions, DUP/INS/UMOV/SMOV, and lowers optional instructions when the
host and advertised guest feature sets can differ. FPCR, FPSR, NZCV, all 32
128-bit vectors, and fault-time pre-operation state remain architectural.

The current Rust owners are `floating.c` for scalar FP arithmetic/comparison
and three-source fused forms, `fp_move.c` for 32-, 64-, and upper-64-bit
GPR/vector transfers with stolen-register rewriting, `simd_immediate.c` for
AdvSIMD modified immediates, `simd_compare.c` for a small integer comparison
subset, `simd_narrow.c` for immediate narrowing shifts, and `broadcast.c` for
DUP element/general forms. `single.c`, `pair.c`, and `structure.c` own guarded
scalar/vector memory operations, writeback, partial-fault ordering, and
structure interleave/replicate shapes. `stub.c` loads and spills FPCR, FPSR,
NZCV, and all vectors; `fault.c` reconstructs them from Linux/Darwin signal
contexts. Unsupported first instructions decline translation; unsupported
instructions later in a trace produce a typed fallback after all prior
instructions are charged and spilled.

| Family | Widths/forms | Retained C | Current Rust | Evidence / gap |
|---|---|---|---|---|
| Scalar FP arithmetic and compare | S/D, unary, binary, conditional compare/select, FMADD family | Native with guest FPCR/FPSR | Broad opcode-box pass-through in `floating.c` | Implemented shape; lacks dense focused acceptance across rounding/exceptions |
| FP/GPR moves | W/S, X/D, X/V.d[1] | Stolen-GPR rewrite | Explicit rewrite in `fp_move.c` | Implemented; direct focused file is absent |
| Integer/fixed FP conversions | Signed/unsigned, W/X, S/D, fixed shifts, rounding variants | Explicit GPR-field classification and rewrite | Zero-opcode conversion forms excluded from `floating.c`; only move opcodes handled | Coherent missing family |
| Modified immediate | MOVI/MVNI/ORR/BIC, integer and FP immediates, D/Q arrangements | Native | Exhaustive encoding test plus execution samples | Implemented focused subset |
| SIMD copy/lane | DUP/INS element/general, UMOV/SMOV | All forms with stolen-GPR handling | DUP forms only in `broadcast.c` | INS and lane-to-GPR families missing |
| Integer SIMD ALU | add/sub, saturating, abs/neg, min/max, halving, multiply, dot, polynomial | Baseline native; optional I8MM lowered/probed | Only three compare opcodes and narrowing shifts have typed owners | Large missing family |
| Vector FP | arithmetic, fused, min/max, compare, reciprocal/rsqrt estimate/step, conversions | Baseline native under guest FP state | General vector boxes are not admitted by `floating.c` | Large missing family with FP exception/rounding gates |
| Widen/narrow/shift | long/wide/high-half, saturating, variable/immediate, rounding | Native | Immediate SQSHRN/UQSHRN slice only | Large missing family |
| Permute/table/reduce | EXT/ZIP/UZP/TRN/TBL/TBX, ADDV and min/max reductions | Native | No typed general owner | Missing |
| Crypto | AES, SHA1/SHA2 and polynomial helpers according to advertised HWCAP | Native | No typed crypto owner | Missing and compatibility-visible |
| Optional matrix/BF16 | I8MM, BFCVT/BFDOT and host-specific availability | Host probe plus baseline lowering where exact | No host-feature policy/lowering owner | Missing; verbatim admission alone would be unsafe |
| Scalar/vector loads | B/H/S/D/Q, literal, unsigned/unscaled/pre/post/register forms | Guest-address fold with precise fault semantics | Guarded `single.c`, including direct-authority path | Implemented subset with transactional writeback |
| Vector pairs | S/D/Q load/store pairs and addressing modes | Folded, partial-result aware | Guarded `pair.c`; focused Q-pair and fault tests | Implemented typed slice |
| AdvSIMD structures | LD/ST1--4, lane/multiple/replicate, immediate/register post-index | Dedicated interleave/fold path | Small typed `structure.c` subset | Partial; wider list/lane arrangements remain |
| FP state and signals | FPCR rounding/flush/default-NaN, FPSR sticky exceptions, vectors | Full prologue/spill and signal reconstruction | Same ownership in stub/fault boundary | Implemented mechanism; per-op exception evidence sparse |

Focused current tests strongly cover modified-immediate encodings, DUP
broadcast forms, Q-vector pair memory and fault rollback, and a small structure
load/store/interleave/replicate cohort. There is no comparably dense focused
test file for `floating.c`, `fp_move.c`, or `simd_compare.c`. Repository
compatibility evidence is mixed by revision: `FULL_CORPUS_001.tsv` records
passing AArch64 `abi/shuffle`, `abi/simd`, `simd_syscall`, and
`simd_syscall_crypto`, proving complete boundary preservation for their opcode
sets. The retained differential snapshot in `report/differential.tsv` records
C-pass/Rust-gap results for `crypto-aes`, `crypto-sha1`, `crypto-sha256`,
`neon-misc`, `neon-recip`, `ipc/neonshm`, `printf-float`, `printf-hexfloat`, and
`strto-float`. Those reports are not exact-current-tree completion evidence,
but they identify domain-sized gaps rather than one fixture-specific branch.

The largest coherent missing family is the baseline AdvSIMD/FP data-processing
box together with its cross-file GPR forms and feature policy. Porting only the
first failing crypto or reciprocal opcode would repeat fixture-driven
migration. A valid lane must mechanically inventory the complete architectural
encoding families above, admit baseline vector-only forms, rewrite every
GPR-crossing operand that can name x16/x17/x18/x28/x30, and explicitly lower or
reject optional I8MM/BF16 and any feature not advertised to the guest. The
likely performance impact is larger than a local emitter tweak: every admitted
instruction stays in a chained native trace, whereas a missing instruction
ends the trace, performs the 1,968-byte public crossing, and may fall back to
the interpreter.

Correctness gates must compare C and Rust results for every element width and
Q arrangement; scalar and vector aliases; overlapping operands; zero/SP GPR
forms; all four FP rounding modes; FZ, DN, NaN payload/sign, signed zero,
infinities, subnormals; FPSR IOC/DZC/OFC/UFC/IXC/IDC accumulation; saturating
QC; conversion overflow and indefinite results; host-feature mismatch; and
signal/checkpoint/fork immediately after an FP exception or vector write.
Memory forms additionally require exact pre/post-index ordering, no writeback
on fault, partial structure semantics, cross-page widths, permissions, epoch
changes, and direct-authority invalidation. A performance run must first add a
fallback-reason/opcode-family counter so reduced public crossings can be
attributed to this port.

No production or test source changed in this audit. The recommended review
unit is the complete baseline AdvSIMD/FP decode and stolen-register/feature
admission mechanism, with optional-feature lowerings as an explicit submatrix;
it is not safe to enable the entire encoding box by verbatim passthrough before
those gates exist.

## x86-64 guest dispatcher fixed-cost capability and cost audit

This read-only lane followed the complete retained x86-64 guest/AArch64 host
entry and return call graph through `../engine/src/core/dispatch.c`
(`run_guest`, `run_block`, and post-return `G_DISPATCH_REASON`),
`../engine/src/translator/guest/x86_64/translate.c` (`run_block`,
`block_return`, translation tails), `emit.c` (`emit_prologue`,
`emit_spill_gpr`, `emit_spill`, and `emit_exit_const`), and `dispatch.h`
(`G_DISPATCH_ENTER`, `G_DISPATCH_DEBUG`, and `G_DISPATCH_REASON`). The current
owner comparison covered `src/arch/x86_64/entry.S`
(`hl_native_x86_64_enter`), `run.c` (`emit_block`, `finish_execution`,
`spill_registers`, and `hl_native_x86_64_run`), and `frontend.c`
(`hl_x86_a64_emit`). Both engines emit AArch64 for this guest/host pairing;
non-AArch64 retained hosts abort this backend, while the current entry symbol
is absent outside AArch64.

| Boundary capability | Retained C oracle | Current Rust-owned native path | Fixed-cost observation |
|---|---|---|---|
| Host entry | `run_block` saves x19--x30, q8--q15, and host SP, then branches to translated code | `hl_native_x86_64_enter` saves x18--x30 on a 112-byte frame, host FPCR/FPSR, and q8--q15 in CPU storage | Current outer entry executes 7 host-GPR stores, 4 vector-pair stores, 2 system-register reads/stores, and one indirect call before guest restoration |
| Guest entry | `emit_prologue` pins CPU in x28 and reloads NZCV, 16 XMM registers, and 16 GPRs | Outer entry reloads FPCR/FPSR, 8 vector pairs, and 8 GPR pairs; each published translation then executes BTI, budget/interrupt admission, and its frontend body | Current outer entry has 8 vector-pair plus 8 GPR-pair loads; block entry adds 2 scalar loads, 2 conditional branches, a count materialization, and compare |
| Ordinary return | Typed retained tails spill architectural state and branch to `block_return`; live direct chains bypass the dispatcher | Each non-chained typed tail charges completion, spills all 16 GPRs with 8 STP instructions, and returns through the outer entry | Current generated common tail is 1 load, 1 subtract, 1 store, 8 GPR-pair stores, and `RET`, excluding reason/PC stores; the outer return adds 8 guest-vector pair stores |
| Host return | `block_return` restores x19--x30, q8--q15, and host SP | Outer entry restores host FPCR/FPSR, q8--q15, x18--x30, and SP | Symmetric 4 vector-pair and 7 host-GPR loads plus FP state restoration |
| Budget | Retained blocks use bounded dispatcher/translation boundaries but expose no equivalent request-scoped instruction budget | Request budget enters `x26`; entry rejects blocks larger than the remainder, checkpoints decrement it, and C charges `scratch[0]`; overspend is fatal | Zero budget yields before entry; insufficient block budget yields without partial entry; a completed syscall may return with nonzero budget |
| Interrupt | Retained block-entry IRQ poll returns fully spilled state to `run_guest`, which delivers pending signals after reason handling | C checks `cpu->interrupt` before lookup and emitted entry reloads it before every admitted block; post-return interrupt outranks branch continuation | Interrupt at outer entry executes zero guest instructions; a chained block retains bounded entry polling |
| Syscall | Guest RIP is advanced by the emitter; full or vector-clean slim spill returns, SMC is committed, `service` runs, and execution resumes | Frontend emits a typed syscall reason and exact next PC; `hl_native_x86_64_run` returns `HL_NATIVE_EXIT_SYSCALL` to the Rust personality | Current outer boundary always saves and reloads all 16 guest vectors; it has no retained `vdirty`-qualified slim syscall path |
| Fallback/fault | Retained reason handling owns C-emulated instructions, soft-map misses, traps, division, SMC, and syscall service | Native runner returns typed fallback/fault/epoch identities to Rust; operand resolver retries only after precise projection checks | No host callback or table lock is taken inside generated code; misses leave execution ownership before translation/resolution |
| Concurrency/teardown | Dispatcher registry and STW generation protect cache lookup, execution, flush, signals, and teardown | `hl_native_execution_enter/leave`, epoch identity, projection authority, cache invalidation, and provenance scopes own admission/lifetime | Neither implementation holds its translation-table lock across a guest syscall; architecture-specific fault reconstruction remains separate |

The largest generic candidate to measure is vector-dirty-qualified syscall
crossing. The retained engine already demonstrates the mechanism: a runtime
`vdirty` latch lets a vector-clean syscall use `emit_spill_gpr`, avoiding eight
128-bit guest-vector stores, while every following prologue republishes the
saved vector file. The current path unconditionally performs eight guest-vector
stores on every native return and eight guest-vector loads on every re-entry.
This is a structural maximum of 16 vector memory instructions per syscall
round-trip, separate from the mandatory host q8--q15 preservation. It is not
yet a speed claim. Before implementation, bounded diagnostics must measure the
syscall share and vector-dirty share after public-exit classification; then a
pinned fixed-work guest needs five checksum-identical retained/current/candidate
repetitions. Correctness gates must include a vector-dirty chain reaching a
vector-clean syscall, sigreturn vector replacement, fallback/fault exits,
interrupt entry, fork, and cache-generation retirement.

The warning-strict direct baseline used all current native C sources plus both
entry assemblies. `x86_budget` and `x86_rep` compiled and passed with no output:

```sh
sources=$(find src/native/exec/src src/native/exec/cache -type f -name '*.c' -print)
cc -std=c11 -Wall -Wextra -Werror \
  -I src/native/exec/include -I src/native/exec/src -I src/native/cpu/include \
  $sources src/native/exec/src/arch/aarch64/entry.S \
  src/native/exec/src/arch/x86_64/entry.S \
  src/native/exec/test/x86_budget.c -lpthread -o /tmp/hl-x86-budget-audit
/tmp/hl-x86-budget-audit
cc -std=c11 -Wall -Wextra -Werror \
  -I src/native/exec/include -I src/native/exec/src -I src/native/cpu/include \
  $sources src/native/exec/src/arch/aarch64/entry.S \
  src/native/exec/src/arch/x86_64/entry.S \
  src/native/exec/test/x86_rep.c -lpthread -o /tmp/hl-x86-rep-audit
/tmp/hl-x86-rep-audit
```

The same warning-strict `x86_chain` compile is presently blocked before
execution by three test-only positional `hl_native_projection` initializers
that omit the newer `active` field (lines 180, 289, and 361). This audit did
not weaken warnings or edit that shared test. No production source changed.

## Rejected run-scoped AArch64 view certificate

The exact baseline was `a0ff93cd4b003022afc47fb6f21e52eef4256679`; the
corrected candidate was `e34a8a4b7ed43b5384a479b521fd9137df86b9b4`. The
retained-C reference remained
`7b7bddddfe7fc32f98a74579f38ee92b3a76fcdc`. The candidate appended a complete
run-scoped first/last/delta/permissions/incarnation/authority certificate,
published it only under a live projection lease and native execution admission,
and made every ordinary AArch64 guard try it before the complete selector.
Write-cache owner transitions invalidated the certificate before changing the
exact dirty owner.

Warning-strict `hl-engine --lib --no-run`, the direct native `aarch64_trace`,
and the focused cached-write-view test passed. A broader engine run found only
three previously classified invalid/racy tests after the cached-write owner bug
was corrected. Focused native tests covered exact issuance, a valid hit, stale
authority and incarnation, permission and bounds rejection, checked-end
overflow, exact successful dirty ranges, and no publication for a faulting
store.

Both release binaries were built alone with 18 Cargo jobs. Their SHA-256 values
were respectively
`f4f39b9def5627dad59cc2c91e784a75b35b7e42e68abb47c7b32500ec6fad54`
and
`f77ddd89460e6eb7ad7288f40a474f2a943dfc684cd5b7fd7af46ae7238bf7b9`.
The combined guest source hash was
`ec97f6f5c598f6fc229231dbf4751fb298ebaf1ae04c530d8aecbc7a1ec926af`.
The temporary manifest selected `--divisor 20 --phase memory`, one warmup and
one measured repetition; its hash was
`2962e46663e7e9557fc73058904d764e012ecc9bb8c40e9f2599ecff7b73def6`.
The checked-in manifest was restored after measurement.

Root confirmed a quiet window before five alternating baseline/candidate pairs.
Every invocation was pinned to CPU 17 with its `performance` governor and used:

```text
taskset -c 17 env \
  HL_COMPAT_ENGINE_OPTIONS='HL_NATIVE_EXECUTION=1;HL_NATIVE_DIAGNOSTICS=1' \
  <exact-release-testing> bench combined --isa arm64 --jobs 1 \
  --results target/testing/a64-cert-<tree>-<pair>.tsv
```

At the earlier artifact-binding checkpoint the host had 15 GiB available RAM,
23 GiB free swap, 149 GiB free on `/Users`, and 12 GiB free in `/tmp`. The
first attempted run was discarded while unrelated compilation was active. No
timing below came from that contaminated run.

All ten scored rows passed with typed native execution and diagnostics. The
memory checksum remained `36526`, matching the pinned workload's recorded
control. Execution counts were identical: 900 runs, 229 builds, 4,898 hits, 16
fallbacks, 7 sites, 22 services, 58,122,035 completed instructions, 2,780
operand callbacks, and 833 operand-cache hits. Baseline guards were
0 fast / 29,021,179 full / 3,626 fallback; candidate guards were
13,421,658 fast / 15,599,521 full / 3,626 fallback. Dirty counters were
identical: 14,536,779 reserved and committed, 3 overflow, and 14,534,044
merged.

| Pair | Baseline guest/wall | Candidate guest/wall |
| ---: | ---: | ---: |
| 1 | 183999 us / 216 ms | 189129 us / 222 ms |
| 2 | 181698 us / 212 ms | 189678 us / 222 ms |
| 3 | 180507 us / 210 ms | 220445 us / 252 ms |
| 4 | 180057 us / 209 ms | 188896 us / 219 ms |
| 5 | 183706 us / 216 ms | 202699 us / 236 ms |

Baseline guest min/median/max was 180057/181698/183999 us; candidate was
188896/189678/220445 us. Baseline wall min/median/max was 209/212/216 ms;
candidate was 219/222/252 ms. Candidate medians regressed by 4.39% guest and
4.72% wall, and every paired sample was slower. The mechanism reduced full
selector executions but its added per-access identity/range checks and static
code outweighed that saving. The production candidate was therefore reverted;
no performance claim is accepted.

## Rejected ingress-scoped AArch64 view certificate

A follow-up measured the corrected ingress-scoped certificate from exact clean
baseline `ad5dc3b4264174eb8e9c3ece1390c7a83b5608b2` against candidate
`a21de7906891de1dda2564d6a14bc7cb732365c8`. The complete evidence is
`/Users/x/dd/.performance/a64_certificate/20260805T001935Z`; its provenance and
resource-record SHA-256 values are respectively
`42347ce3f1bd3f03004cd7b37042f4a5ec951f4eb36d1cd42f879dd28173661f`
and `1dfd0c1f70d0bdbd25db8a4dba45bd56c9d79578df0c078edc807a7adcdc3395`.
The guarded harness SHA-256 was
`69e58a1049c6c03a5c771f122c025604b71f7192632a94986c3bac3f0037b7e7`.

The baseline testing/engine artifact SHA-256 values were
`3416aaf003e45455ae4d86139092d242167c7e9ddab5137d9431a865e2277b07`
and `7571fc7ab995273a4f734ac018dd28587c7af8626267cab46520db8134d1b962`;
the candidate values were
`7baa08ecaf807349c27d3fcc4ae7836dc4bdede057f3c7625d0f61bd28892fd7`
and `ddff6868c19f91e6439a2c72b6045cc34266899d0a6ca0fdda496b9a09da0375`.
Every run used guest artifact
`a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9`.

Five alternating CPU-17-pinned, native-verified pairs produced these guest
times:

| Pair | Baseline | Candidate |
| ---: | ---: | ---: |
| 1 | 186845 us | 192360 us |
| 2 | 186470 us | 193866 us |
| 3 | 187340 us | 194255 us |
| 4 | 187242 us | 195578 us |
| 5 | 186402 us | 193667 us |

The medians were 186845 us and 193866 us: the candidate regressed by 3.758%
and lost every pair. All samples returned checksum 36526. Runs, builds, cache
hits, fallbacks, sites, services, completed instructions, operand callbacks,
operand-cache hits, dirty reservations, overflows, commits, merges, and guard
fallbacks were identical. Only 2,320 of 29,021,179 ordinary guard selections
used the certificate fast path, approximately 0.008%.

The candidate is rejected. Active-view selection means that the view certified
at ingress rarely matches the views actually read by this workload, so paying
the certificate machinery cannot remove enough complete selections to offset
its cost. No production source from this candidate is accepted.

## Consolidated AArch64 call, fallback, and floating-point evidence (2026-08-05)

## AArch64 call and return fallback audit

Baseline: `067f91763415c54ef0f5d0bc5f7226bade17523b`. The measured calls workload
completed 9,422 instructions with 180 fallback boundaries (152 classified
other and 11 control), 572 builds, 560 branch boundaries, and 61 IBTC misses.

### Retained oracle

The read-only implementation studied was
`../engine/src/translator/guest/aarch64/translate.c` (`translate`, direct
`b`/`bl`, `br`/`blr`/`ret`, PAC-hint and `retaa`/`retab` cases), `stubs.c`
(`emit_ibranch*`, `emit_shadow_ret`, `emit_bl_ras`, and chain exits), and
`dispatch.h` (`G_IBTC_FILL`). `cache.c` supplies the map and arena lifetime used
by those paths. Translation and cache state are process-owned; the JIT lock
serializes publication, while generated readers are lock-free. Misses spill the
target and site before dispatcher return. Shared entries publish body before
target in the single-thread path and as one atomic pair in the threaded path.
Cache flush tears down shared reachability; once SMC is observed, retained code
does not publish caller-owned monomorphic sites. Direct calls publish guest x30
before transfer. Indirect calls read the target before replacing guest x30.

### Capability map

| Retained capability | Rust owner | Status |
|---|---|---|
| direct `b`/`bl`, x30 update, patchable edge | `direct.c`, `trace.c` relocations | implemented |
| `br`/`blr`/`ret`, including `blr x30` ordering | `indirect.c` | implemented |
| per-site monomorphic and shared 64K IBTC | `indirect.c`, `executor.c::ibtc_fill` | implemented |
| generation-safe invalidation/publication | native cache and executor admission | implemented |
| PAC hints as no-op; `retaa`/`retab` as `ret x30` | `system.c`, `indirect.c` | implemented here |
| shadow return stack / host RAS call-return pairing | none | missing optimization |
| megamorphic interpreter-dispatch shared-only probe | none | missing optimization |

The two remaining items are speculative performance mechanisms, not missing
architectural call/return behavior. A shadow stack adds lifetime, mismatch,
signal, fork, and unwind state and is not justified by the observed 61 IBTC
misses without a separately measured return-heavy A/B workload.

## AArch64 fallback attribution

The retained diagnostic ownership was audited in
`../engine/src/core/dispatch.c::run_guest` and the AArch64 translation refusal
paths in `../engine/src/translator/guest/aarch64/translate.c`. Retained code
keeps diagnostics outside signal handlers and attributes dispatcher reasons
after architectural state is fully published. The Rust owner is
`src/executor.c::run_aarch64`; cache-build declines and guard-resolver exits are
the two bounded points where the refusal category is known without logging a
guest PC or retaining guest data.

The first ABI extension appended six monotonic counters, preserving the complete
336-byte prefix. Guard read/write counters identify projection resolution.
Terminal declines are categorized as SIMD/FP, memory, control/system, or other
from the fixed instruction word. The subsequent 64-byte extension preserves the
complete 384-byte ABI and separates trace-build entry rejection from a fallback
stub reached after a generated prefix. It also records mutually exclusive call,
return, indirect branch, system, memory, and other instruction forms. Guard
resolver exits count as generated memory fallbacks and remain separately divided
by access direction. Counters are atomic because executor instances admit
concurrent guest threads. They are initialized, incremented, read, and printed
only when native diagnostics are enabled; ordinary execution does no attribution
work.

The retained files inspected for the phase/form extension were
`../engine/src/translator/guest/aarch64/translate.c` (`translate`, direct branch,
indirect branch, return, exception, and system decode paths), `stubs.c`
(`emit_ibranch_ip2_ready`, `emit_ibranch`, and dispatcher exits), `dispatch.h`
(`G_IBTC_FILL`), and `src/core/dispatch.c::run_guest`. The process owns retained
translation and cache state; the JIT lock protects build/publication, generated
readers are lock-free, dispatcher return occurs only after architectural state is
spilled, and cache teardown removes published reachability. Architecture form
selection is guest-AArch64-specific; host differences belong to executable-memory
publication and signal entry, not fallback attribution. Retained translation
implements these forms rather than exposing an equivalent rejection-phase counter.
Rust therefore owns the bounded phase/form diagnostic at
`src/executor.c::run_aarch64`, where both build decline and generated exit are
fully identified outside asynchronous fault handling.

These are causal categories, not capability claims. In particular, a guard
fallback means a translated memory operation needs a new projection; it does
not mean the memory opcode is unsupported. Terminal family counters identify
where a complete retained family audit should begin before implementation.

On exact base `09e5ed8ca`, pinned guest SHA-256
`a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9` ran on CPU
17 with typed native execution and diagnostics and `--divisor 60000 --phase
calls`. Seven public runs completed 16 builds, 35 hits, seven scheduler
fallbacks, 15 branch exits, one generated fallback, and 197 instructions. The
phase/form counters attributed six first-instruction build rejections and one
generated memory-guard fallback; form totals were three memory and four other,
with zero call, return, indirect, or system declines. This current-root workload
is smaller than the historical 9,422-instruction row, so its 152 `other` value
cannot be relabeled by extrapolation. A diagnostic `--divisor 1000` row confirmed
the same mechanism at larger scale: 132 entry rejections and two generated guard
fallbacks, divided into five memory and 129 other forms. The old coarse `other`
cluster is therefore mechanically an admission cluster on the measured tree, not
a hidden generated call/return exit.

## AArch64 scalar conversion audit

The retained C oracle was `/Users/x/dd/engine`. The scalar conversion domain
studied was `src/translator/guest/aarch64/translate.c` (`gpr_field_mask`, stolen
register rewriting and vector-dirty classification) and
`src/translator/guest/aarch64/interp.c` (the scalar FP integer-conversion box,
rounding modes, widths, invalid encodings, FPCR and FPSR behavior). The retained
native path emits allocated host-AArch64 conversions directly, rewriting a GPR
operand through scratch when it names engine-private x16, x17, x18, x28 or x30.
The interpreter owns exceptional and unavailable forms.

Husklet already owned FMOV in `fp_move.c`, vector conversions in `simd_float.c`,
FP state entry/exit, and exact guarded memory. It had no native owner for the
ordinary scalar integer-conversion family. In the combined float-SIMD loop,
`SCVTF s28,w1` and `FCVTZU x5,s31` therefore terminated native translation.
The engine completed only 164 native guest instructions and interpreted the
remaining nested array loop. Mapping lookup and guard selection were downstream
of that refusal and could not explain the seconds-scale result.

`fp_convert.c` now admits the allocated non-fixed scalar SCVTF, UCVTF, FCVTNS,
FCVTNU, FCVTPS, FCVTPU, FCVTMS, FCVTMU, FCVTZS, FCVTZU, FCVTAS and FCVTAU
width/type combinations. It executes the architectural instruction directly so
the installed guest FPCR/FPSR retain hardware rounding and exception semantics.
GPR sources and destinations naming stolen registers are loaded from or stored
to the generated CPU record. Half precision, fixed-point, FMOV and reserved
rounding combinations remain with their existing owners or fallback.

The structural trace test covers the two benchmark encodings. The focused
AArch64 FP executable test covers GPR-to-FP, FP-to-GPR, truncation, and stolen
x28 on both sides. The warning-strict Rust native executor suite passed 63 tests
with zero failures and two ignored captures; the standalone warning-strict
`aarch64_fp.c` executable passed on the AArch64 host.

One candidate runner drove both release engines with typed native execution and
diagnostics, the same guest SHA-256
`a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9`, divisor
100 and three measured repetitions. Baseline guest time was 3,556,492 us median
(2,892,879--3,563,191); candidate was 1,805,477 us
(1,605,320--2,013,783), a 49.23% reduction. All samples produced checksum
23,027,281,045. Baseline completed only 164 native instructions; candidate
completed 35,976,315 with stable per-repeat counters. Remaining time is now
measurably dominated by 13,401,361 exact guards and 4,455,730 successful-store
dirty publications, which this change deliberately does not weaken.

## 2026-08-05 AArch64 write-publication evidence index

The cached-write, dirty-range, projection-lease, certificate-integration, and
projected-view-cache measurements and ownership matrices are consolidated in
`WRITE_PUBLICATION.md`. That record preserves the exact candidate revisions,
artifact hashes, benchmark samples, and safety prerequisites without repeating
them in this performance history.
