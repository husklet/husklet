# Dead and legacy audit: core, backends, shims, docs, and packaging

Date: 2026-07-12. Baseline: `fde47c2b`. This audit documents candidates; it does not authorize deletion.

## Coverage

This partition covers **271/271 tracked paths** not assigned to the scratch, test/vendor/reference, or runtime-
frontend audits. The canonical sorted list is in
[`dead-legacy-core-backends-files.tsv`](dead-legacy-core-backends-files.tsv). Its path-column SHA-256 is
`bb82c9d15196c5fd3a4f37bbe6ea2da2f202098992fad1c2c692d73be2da4a7e`.

| Area | Files |
|---|---:|
| `dd-gpu/` | 38 |
| `dd-images/` | 36 |
| `dd-compositor/` | 27 |
| `dd-shim-vk/` | 26 |
| `dd-display/` | 25 |
| `dd-shim-gl/` | 22 |
| `website/` | 20 |
| pre-audit `docs/` | 17 |
| `dd-gpu-wgpu/` | 15 |
| `dd-shim-cuda/`, `dd-shim-cudart/`, `dd-shim-common/` | 27 |
| root manifests/scripts, `.github/`, `assets/`, and `nix/` | 18 |

Every file was classified after inspecting its content or, for binary assets, format and consumers. Cross-checks
covered workspace/default membership, Cargo targets and module declarations, Make/CI/package wiring, runtime entry
points, environment-variable producers/consumers, tests, exact path/basename references, and Git history. All Rust
modules in this partition have a module or call-site consumer. Cargo examples are implicitly discoverable and are not
called dead solely because no other source names them.

## Remove after one final provenance check

### R1 — Chrome-specific IR reduction tools with baked historical paths

`dd-gpu/examples/replay_ir_sweep.rs` is a large one-off reducer whose defaults name
`target-chrome-codex/release/dd-display`, texture id 514, and a 512x256 Chrome capture. No maintained target,
documentation, or script invokes it. `dd-gpu/examples/trim_ir.rs` and `dump_ir.rs` are related unowned capture tools.
Before removal, verify that the active Chrome plan does not still depend on them; if reduction remains useful, replace
the three tools with one documented, target-independent IR inspection/reduction command and behavioral tests for its
stream transformations. Confidence: **high for removing the baked Chrome tool**, **medium for the two generic tools**.

### R2 — unowned live legacy-compositor example

`dd-display/examples/live_geometry_popup.rs` is a manual raw-wire client for the hand-written compositor. It has no
caller or documented command outside its own header and assumes fixed registry globals and ignored configure serials.
The maintained Smithay popup/input tests cover most of its protocol intent. Run its unique live Cocoa click/placement
journey once; migrate any missing assertion into a Rust protocol test, then delete the example. Confidence: **medium**.

## Verify and simplify

### V1 — retire the hand-written compositor instead of maintaining two complete servers

`dd-display/src/server.rs` remains the default; `DD_DISPLAY_SMITHAY=1` execs `dd-compositor`, and packaging still
allows fallback when the Smithay binary or `libxkbcommon` is absent. Therefore the roughly 5,000-line server is
**legacy but live**, not dead. It duplicates protocol decoding, input, output, dmabuf, popup, and lifecycle behavior.
Finish the Smithay live gates, make Smithay unconditional, remove the environment selector and fallback messages, then
delete the hand-written protocol server and its legacy-only examples/tests together. This is the largest core
maintenance reduction, but deleting it before default cutover would remove the current product path.

### V2 — diagnostic environment branches need owners and expiry

The following behavior switches have only one or two implementation consumers and no stable CLI/config surface:

- `DD_DISPLAY_AUGMENTER`
- `DD_DISPLAY_TEST_TRIANGLE`
- `DD_VK_NO_WL_PRESENT`
- `DD_DISPLAY_SYNC_PRESENT`
- `DD_DISPLAY_MIRROR_INPUT_GEOMETRY`
- `DD_RENDER_NOASYNC`
- `DD_TILE_TRACE` and `DD_TEXTURE_DUMP_DIR`

