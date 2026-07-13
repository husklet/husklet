# Deep audit: cross-cutting flags, features, warnings, and ownership

Date: 2026-07-12. This is a continuing documentation-only pass after the complete file inventory.

## Immediate cuts with no runtime effect

### Empty `dd-gpu` features `metal` and `cuda`

`dd-gpu/Cargo.toml` declares `metal = []` and `cuda = []`, while comments claim they gate direct Metal and CUDA
executors. No `cfg(feature = "metal")`, `cfg(feature = "cuda")`, dependency feature edge, CI command, Make target,
or package command consumes either feature. The real Metal executor is in `dd-display`; the wgpu/Metal executor is in
`dd-gpu-wgpu`; no native CUDA-host executor exists. Removing these two empty feature names and the false comments
changes neither generated code nor dependency resolution for any repository target. Compatibility risk is limited to
an undocumented external user passing a no-op feature name; this workspace is not published as a stable crate.
Confidence: **high**. Performance risk: **none**.

### Unused duplicate archive traversal helper

`dd-images/src/image/archive/mod.rs::tar_members_contained` has no caller and triggers `dead_code` during
`cargo check --workspace --all-targets`. A separate, tested implementation in `dd-daemon/src/util/paths.rs` is used by
daemon build handlers. The unused function was added as hardening but never wired into `dd-images` load/import.
Choose one of two explicit outcomes: call it from every applicable extraction boundary and add adversarial tests, or
delete the duplicate. Leaving security-looking dead code creates false confidence. Deleting the currently unreachable
function has no runtime or performance effect; wiring it is preferable if archive load/import still extracts with
`tar`. Confidence that the function itself is dead: **compiler-confirmed**.

## Flags requiring ownership or removal

Literal environment-call inventory found several single-site alternate behaviors with no stable CLI/config producer:

| Flag | Sole/narrow behavior | Safe removal condition |
|---|---|---|
| `DD_DISPLAY_AUGMENTER` | re-enables legacy augmenter global | no maintained protocol journey needs the global |
| `DD_VK_NO_WL_PRESENT` | disables Vulkan Wayland presentation | replace fault isolation with an injected Rust test |
| `DD_DISPLAY_TEST_TRIANGLE` | substitutes diagnostic triangle rendering | retain only if a documented live hardware gate drives it |
| `DD_DISPLAY_MIRROR_INPUT_GEOMETRY` | alternate Cocoa input geometry | remove after one canonical coordinate model has live coverage |
| `DD_DISPLAY_SYNC_PRESENT` | forces synchronous Metal presentation | remove after async ordering/failure tests replace manual A/B use |
| `DD_RENDER_NOASYNC` | disables asynchronous rendering | remove with obsolete fallback after stress/parity proof |
| `DD_TILE_TRACE`, `DD_TEXTURE_DUMP_DIR` | Chrome texture instrumentation | keep only while the active Chrome plan names a reproducible use |

These branches are not declared dead solely by low reference count. Each should gain an owner, a command/test, and an
expiry criterion. Removing an unowned flag must remove its alternate implementation and stale launcher forwarding in
the same change. Shared operational contracts such as `DD_GPU_EXEC`, `DD_SHIM_DEBUG`, `DD_SHIM_STRICT`, engine
container configuration, packaging/signing inputs, and golden-update controls are not candidates.

## Build evidence and blind spots

`cargo check --workspace --all-targets` reached project compilation but stopped because the local environment lacks
`pkg-config`/GTK system metadata. Before that failure it produced the project warning above for
`tar_members_contained`; remaining warnings were in vendored Smithay. This command is insufficient for macOS-only
renderer and GUI dead-code claims. Those require `make mac-crates` and a GTK/Nix all-target build without blanket lint
suppression.

### Stale rebuild warning in the contributor guide

`docs/AGENTS.md` says the C engine's included files are not all tracked by `build.rs` and therefore require a manual
clean. Current `dd-jit-darwin/build.rs::rerun_dir` recursively emits `cargo:rerun-if-changed` for every `.c` and `.h`
under `src/runtime`, as well as the entitlements file. The warning describes an old build graph and encourages costly
full cleans. Verify by touching a nested included C file and observing the engine rebuild once; then replace the stale
warning with the narrower rule for any genuinely untracked non-C/H generator input. This is documentation cleanup
with no compatibility risk and can improve developer iteration speed.

### Stale default-member statement

The same guide says `dd-display`, `dd-compositor`, and `dd-gpu-wgpu` are all excluded from workspace
`default-members`. Current root `Cargo.toml` includes `dd-display` in `default-members`; only the compositor, wgpu
backend, GUI, and shim-specific products remain outside that list. This mismatch can cause reviewers to assume display
code is never checked by a plain workspace build. Update the table directly from the manifest and keep the macOS gate
warning only for paths actually excluded or cfg-empty on the current host. Documentation-only change, zero runtime
risk.

## Dependency and target observations

- `dd-gpu`'s `runtime` feature is live: `dd-cli` enables it and `dd-gpu/src/lib.rs` gates the integration seam.
- Cargo examples are implicit targets. Lack of a textual caller is ownership evidence, not reachability proof.
- Crates excluded from `default-members` are still workspace/package targets; removal requires package and macOS
  build evidence, not a Linux default build.
- Empty features, stale manifest descriptions, and blanket `allow(dead_code)` are valuable audit signals because they
  can conceal nonexistent architectures or abandoned members, but only the empty-feature case above is already
  behavior-neutral enough for immediate deletion.

## Next proof passes

1. Run a macOS/Nix all-target warning inventory and map every suppressed GUI symbol to a callback/template consumer.
2. Extract the C engine preprocessor/call graph for fallback flags and compare compiled symbol sets under both values.
3. Exercise diagnostic flag branches under their last known reproductions; delete branches that no longer change
   observable output or failure diagnosis.
4. Audit direct dependencies with compiler/build-script awareness; do not remove crates based on source-token search
   when proc macros, derives, build scripts, or platform cfgs consume them.
