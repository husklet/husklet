# Phase 1 cleanup execution plan

Use the [disposition ledger](disposition-ledger.md) IDs. One patch may combine only candidates with the
same owner, proof surface and rollback boundary.

## Batch 1 — documentation and compiler-proven private cuts

C03–C11, C19 and C32 are the first review set. Separate JIT C, GUI mac-only, Vulkan shim, wgpu, images and
generated GL into owner-specific commits. “Small” does not mean one mixed commit. Run compiler reachability
again on the exact tree immediately before deletion.

## Batch 2 — remove standalone islands

C02 benchmark removal includes binary, gates, fixtures, Make/Cargo targets, CI and rebrand references.
C01 scratch removal follows only after unique behavioral probes and provenance are captured. Repository
size and tracked-path reduction are acceptance evidence; they do not replace migrated behavior tests.

## Batch 3 — abandoned JIT experiments and memory shape

C13 removes one complete experimental subsystem. First record linked map/size, then remove state, tables,
parsing, forwarding, translation/dispatcher hooks, pcache poison/identity and comments together. C14/C15
are independent allocation refactors: preserve limits and feature semantics, compare hot syscall paths and
disabled RSS/BSS.

## Batch 4 — behavior-preserving centralization

C23 shares cold constants/contracts first, then transforms/blend/input normalization. Cross-path golden
vectors are captured before replacement. Per-pixel helpers require optimized-code evidence that extraction
did not introduce calls, bounds checks or allocations.

## Batch 5 — migration-dependent removals

C17 terminal hooks, C20 path resolver, C22 legacy compositor, C24 augmenter, C28 migration reader and C35
old image builder wait for their named destination/trace/window. The implementation PR links the migration
commit and deletes old caller/config/docs in the same series.

## Batch 6 — correctness work discovered by cleanup

C25, C27, C30 and C31 are fixes, not deletion metrics. C08 also becomes a fix if the unused archive safety
helper reveals an unprotected extraction path. These land with negative behavioral tests before any dead
scaffolding is removed.

## Required gate matrix

| Owner/surface | Minimum gate |
|---|---|
| Rust headless crates | package all-target tests and workspace default tests |
| JIT C/runtime | three unity builds, Rust launch tests, required guest lanes and affected feature modes |
| daemon/images | offline API/archive/state tests plus affected real-image quick journey |
| guest shims | host Rust tests, both guest target builds, independent C ABI clients |
| display/compositor | headless wire/protocol/pixel tests and mac live parity where behavior touches Cocoa/Metal |
| wgpu/GUI/package | mac Nix build/tests, app view smoke and bundle smoke when packaging/deps change |

No gate passes through a source-substring assertion. Required unavailable platforms fail CI at preflight;
developer-only local targets may report an explicit skip.

## Ledger maintenance

Before merge, update the candidate row to `landed <commit>` or remove it from the active table and append a
short completed index. Re-run searches for all names and consumers. If evidence contradicts the row, change
its disposition rather than forcing the proposed cleanup.
