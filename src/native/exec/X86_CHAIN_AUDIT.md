# x86-64 translated-chain audit

This audit covers the x86-64 guest translated-block lookup and chaining domain.
The retained C tree was read only at `/Users/x/dd/engine`.

## Retained C implementation studied

- `src/translator/cache.c`: `map_idx`, `map_host`, `map_body`, `map_put`,
  `map_clear`, `add_pend3`, `patch_links_to`, `jit_flush_to_fresh`,
  `smc_inplace_drop`, and `jit_after_fork`.
- `src/core/dispatch.c`: `run_guest`, including lookup/translation locking,
  publication ordering, IBTC fill, arena-generation pinning, and block entry.
- `src/translator/guest/x86_64/emit.c`: `emit_chain_exit` and the indirect
  target probe emission.
- `src/translator/guest/x86_64/translate.c`: `translate_block` publication,
  `patch_links_to`, tier-two replacement, and cache/IBTC invalidation.
- `src/translator/guest/x86_64/dispatch.h`: `G_DISPATCH_ENTER`,
  `G_SHADOW_CLEAR`, `G_DISPATCH_CHAIN`, and `G_IBTC_FILL`.
- `src/translator/guest/x86_64/abi.h`, `glue.c`, and `glue.h`: x86 guest-PC
  identity, global cache ownership, and the two-way IBTC lifetime.

The retained engine owns one translation arena, open-addressed guest-PC map,
pending direct-edge index, and IBTC generation. Single-threaded lookup is
lock-free. After guest threads exist, `g_jit_lock` covers lookup, translation,
publication, and link mutation; stop-the-world generation pinning prevents an
arena from being reclaimed under an executing peer. Direct edges are patched
only after executable bytes and instruction-cache coherence are published.
Pending edges are indexed by target and removed when patched. Indirect edges
probe an identity-keyed IBTC and return to the dispatcher on a miss. Mapping
replacement, executable writes, cache rollover, fork repair, and tier-two
replacement retire or repair every host-code pointer according to their
generation. A block-boundary interrupt poll keeps indefinitely chained guest
control flow observable to signals and stop-the-world requests.

## Rust/native ownership comparison

| Retained capability | Rust/native owner | State |
| --- | --- | --- |
| guest-PC lookup and generation | `cache/cache.c` | implemented |
| pending and resolved direct edges | `cache/relocation.c` | implemented |
| W^X publication before link visibility | `arena.c`, `translation.c` | implemented |
| source-range invalidation and edge undo | `cache/cache.c`, `cache/relocation.c` | implemented |
| dynamic-target identity cache | `executor.c`, `arch/x86_64/run.c` | implemented, one-way |
| per-block interrupt checkpoint | `arch/x86_64/run.c` entry prefix | implemented |
| live remaining-budget checkpoint | `frontend/output.c`, entry prefix | implemented |
| guarded direct-cycle closure | `cache/relocation.c` | implemented |
| x86 declaration that its blocks are guarded | `arch/x86_64/run.c` emission | **missing** |

`cycle_requires_exit` correctly refuses to close an unguarded cycle. The x86
frontend requests `HL_X86_A64_CHECKPOINTS | HL_X86_A64_LIVE_CHAIN`; every
ordinary block entry loads the interrupt flag and compares the live remaining
budget, and each checkpoint subtracts its accounted instruction count from
that budget. Direct targets enter at word two, intentionally preserving both
checks while skipping only BTI and the redundant budget reload. Nevertheless,
`emit_block` left `hl_native_emission.cycle_safe` zero. Consequently every x86
control-flow cycle retained one host dispatcher exit despite already carrying
the required guards.

The repair marks these emissions cycle-safe. It does not weaken the relocation
admission algorithm: mixed cycles containing any future unguarded emission
still retain an exit, frontier saturation still fails closed, conditional
self-loops retain their separately bounded implementation, and invalidation
still restores the original dispatcher edge before retiring a target.

## Evidence contract

The focused two-block x86 cycle test must prove both edges close, a finite
budget still yields at the exact instruction count, an interrupt at entry makes
no guest progress, and invalidating either member restores the surviving edge.
The full native execution tests and warning-strict engine build remain required.

The host is AArch64 Linux, so an x86-64 host-native timing row is physically
unavailable. QEMU is retained only as the x86 semantic/timing control; it is not
reported as native. Performance acceptance compares the same pinned x86-64
guest, CPU affinity, options, checksums, and exact baseline/candidate Rust
executables, alongside the retained C engine.

On CPU 17, nine alternating calls-phase pairs using the same static PIE guest
(`bda1b267...`) produced a baseline median of 21,424 us and candidate median of
20,823 us: 2.81% lower, with the candidate winning all nine pairs. Both reported
checksum `11942663544271968902` and 3,580,664 completed instructions. Public
branch boundaries fell from 61,793 to 161; relocation-cycle refusals fell from
9 to 0. The retained C row was 230 us and QEMU was 1,463 us. This closes a real
avoidable crossing but does not claim x86 parity: instruction lowering and
remaining boundary costs still dominate.

## Immediate checkpoint accounting

The follow-up audit read the complete retained dispatcher and cache path in
`../engine/src/core/dispatch.c` (`run_guest`, `run_block`, and
`block_return`), `../engine/src/translator/cache.c` (`map_host`, `map_put`,
pending-link resolution, IBTC publication, cache rotation, stop-the-world
admission, and fork repair), and the x86 frontend in
`../engine/src/translator/guest/x86_64/{translate.c,emit.c,dispatch.h}`. The
dispatcher owns CPU registration and teardown; the cache owns translation
identity, arena generation, locking, W^X publication, pending edges, and
invalidation. Generated blocks own no locks, allocations, host calls, errno,
or cancellation paths. Fault-capable instructions commit only after their
guard succeeds, while every emitted block reaches an interrupt-visible
boundary.

The Rust owners are `src/arch/x86_64/run.c`,
`src/arch/x86_64/frontend.c`, `src/arch/x86_64/frontend/output.c`,
`src/translation.c`, and `cache/{cache.c,relocation.c}`. Translation identity,
publication, direct and indirect links, invalidation, rollover, and teardown
are implemented. Unlike the retained engine, Rust admits work with an exact
instruction budget. Before every instruction that may fault or fall back, the
frontend accounts the preceding segment so replay cannot double-commit
architectural state.

That accounting formerly materialised the segment length in x17 and then
added or subtracted x17. A decoded block is bounded by
`HL_X86_A64_MAX_INSTRUCTIONS == 64`, so every segment length fits AArch64's
12-bit add/sub immediate. The emitter now uses one immediate instruction; the
old register form remains as a fail-closed path if the frontend bound ever
grows beyond 4095. Live-chain checkpoints shrink from two host instructions
to one, and dispatcher-return checkpoints from four to three. Budget,
partial-result, fault-before-commit, interrupt, cache lifetime, and locking
semantics are unchanged.

Exact-tree warning-strict direct builds and executions of `x86_budget` and the
complete `x86_translation` contract passed. The warning-strict `hl-engine`
suite completed 478 passing tests and two ignored tests; its sole failure was
the independently changing retained option registry (`41` retained entries
versus the test's expected `40`), not native execution. This change has exact
structural evidence (one fewer emitted host instruction at every checkpoint),
but makes no wall-time claim: the existing benchmark artifacts do not retain
per-checkpoint counts, and a timing claim without identical counters would be
false precision.
