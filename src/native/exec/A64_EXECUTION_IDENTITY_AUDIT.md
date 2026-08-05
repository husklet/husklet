# AArch64 cold execution-identity publication

## Retained oracle and ownership

The read-only retained audit covered `../engine/src/core/dispatch.c`
(`run_guest`, `run_block`, and `block_return`),
`../engine/src/translator/guest/aarch64/translate.c` (`translate_block`,
`stitch_cond`, and `emit_irq_check`),
`../engine/src/translator/guest/aarch64/stubs.c`
(`emit_prologue`, `emit_spill`, and `emit_chain_exit_from`), and
`../engine/src/translator/cache.c` (`map_host`, `map_put`,
`patch_links_to`, `jit_publish_code`, and `jit_flush_to_fresh`). The retained
dispatcher owns one CPU for the guest thread lifetime. Generated code borrows
it while architectural registers remain live; every public exit spills before
return. The JIT lock serializes threaded emission, code is published before
map, pending-chain, or IBTC ingress, and retired generations remain immutable
until no executing thread pins them. AArch64 host signal capture reconstructs
state from host PC plus published provenance; non-AArch64 hosts do not enter
this code. Linux and Darwin differ in ucontext and W^X mechanisms, not in trace
admission.

Retained bounded traces inline fresh conditional fall-through successors, poll
every in-cache cycle, and deliver pending signals only after a settled spill.
The retained engine has no request instruction budget and does not publish a
per-entry cache identity for partial-budget correction.

Rust adds request-local budget accounting and projected-memory retry. Its
execution gate pins the cache, authority, provenance, and CPU from lookup
through fully spilled return. `trace.c::trace_build` emits interrupt-token and
whole-trace budget admission. `executor.c::run_aarch64` consumes a bit-one
`cpu.indirect_site` identity only when a projected-memory guard fallback must
identify the executed trace and refund its uncompleted suffix. Host fault and
signal callbacks use host-PC provenance and never consume this field. The
aligned form remains separately owned by `indirect.c` for IBTC miss patching.

## Capability matrix

| Capability | Retained C | Rust owner | Status |
|---|---|---|---|
| Registers live across direct edges; spill on public exit | dispatch assembly and AArch64 stubs | `entry.S`, `stub.c` | implemented |
| Bounded conditional fall-through stitching | `translate_block`, `stitch_cond` | `trace.c`, `conditional.c` | implemented |
| Interrupt poll on every cycle | IRQSLIM admission and chain emission | budget header plus relocation `cycle_safe` closure | implemented |
| Whole-request instruction budget and exact suffix refund | no corresponding budget | `stub.c`, `trace.c`, `run_aarch64` | Rust-only, implemented |
| Publish code before executable ingress | JIT lock, publish, map/patch/IBTC order | cache execution gate and atomic provenance publication | implemented |
| Async fault reconstruction without dispatcher state | host PC plus cache provenance | fault scope plus atomic provenance | implemented |
| Identify the executed trace for operand retry | not required | bit-one `indirect_site` plus `hl_native_cache_execution` | implemented, previously over-eager |
| Per-site indirect branch patch identity | retained IBTC site | aligned `indirect_site` in `indirect.c` | implemented and unchanged |

## Mechanism

Previously every dispatcher entry, direct chain, and IBTC hit executed `ADR`,
`ADD #1`, and `STR` before interrupt and budget admission merely in case that
trace later took a projected-memory guard fallback. Ordinary branch, syscall,
yield, interrupt, and successful memory paths never consume the value.

The guard cold stub now publishes its own in-entry address tagged in bit zero
immediately before its fully spilled fallback exit. Any address within the
pinned entry identifies the same immutable cache record. Normal trace entry no
longer performs the three instructions. This preserves local and token
interrupt ordering, reject-before-work budget semantics, conditional refunds,
fault provenance, W^X publication, cycle qualification, and IBTC patching. No
lock, allocation, host call, or destructor is introduced into generated code
or a signal callback.

## Evidence contract

Focused native executor tests cover asynchronous token interruption, bounded
budget returns, warm direct/indirect chains, projected-memory retry, fault-owner
lifetime, cache reset, fork repair, and executable-write invalidation. Warning-
strict build evidence must be recorded from the exact committed tree. Pinned
performance evidence uses one diagnostics-on proof followed by diagnostics-off
timed rows through the typed benchmark matrix; ambient engine options are not
evidence.

On Linux AArch64 CPU 17, the harness first proved seven native runs and then
timed seven quiet rows from harness commit `9fcaf4f1c`. Baseline engine SHA-256
`f05bdb8b1fea904cda27a738d91f2fa64a9df3808954476a35a59c807ef1c774`
had a 1,948 us median; candidate SHA-256
`37595c1a7e40d11e567b22c1b130650225f09d6bab911200bc86ea1bf8fd682d`
had a 1,841 us median, a 5.49% reduction. Both used guest SHA-256
`a8f403013de972313b6c4f3450a82f5e7222690cf05610636155b0c56526d5f9`
and retained runner SHA-256
`0633ed0f914f666f2127ca1b86a4def69eea65c59f7942f8afd66d2b7a6ebc62`.
Every row returned checksum `9349119015121845085`; both diagnostic proofs
reported seven runs, six builds, 18 hits, four branch exits, one fallback,
five yields, 349,589 completed instructions, and one guard fallback. Admission
load was elevated (13.29 for baseline and 11.55 for candidate), so the timing
is focused causal evidence rather than a release claim. The exact removal of
three hot AArch64 instructions per admitted trace is source-verifiable.
