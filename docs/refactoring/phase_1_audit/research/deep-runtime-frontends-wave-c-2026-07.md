# Runtime/frontends deep audit — wave C (2026-07)

## Scope and method

This is a documentation-only second pass over every tracked file in `dd-daemon/`, `dd-cli/`,
`dd-client/`, `dd-gui/`, `dd-term-core/`, `dd-images/`, `dd-tests/`, plus `Makefile`, the three GitHub
workflows, `nix/`, and `website/`: **1,370/1,370 tracked files**. The pass cross-checked Cargo targets
and dependencies, module/public exports, CLI parsing and dispatch, environment readers, Make/CI/package
entry points, test registries, executable fixtures, serde/wire compatibility fields, debug hooks, and
blanket lint suppressions. Vendored/reference trees were treated as compatibility inputs, not candidates
for opportunistic pruning.

No runtime file is changed by this audit. “Remove” below means a proven isolated maintenance island;
anything with a public, serialized, packaging, or performance-path consequence is explicitly gated by a
migration or measurement.

## High-confidence actions

### Remove the dedicated benchmark island atomically

The standalone benchmark surface is not correctness coverage. Its complete ownership boundary is
`Makefile`'s `bench` target, `dd-tests/src/bin/bench.rs`, `dd-tests/src/bench_gates.rs`, the 16 sources in
`dd-tests/guests/bench/`, the `bench_gates` export in `dd-tests/src/lib.rs`, and the three benchmark-only
assertions in `dd-tests/tests/gate_invariants.rs`. Remove those consumers together, then delete stale
`BENCH_N`, `BENCH_K`, generated-column and auto-binary references from rebrand documentation.

Keep the correctness runner's `PERF`/`PERF_N` mode and its timeout/success invariants: those re-run real
test cases and can expose performance regressions without maintaining a separate workload corpus. Also
keep `dd-tests/tools/fclient.c` until forkserver test ownership is decided; its one-shot protocol client
is useful independently of its optional timing mode.

### Fix scenario argument validation and host-specific defaults

`dd-tests/src/bin/scenarios.rs:126-131` defaults `DD_IMAGES` to the developer-specific absolute path
`/Users/x/dd/poc/images`. This makes an unconfigured checkout depend on one machine. Derive the default
from the repository/state layout used by the daemon, or require `DD_IMAGES` with an actionable error.

The same parser silently accepts an invalid `-t/--target` value: `parse_target` returns `None`, and
`unwrap_or(targets)` preserves the previous selection. That is a false-green risk analogous to the
already-rejected empty category selection. Return exit 2 for a missing or unknown target and cover it
with a Rust parser test. A small typed/clap parser would also eliminate the current hand-written
missing-value edge cases.

### Narrow blanket GUI lint suppressions after the macOS build gate

Twenty-plus GUI modules begin with `#![allow(unused_imports, dead_code)]`, including every view and most
widget/dialog modules. This prevents the compiler from distinguishing intentionally staged UI from
orphaned components. Do not mass-delete symbols from Linux evidence because `dd-gui` is deliberately
outside default members and requires the macOS GTK/Nix environment. Instead run `cargo check -p dd-gui
--all-targets` in that environment, remove each module-level allowance, and retain only item-level
annotations with an ownership comment. This is the prerequisite for a trustworthy GUI call-graph cut.

### Correct stale build/package contracts

- `dd-term-core/Cargo.toml` says the GPU shell is `winit + wgpu`; `dd-gui/Cargo.toml` and the live binary
  define `dd-term` as GTK4/GSK. Update the comment; it currently sends dependency work toward an obsolete
  architecture.
- `Makefile` defines `scenarios-prune` but omits it from `.PHONY`. Add it so an identically named file
  cannot suppress the maintenance action.
- `release.yml` publishes instructions using `dd install` / `dd app`, while the declared CLI binary is
  `ddcli`. Preserve this as an explicit rebrand migration item; do not silently change only one surface.
- `release.yml` and `smoke.yml` install floating stable Rust rather than honoring the repository toolchain
  pin. Make CI consume the pin so local and release compiler behavior cannot drift.

## Migration-required surfaces (keep for now)

### CLI and persisted workspace compatibility

