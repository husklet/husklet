# Render-stack deep audit — wave E: symbols, allowances, dependencies, and flags

Audit date: 2026-07-12. Documentation only.

## Verification boundary

Scope continues wave B across `dd-display`, `dd-compositor`, `dd-gpu`, `dd-gpu-wgpu`, and every `dd-shim-*` crate. Searches covered all target/cfg source, tests, examples, Cargo manifests, root build/package entry points, generated ABI surfaces, and repository environment consumers. External guest/package consumers are explicitly possible; a one-site environment read is not proof of zero use.

Compiler proof required for any accepted cut:

```sh
cargo check -p dd-gpu --all-targets
mac bash -lc 'cd <worktree> && cargo check -p dd-display -p dd-compositor -p dd-gpu-wgpu --all-targets'
cargo check -p dd-shim-common -p dd-shim-cuda -p dd-shim-cudart -p dd-shim-gl -p dd-shim-vk --all-targets
```

The macOS command must use an isolated target directory and the repository's libxkbcommon setup. Run relevant behavioral tests after compilation; warning disappearance alone is not behavioral evidence.

## Exact `allow(dead_code|unused*)` inventory

| Site | Finding | Decision |
|---|---|---|
| `dd-gpu-wgpu/src/shader.rs:16`, `legacy_msl` | Only production-tree occurrence is its definition; only caller is `legacy_msl_is_not_spirv` in the same file's test module. No cfg, example, display, compositor, shim, Make, or Cargo consumer. | **Delete now, high confidence:** function, allowance, and dedicated test assertions. Keep `spirv_to_wgsl`'s non-SPIR-V `Ok(None)` fallback separately. |
| `dd-gpu-wgpu/src/shader.rs:50`, `glsl_to_wgsl` | Called only by its unit test today. Comment claims future forwarded-GLES use; current shim path does not call it. Naga's `glsl-in` feature is also used by macOS tests that mint SPIR-V independently. | **Verify/migrate:** either wire it into negotiated GLSL payload handling or remove function/test. Do not remove Naga `glsl-in` based on this symbol alone. |
| `dd-gpu-wgpu/src/backend.rs:323`, `TexEntry.format` | Earlier dead-code lead was false. Reads occur in flip scratch comparison, destination clear/blit pipeline choice, render-target creation, validation, and execution (for example lines ~1018, 1044, 1664, 1673, 1760, 1789, 1961, 2143). | **Keep field. Delete stale allowance/comment only** after macOS all-target compilation proves no cfg-only warning. Removing the field breaks format-correct pipelines. |
| `dd-display/src/present_cocoa.rs:255`, `Win.window` | Actively read for size, pointer-to-surface routing, content view, scale, close/focus, cursor, and screen refresh; additionally owns the Objective-C object. | **Keep field.** The allowance appears stale in current macOS code; remove annotation only after mac compile. RAII false positive. |
| `dd-compositor/src/handlers/text_input.rs:63`, `TextInputState.global` | No ordinary read, but `GlobalId` represents retained advertised-global identity/lifetime. | **Keep.** Ownership/protocol false positive unless Smithay documentation proves dropping it is harmless and no future removal is needed. |
| `dd-display/src/server.rs:403`, legacy `input_region` | Written through `set_input_region` pending→commit, never read by legacy hit testing. This is real missing behavior, not harmless dead state: deleting it preserves the bug and loses committed protocol state needed for migration/parity. | **Keep until behavior migrates** or legacy server is retired. Do not call a missing consumer a safe cleanup. |
| `dd-gpu/src/software.rs:42`, `ShaderModule::Spirv(Vec<u32>)` payload | Variant is constructed in two shader-create paths; executor matches it but never reads bytes and truthfully cannot run SPIR-V. | **Medium-confidence memory cut:** change to a unit variant only if shader diagnostics/replay never need bytes. Test create/destroy, unsupported execution, replay rollback, and memory accounting. Keep variant semantics. |
| `dd-shim-gl/src/glconst.rs:4`, module-wide allowance | Constants are an ABI/protocol vocabulary referenced irregularly by generated exports and tests. Module-wide suppression hides genuinely stale constants. | **Refactor, not bulk delete:** remove blanket allowance in a branch, compile all generated shim targets, then classify individual warnings against GL ABI generation. |

Inventory total: eight allowance sites. Immediate symbol deletion set is exactly `legacy_msl` plus its dedicated test. Immediate annotation-only cleanup candidates are `TexEntry.format` and `Win.window`; annotations have no runtime effect but require macOS proof.

## TODO and future-only state

### Keep: ABI/layout or live state

- CUDA/CUDART `reserved*` fields are `#[repr(C)]` ABI layout in runtime/device property structures and mirror C headers. Never remove without size/offset assertions against CUDA headers and guest binaries.
- GL `reserved` buffer/texture/sampler/query names implement real generate-then-bind object lifecycle. They are read throughout `gles.rs` and covered by ES3 object tests.
- Vulkan reserved presentation IDs and wgpu present-target maps prevent guest collision with host output resources; tests explicitly exercise collision behavior.
- `dd-shim-vk` pending submission states and command-pool ownership are live correctness state even where reset/free semantics remain incomplete.

### Migration debt, not safe deletion