Some remain useful for fault isolation or Chrome evidence, so reference count alone does not prove death. Give each an
owner, behavioral test/reproduction command, and removal condition. Delete a flag and its alternate branch together
when no active journey uses it; do not retain permanent product forks as undocumented environment variables.

### V3 — duplicated C CUDA frontend is an oracle and source generator, not yet dead

`dd-gpu/cuda/` overlaps the Rust `dd-shim-cuda` and `dd-shim-cudart` products. The Rust registries, ABI comments,
parity tests, and extraction scripts still explicitly treat the C shim as their source/oracle, while install paths can
still build it. Reverse that ownership: make pinned manifests plus Rust behavior the source of truth, retain only a
small independent C ABI client as parity evidence, migrate remaining launcher/install consumers, then remove the C
driver/runtime implementations. `dd-gpu/nvml/` remains separately live because the launcher installs its library.

### V4 — inventory documents contain landed-history maintenance burden

`docs/rendering/SHIM_GL_COMPLETENESS.md` is described as generated but contains extensive hand-maintained phase
history, commit-era closure narratives, and counts that can drift from the build-generated capability table. Replace
it with a generated compact census plus current residuals, or remove it in favor of executable inventory output and
the compact rendering backlog. `SHIM_RUST_ARCHITECTURE.md` is heavily referenced by source comments and still owns
cross-shim design, so it should be condensed only after those references move to stable API-level documentation.

### V5 — default-member exclusions hide stale backend code

`dd-display`, `dd-compositor`, and `dd-gpu-wgpu` are outside normal default builds. This is intentional platform
gating, not deadness, but it allows unused imports, cfg-only branches, stale comments, and broken shared-type consumers
to survive. Keep `make mac-crates` mandatory and add an all-target compile/lint inventory before deleting warnings or
feature-gated modules. Blanket `allow(unused)` in `dd-gpu-wgpu` should be replaced with narrow cfg annotations after
the macOS build is warning-clean.

### V6 — examples and manual scripts require an ownership policy

`tools/dev.sh`, `run_golden.sh`, rendering reproduction scripts, `dd-images/examples/pull_image.rs`, wgpu examples,
and the remaining display/GPU examples are manually invoked entry points. Some have no textual caller because Cargo
discovers examples and users invoke scripts directly. Keep those with a documented command and expected evidence;
port unique correctness behavior into Rust/C tests; remove unowned examples rather than letting them silently compile
only on rare `--all-targets` runs.

## Keep

- `third_party` is covered separately; within this partition, all crate `src` modules are reachable through Cargo
  module graphs or exported ABI entry points.
- Vulkan/GL/CUDA manifests and extraction scripts are build inputs and ABI audit artifacts, not generated clutter.
- Golden PNGs, website images/GIFs, `assets/logo.png`, and the tiny hello rootfs are referenced package/test/site
  assets. Their binary form alone is not removal evidence.
- `website/assets/SCREENCAST.md`, `demo.tape`, and `gen_demo.py` document or generate published media; retain while
  those media remain published, but update commands if branding changes.
- `tools/dev.sh` is a user-invoked bootstrap script despite having no caller. It currently proves the old mac-userland
  builder is still reachable; consolidate that builder before changing the bootstrap.
- Docker/image legacy schema parsing, architecture fallbacks, Vulkan loader compatibility entry points, and deprecated
  protocol events have persisted-input or public-ABI consumers and tests. “Legacy” in those cases means compatibility,
  not dead code.
- Debug logging controlled by the common `DD_SHIM_DEBUG`/`DD_SHIM_STRICT` contract remains shared across products and
  is not equivalent to isolated experimental flags.

## Recommended cleanup sequence

1. Remove the captured scratch repository content identified by the scratch audit; it dominates tracked size.
2. Complete Smithay default cutover, then delete the legacy server and selector as one reviewed change.
3. Replace the C CUDA frontend as source-of-truth before deleting it.
4. Assign owners/expiry to diagnostic flags and remove unowned alternate branches.
5. Consolidate IR-debug examples and compact the hand-maintained GL completeness history.
6. Run all-target/macOS gates before any cfg/platform pruning.
