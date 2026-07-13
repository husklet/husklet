# Dead and legacy audit: runtime frontends

Date: 2026-07-12. Baseline: `fde47c2b`.

## Coverage

This audit covers **315/315 tracked files** returned by:

```sh
git ls-files dd-daemon dd-cli dd-client dd-gui dd-term-core dd-jit dd-jit-darwin
```

| Tree | Covered | Total | Classification |
|---|---:|---:|---|
| `dd-daemon/` | 117 | 117 | daemon library/binary, Docker-compatible handlers, runtime/build/network/image code and tests |
| `dd-cli/` | 15 | 15 | `ddcli` binary, command parser, daemon/workspace/direct-engine launch plumbing |
| `dd-client/` | 14 | 14 | shared Docker API client and view models |
| `dd-gui/` | 47 | 47 | two declared binaries, GTK UI, packaging assets and macOS build/diagnostic scripts |
| `dd-term-core/` | 18 | 18 | public terminal core modules, integration test and three Cargo examples |
| `dd-jit/` | 18 | 18 | public runtime facade, container builder/runtime modules and example |
| `dd-jit-darwin/` | 86 | 86 | backend/build manifests, Rust launch API, C engines, translators, jail and tests/docs |

The exhaustive file-level partition is: **279 runtime/source files**, **9 dedicated test/support files**, **12 build/manifests**, and **15 assets/readmes/scripts/other files**. Embedded `#[cfg(test)]` modules remain classified with their owning source file. Every tracked path falls into exactly one row and one partition; there were no unclassified files.

Cross-checks included every `Cargo.toml` target/dependency, Rust `mod` declarations, root Make targets, package scripts, environment-variable producers/consumers, repository-wide path references, and tests. “No reference” below means an exact repository `rg` search excluding this audit.

## Remove or relocate

### R1 — speculative checkpoint design stored inside the shipped backend

**Candidate:** `dd-jit-darwin/docs/CHECKPOINT.md`.

Evidence: the document explicitly says “No checkpoint/restore code exists yet”; no code, Cargo target, Make target, test, or other documentation references it. It contains volatile source line numbers and a proposed on-disk format, so ordinary engine refactors make it stale without affecting behavior. Move any still-wanted product idea into the current engine backlog, then delete this crate-local design snapshot. Confidence: **high**.

### R2 — stale architecture statement in the terminal core manifest

**Candidate text:** `dd-term-core/Cargo.toml` says the GPU shell is “winit + wgpu”.

The only shipped shell is `dd-gui`'s explicit `dd-term` binary, implemented with GTK4/GSK/VTE (`dd-gui/Cargo.toml`, `dd-gui/src/bin/term.rs`). No winit dependency exists in the assigned trees. Correct the comment; do not remove `dd-term-core`. Confidence: **high**.

## Verify before removal

### V1 — blanket dead-code suppression across the old GTK view layer

`dd-gui/src/ui/mod.rs` and every file under `dd-gui/src/ui/views/` use crate/file-level `allow(unused_imports, dead_code)`. The modules are all declared, but broad suppression prevents the compiler from distinguishing active view code from abandoned widgets after the newer `dd-gui/src/bin/term.rs` manager UI grew. Remove the blanket attributes on a macOS GTK build, then classify individual warnings. Do not delete the files based on Linux default-member builds because `dd-gui` is excluded there. Confidence that dead members exist: **medium**; confidence in deleting whole modules: **low**.

### V2 — unowned diagnostic and screenshot environment surface

The GUI has `DD_SHOT*`, `DD_TERM_SHOT*`, `DD_TERM_VIEW`, `DD_TERM_*_PANE`, `DD_TERM_TABS`, `DD_TERM_SPLIT`, `DD_TERM_TYPE`, and debug-log switches. They are absent from the user CLI and are mostly driven only by `dd-gui/mac/shot.sh` or manual use. Establish a documented test-hook allowlist; remove hooks with neither a scenario nor a packaging invocation. Keep `DD_SHOT*` while `shot.sh` exists. Confidence: **medium**.

### V3 — engine A/B flags retained indefinitely

`dd-jit-darwin/src/spawn_config.rs` forwards numerous rollback/diagnostic flags (`NOFUTEXQ`, `NOSMCHASH`, `NOSTEAL1617`, `W4_NOOPENCACHE`, `NOLSE`, `NOSTITCH`, and others) into C paths. Several intentionally select explicitly “legacy” algorithms. Most external references are inventory text or narrow regression fixtures, not production launch policy. Give each flag an owner, failing regression, and removal date; remove the fallback implementation once its replacement has survived the full matrix. Do not remove them en masse: pcache keys incorporate several translation flags, and tests actively use some (`DDX_FORCE_BASE_COLLIDE`). Confidence: **medium**.

### V4 — legacy macOS userland builder duplicates the newer image builder

`dd-gui/mac/mac-userland.sh` and `dd-gui/mac/mac-image.sh` both build native macOS images. The former is older/local-store-oriented; the latter is the Makefile's `mac-image` implementation and packs explicit base/dev closures. The old script is **still invoked** by `tools/dev.sh` and `dd-tests/scenarios/macos-container.sh`, so it is not dead. Migrate those two callers to `mac-image.sh` (preserving the lightweight/local behavior or adding a variant), then remove the duplicate. Confidence: **high after migration**, **unsafe now**.

### V5 — unreferenced Cargo examples

`dd-jit/examples/run_container.rs` and the three `dd-term-core/examples/*.rs` files have no Make/package/scenario callers. Cargo discovers them implicitly, so absence of textual references is not proof of death. Require `cargo test --examples` or a documented manual journey; otherwise move their unique coverage into Rust tests and delete them. Confidence: **low-to-medium**.

## Keep

- `dd-daemon/src/containers/exec/{start,attach}.rs` and lifecycle query fields marked `allow(dead_code)` are Docker wire-compatibility fields. Deserialization compatibility is observable even when dd ignores the value.
- `dd-term-core/src/workspace.rs` legacy tab-row loader has direct regression coverage and preserves existing user configuration.
- `dd-daemon`'s legacy Dockerfile `ENV K V` parser and flat-rootfs/legacy-container branches are compatibility paths with tests or persisted-state inputs, not unreachable code.
- `dd-gui/mac/{mac-image.sh,shot.sh}`, `dd-gui/package/`, and signing placeholders are wired by Make/package flows. The signing `.gitignore`/README intentionally reserve an out-of-tree secret location.
- `dd-jit-darwin` architecture-specific targets are selected by `build.rs` and runtime guest architecture; host-incompatible code cannot be judged from the Linux Rust default build.
- `DDJIT_DIR`, `DD_ENGINE_DIR`, `DD_DAEMON_BIN`, display/GPU socket variables, guest config variables and pcache variables have live producers and consumers across CLI/JIT/engine boundaries.

## Recommended order

1. Relocate/delete R1 and correct R2.
2. Run the macOS GUI build without blanket warning suppression and produce a symbol-level V1 deletion patch.
3. Consolidate the two macOS image builders, changing scenarios before deleting the old script.
4. Turn diagnostic/A-B variables into an owned allowlist; delete only flags whose fallback and dedicated regression can be removed together.
5. Add an examples gate or fold useful example behavior into tests.

This is a documentation audit only. It does not assert runtime behavior from source-string tests and authorizes no deletion by itself.
