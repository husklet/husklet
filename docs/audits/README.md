# Repository dead and legacy code audit

Audit baseline: `fde47c2b`, 2026-07-12. Scope: every one of the **42,703 tracked paths** at that baseline.
The audit made documentation changes only. Findings are cleanup proposals, not deletion authorization.

## Complete coverage

| Partition | Covered | Total | Detailed evidence |
|---|---:|---:|---|
| scratch snapshots, generated probes, and root artifacts | 40,657 | 40,657 | [`dead-legacy-scratch-2026-07.md`](dead-legacy-scratch-2026-07.md) |
| tests, vendored Smithay, and pinned references | 1,460 | 1,460 | [`dd-tests-third-party-reference-audit.md`](dd-tests-third-party-reference-audit.md), [manifest](dd-tests-third-party-reference-files.tsv) |
| daemon, CLI/client/GUI, terminal, and JIT engines | 315 | 315 | [`dead-legacy-runtime-frontends-2026-07.md`](dead-legacy-runtime-frontends-2026-07.md) |
| GPU/image core, shims, renderers, docs, website, and root build/package files | 271 | 271 | [`dead-legacy-core-backends-2026-07.md`](dead-legacy-core-backends-2026-07.md), [manifest](dead-legacy-core-backends-files.tsv) |
| **Total** | **42,703** | **42,703** | no unassigned tracked paths |

“Read” means source/text content was inspected; binary assets and the 40,546-file captured rootfs were inspected by
Git blob metadata, file type, package/rootfs structure, consumers, and representative content. Treating every copied
system library byte as handwritten source would add no reachability evidence. Each partition documents its method and
confidence.

## Highest-value cleanup

1. **Remove the captured scratch island after migrating two checkpoint probes.** `scratch-erl/rootfs/**`, copied
   engines, logs, dumps, and translation-cache captures account for 40,657 tracked paths and about 2.6 GB of
   uncompressed blobs. They have no maintained consumer. Replace any required Erlang reproduction with a recipe and
   small behavioral fixture; port `.ckpt_stress` behavior into Rust before deleting it.
2. **Finish Smithay cutover, then delete the live legacy compositor.** `dd-display/src/server.rs` is duplicated
   maintenance, but remains the default today. Make Smithay unconditional and pass live gates before removing the
   selector, fallback, legacy server, and legacy-only examples.
3. **Complete the already-requested benchmark removal atomically.** The 18-file benchmark island is separate from
   correctness coverage. Remove its Make/Cargo exports, invariant assertions, and stale rebrand references together;
   keep correctness-matrix performance modes that exercise behavior.
4. **Replace C CUDA oracle ownership before deleting it.** Rust shims still generate manifests and parity claims from
   `dd-gpu/cuda`. Move truth to pinned manifests and Rust behavior first; retain a small independent C ABI client.
5. **Consolidate duplicate macOS image builders.** `mac-userland.sh` is older but still called by `tools/dev.sh` and a
   scenario. Migrate those callers to the maintained builder before removal.

## Deep symbol and branch passes

- [`deep-audit-cross-cutting-2026-07.md`](deep-audit-cross-cutting-2026-07.md) — empty Cargo features, compiler-
  confirmed dead archive helper, and environment-flag ownership.
- [`jit-deep-audit-a-2026-07.md`](jit-deep-audit-a-2026-07.md) and
  [`jit-debug-measurement-bundle-wave-d-2026-07.md`](jit-debug-measurement-bundle-wave-d-2026-07.md) — unity-build
  reachability, JIT fallback flags, and the abandoned ARM-B1 instrumentation bundle. The strongest zero-production-
  behavior cut is `IBPROF`/`VDBETRACE`/`VTHITCOUNT`/`CTXDISP`: no maintained producer, yet `IBPROF` alone reserves an
  estimated 60.9 MiB of static BSS while disabled.
- [`render-stack-deep-audit-wave-b.md`](render-stack-deep-audit-wave-b.md) and
  [`render-stack-deep-audit-wave-e.md`](render-stack-deep-audit-wave-e.md) — loader/ABI false positives, exact dead
  wgpu helper, compositor migration, RAII fields, diagnostics, and cfg-aware dependency findings.
- [`deep-runtime-frontends-wave-c-2026-07.md`](deep-runtime-frontends-wave-c-2026-07.md) and
  [`deep-runtime-frontends-wave-f-2026-07.md`](deep-runtime-frontends-wave-f-2026-07.md) — exact GUI symbols, dormant
  terminal hooks, scenario parser false-green paths, CI/package drift, and dependency/target verification.

## Small high-confidence cleanup

- Relocate or delete `dd-jit-darwin/docs/CHECKPOINT.md`; it describes nonexistent speculative code and has no owner.
- Correct the stale `winit + wgpu` statement in `dd-term-core/Cargo.toml`; the shipped terminal uses GTK/GSK/VTE.
- Verify and retire `dd-gpu/examples/replay_ir_sweep.rs`, whose defaults are tied to one historical Chrome capture.
- Migrate the unique live Cocoa assertion from `dd-display/examples/live_geometry_popup.rs`, then remove the unowned
  fixed-global legacy-compositor client.
- Condense `SHIM_GL_COMPLETENESS.md` to generated current inventory; remove landed-history prose already preserved by
  Git.

## Do not delete based on appearance

- Vendored Smithay is a direct path dependency; disabled upstream backends are feature-gated vendor source.
- ABI compatibility fields, deprecated protocol events, persisted workspace/image formats, and architecture-specific
  JIT engines have observable consumers or regression tests.
- Golden images, guest binaries, manifests, registry inputs, and website media are intentional binary fixtures/assets.
- Cargo examples and manual scripts are discoverable without textual callers. Require ownership or migrate their
  unique behavior, but do not infer death from `rg` absence alone.
- Diagnostic environment variables need owner/reproduction/expiry review. Remove the variable and alternate branch
  together only when no active journey depends on them.

## Rules for cleanup patches

1. Re-check reachability on the then-current tree; this audit is a baseline, not a permanent fact.
2. Remove consumers, flags, documentation, packaging, and tests atomically with the dead implementation.
3. Preserve useful behavior by moving it into Rust/C behavioral tests—not source-string tests.
4. Run appropriate all-target, macOS, packaging, and live gates before pruning cfg/platform paths.
5. Update the applicable manifest and this index after each accepted cleanup so completed candidates disappear.
