# AArch64 call and return fallback audit

Baseline: `067f91763415c54ef0f5d0bc5f7226bade17523b`. The measured calls workload
completed 9,422 instructions with 180 fallback boundaries (152 classified
other and 11 control), 572 builds, 560 branch boundaries, and 61 IBTC misses.

## Retained oracle

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

## Capability map

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