All `ddcli` subcommands and workspace options reach dispatch code or are locked by parser tests. In
particular, optional `--arch`, value-optional `--cuda`/`--gui`, `--slot`, `--cwd`, and the external-image
shorthand are compatibility contracts with the GUI and persisted workspaces. Removal or renaming belongs
in the rebrand plan with aliases and a deprecation window, not dead-code cleanup.

The `--cuda` help still describes presence-only simulation while the workspace launcher now wires the
GPU provider. Reconcile the wording against current CUDA behavior; documentation drift is safer to fix
than changing the option.

### Environment/debug hooks

Production integration variables (`DDOCKERD_SOCK`, `DD_IMAGES`, `DD_STATE`, `DD_VOLUMES`,
`DD_DAEMON_BIN`, `DD_CLI_BIN`, `DD_DISPLAY_SOCK`, `DD_GPU_EXEC_SOCK`, and `DD_ENGINE_DIR`) have concrete
daemon/CLI/GUI consumers and must remain through rebrand compatibility aliases.

The large `DD_TERM_*` and `DD_SHOT*` families are concentrated in screenshot/demo setup in
`dd-gui/src/bin/term.rs` and `dd-gui/src/main.rs`. They are not evidence of product branches, but they do
carry maintenance cost. Inventory them in one testing document, move screenshot setup behind a dedicated
test-support module/feature, and then remove any variable with neither a Rust test nor a checked-in
screenshot script. Do not remove them blindly: several select otherwise hard-to-reach UI states.

`DD_DEBUG`, `CRASHDBG`, `PERF`, and `DD_SCEN_PROFILE` are diagnostic/test ownership points. Keep them,
but centralize their semantics so “set to any value” versus “equals 1” is not inconsistent across crates.

### Wire and public API fields

The item-level `#[allow(dead_code)]` fields in daemon exec/attach/container query structs are explicitly
deserialized Docker API compatibility fields. They are correct exceptions and must not be removed merely
because handlers ignore them. Likewise, `dd-client` model exports and `dd-images` serde structures cross
crate or persisted/wire boundaries; require consumer and serialized-fixture migrations before pruning.

## Dependency and binary conclusions

The seven manifests have no obvious dependency that can be removed safely from textual evidence. Their
declared libraries/binaries all have workspace, Make, GUI, or scenario consumers. `dd-client`'s bollard,
stream, and byte dependencies form its Unix-socket Docker façade; `dd-images`' three dependencies cover
serialization and digesting; daemon async/http dependencies map to its server stack. Run platform-aware
`cargo machete`/`cargo udeps` only as supporting evidence, because macOS-target and feature-gated uses can
look unused on Linux.

`dd-images/examples/pull_image.rs` is an intentional manually runnable API example, not an installed
binary. Keep it unless the public pull API itself is retired. The `dd-tests` `scenarios` binary is wired
by Make targets and CI-facing workflows and is correctness coverage, not a benchmark.

## Test ownership and retained performance paths

Keep guest fixtures registered by the Rust case/scenario graph, compliance suites invoked through their
relative scripts, protocol clients, and prebuilt architecture fixtures required by execution tests.
Executable-bit or lack of an exact full-path reference alone is insufficient deletion evidence: shell
suites commonly use relative discovery and Cargo discovers bins/examples by convention.

After benchmark removal, validate with Rust-owned gates:

1. `cargo check -p dd-tests --all-targets`;
2. `cargo test -p dd-tests --test gate_invariants` and the normal Rust suite;
3. `make test-ci` for engine-lane ownership;
4. macOS/Nix `cargo check -p dd-gui --all-targets` before any GUI symbol pruning.

Preserve production fast paths and the correctness matrix's optional timed reruns. This audit found no
performance implementation whose deletion is justified by the benchmark cleanup.

## Second-pass disposition

- **Immediate isolated removal:** dedicated 18-file benchmark implementation plus its exact consumers.
- **Immediate documentation/build hygiene:** stale terminal architecture comment, missing `.PHONY`, pinned
  CI toolchain, and release-command/rebrand reconciliation.
- **Fix with Rust tests:** scenario target parsing and machine-specific images default.
- **Measure/migrate first:** GUI lint-hidden symbols, screenshot/debug env hooks, CLI/rebrand aliases,
  serde/public APIs, and Docker wire fields.
- **Keep:** compositor/rendering correctness tests, scenario/compliance coverage, examples with live public
  APIs, production performance paths, and vendored/reference compatibility sources.