- `dd-display/src/present_cocoa.rs` mixed IOSurface root overlay TODO: children reach the presenter but are not yet GPU composited. Deleting overlay plumbing would regress the intended bridge; implement and test instead.
- `dd-compositor/src/lib.rs` retired zero-copy buffer queue awaits completion tokens. Removing it risks premature `wl_buffer.release`; it is a safety retention path.
- Vulkan rasterization/MSAA/blend pointer placeholders document state currently ignored by lowering. Convert ignored pointers to explicit capability rejection before removing comments/state, otherwise applications receive false support.
- Gesture/tablet/content-type seams are advertised protocol policy. Future host bridges are not evidence of deadness while clients can bind the protocols; remove only with advertised-global/version changes.

### Comment cleanup

Numerous “future/later increment” comments describe actual unsupported behavior. Replace them with typed failure/capability references and ledger ownership. Remove historical prose only after the behavior is implemented or rejected; otherwise comments are the only warning that a path is partial.

## Cargo dependency and feature audit

| Dependency/feature | Evidence | Decision |
|---|---|---|
| `dd-gpu` optional `dd-jit`, feature `runtime` | Keeps pure IR/wire/software core dependency-light; CLI/runtime integration enables it. | Keep split. Validate with default and `--features runtime`. |
| Smithay `default-features=false`, `wayland_frontend` | Avoids DRM/GBM/X11/udev while supplying compositor protocols. | Keep. Manual vendor pruning is unsafe. |
| wgpu/wgpu-hal/pollster | Direct backend adapter/device and raw Metal interop callers. | Keep. |
| Naga features | `spv-in`/`wgsl-out` serve production translation; `glsl-in` and `spv-out` serve shader tests/current helper; `msl-out` and `wgsl-in` require separate symbol-level confirmation. | Candidate feature reduction only after `cargo check --all-targets` and all mac shader tests. Measure compile size/time, not runtime speed. |
| wgpu macOS dev deps (`objc2-*`, `dd-shim-vk`, `ash`) | Used by macOS interop/Vulkan journey tests, not production. | Keep unless corresponding tests are removed/replaced. |
| shim `dd-gpu` dependencies | IR/capability/wire types cross FFI; Rust call-graph tools may miss generated exports. | Keep unless generated code and export census compile without them. |

No dependency is proven immediately removable from manifests in this wave. Run `cargo tree -e features` on macOS before feature edits; Linux's cfg-empty wgpu crate cannot prove mac dependency deadness.

## Environment branch audit

Repository-single-read flags include `DD_DISPLAY_AUGMENTER`, `DD_DISPLAY_DUMP_EVERY`, `DD_DISPLAY_MIRROR_INPUT_GEOMETRY`, `DD_DISPLAY_PNG_EVERY`, `DD_DISPLAY_PRESENT_DEBUG`, `DD_DISPLAY_SYNC_PRESENT`, `DD_DISPLAY_WINDOW_DRAG`, `DD_GPU_DUMP_DIR`, `DD_GPU_DUMP_TEXTURES`, and `DD_VK_NO_WL_PRESENT`.

These are not zero-consumer symbols: environment variables are externally invoked interfaces. Classify as follows:

- **Debug-only retirement candidate:** `DD_DISPLAY_AUGMENTER` has one registry branch and no repository launcher/test consumer. Before removal, capture registry traces from supported guests and search packaging/deployment scripts outside this repository.
- **Diagnostics:** dump/present/window-drag/input-geometry flags. Consolidate and document process ownership; removal saves maintenance, while off-state cost is normally one env lookup or cached branch.
- **Correctness/performance controls:** `DD_DISPLAY_SYNC_PRESENT` and `DD_VK_NO_WL_PRESENT`. Keep until async present and live WSI are proven across device loss/headless tests.
- **Generated/version constants:** `DD_DRIVER_VERSION`, `DD_ICD_INTERFACE_VERSION`, and similar one-site identifiers are compile/build inputs, not runtime dead flags.

Off-state hot-path review:

- `DD_SHIM_DEBUG`/strict helpers generally cache the environment decision or branch before formatting; retain unless profiling shows measurable cost.
- `DD_TILE_TRACE` checks an environment variable from frame lowering and calls a no-op-like diagnostic function each frame. Closing the Chrome investigation permits removing the call/module and eliminates an off-state env lookup/function call; benchmark frame lowering before claiming material speedup.
- Dump-directory paths execute only when enabled. Removing them does not speed ordinary rendering beyond branch elimination.
- Repeated uncached `std::env::var` in per-frame/per-draw code should be cached even if the feature remains; verify call frequency before optimization.

## Immediate safe deletion set

1. `dd-gpu-wgpu/src/shader.rs::legacy_msl`.
2. Its `legacy_msl_is_not_spirv` setup/assertion that exists solely for that helper (retain malformed-SPIR-V and non-SPIR-V fallback tests through `spirv_to_wgsl`).
3. Stale `allow(dead_code)` attributes/comments on `TexEntry.format` and `Win.window` only after macOS compiler proof; do **not** delete either field.

Required validation: isolated macOS `cargo check -p dd-gpu-wgpu -p dd-display --all-targets`, shader tests, texture copy/blit/flip tests, IOSurface tests, and `git diff --check`.

Everything else is cut-after-migration, a correctness gap, ABI/layout state, target-cfg test support, or an external environment interface. This deliberately keeps the immediate set small enough that it cannot reduce guest compatibility or rendering speed.
