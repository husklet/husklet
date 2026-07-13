# Render-stack deep dead/legacy audit — wave B

Audit date: 2026-07-12. Documentation only; no removal is authorized by this report.

## Scope and proof method

Tracked scope: 181 files across `dd-display/` (25), `dd-compositor/` (27), `dd-gpu/` (38), `dd-gpu-wgpu/` (15), `dd-shim-common/` (3), `dd-shim-cuda/` (12), `dd-shim-cudart/` (12), `dd-shim-gl/` (23), and `dd-shim-vk/` (26).

Evidence included Rust/C callers, generated/exported ABI manifests, Cargo target/features, root workspace membership, Make/package entry points, launcher environment forwarding, capability negotiation, runtime socket paths, macOS-only tests/examples, and default-vs-opt-in behavior. A symbol was not called dead merely because Rust callers were absent: shim exports are loaded by guest dynamic linkers and Vulkan/CUDA/OpenGL loaders.

Confidence levels: **high** means a cut has no product consumer and no ABI/capability effect; **medium** requires one migration/gate; **low** is inventory only.

## Remove now: compatibility- and speed-neutral

| Candidate | Evidence | Confidence | Risk / validation |
|---|---|---:|---|
| `dd-gpu-wgpu/src/shader.rs::legacy_msl` | Marked `allow(dead_code)`; only caller is its unit test. Backend malformed/legacy shader handling does not invoke it. | High | Remove function and its dedicated recognition test; run wgpu shader tests on macOS. This does **not** remove the backend's builtin fallback classification. |
| `dd-gpu-wgpu/src/backend.rs` dead orientation field at line ~323 | Explicit `allow(dead_code)` and reserved for “later increments”; no current read. | High | Remove field/initializers after one `cargo check -p dd-gpu-wgpu --all-targets` on macOS. No runtime branch changes. |
| `DD_DISPLAY_AUGMENTER` registry branch in `dd-display/src/server.rs` | One read, no launcher/package/test consumer; comment says debugging-only and default disabled. | Medium-high | Remove only after confirming no private deployed guest binds the dd-private augmenter. Capture one registry trace from Chrome/GTK first. |
| Stale historical comments claiming future use for dead fields/functions | Exact proven examples above; other `allow(dead_code)` sites are audit leads only. | Medium | Delete code only where caller/ABI proof is complete; otherwise replace “later” comments with an owned ledger row. |

No performance-critical fallback is in this section. Removing disabled diagnostics does not improve hot-path speed unless their off-state still allocates or formats; verify generated assembly/profile before claiming a speedup.

Two apparent dead fields are explicitly **not** removal candidates: `CocoaPresenter::Win.window` retains the native window by ownership even without reads, and `TextInputState::global` retains/identifies the advertised global. Rust read-count analysis cannot infer RAII/foreign-object lifetime.

## Cut after migration

### 1. Legacy manual Wayland compositor

`dd-display/src/server.rs` (manual wire/state machine) is the default. `dd-display/src/main.rs` execs `dd-compositor` only when `DD_DISPLAY_SMITHAY=1`. The two paths duplicate registry policy, dmabuf, input, popup placement, output/scale, presentation feedback, GPU startup, and native event-loop behavior.

Cut target after Smithay becomes default:

- `dd-display/src/server.rs`, legacy dispatch portions of `main.rs`, legacy-only test helpers, and duplicated protocol constants.
- Preserve `dd-display::present`, Cocoa/Metal presenter, wire client utilities still used by guest/tests, and renderer backend code.

Prerequisites:

1. Remove the opt-in flag by running Smithay as default for Chrome, GTK4, Vulkan WSI, EGL, clipboard/input/IME, popups, multi-window focus, dmabuf, output hotplug, and resize journeys.
2. Run both paths against identical protocol transcripts and pixel dumps; account for every event/order difference.
3. Prove package/app launch no longer references the legacy binary mode and retain a rollback release before deletion.

Confidence that duplication exists: high. Confidence that deletion is safe now: low. Compatibility risk: very high; potential maintenance reduction: very high.

### 2. Legacy shader payload and builtin pipelines

`dd-gpu-wgpu/src/backend.rs` still recognizes non-SPIR-V “legacy MSL-as-bytes”/opaque payloads by selecting builtin pipelines. Examples `verify_ir.rs` and `iosurface_interop.rs` deliberately exercise this shape. Capability negotiation now rejects unsupported shader payloads earlier, but the fallback remains a compatibility path.

Cut only after wire-version telemetry shows no legacy payload, all shims emit negotiated SPIR-V/known IR, and examples are converted to current payloads. Validate Chrome/Vulkan/GLES and old image compatibility. Removing it prematurely turns rendered fallback content into typed failures; speed impact is negligible, compatibility risk medium-high.

### 3. Duplicate CUDA driver/runtime shim state

`dd-shim-cuda` and `dd-shim-cudart` each own near-parallel `state.rs`, `stub.rs`, generated build logic, `DD_CUDA_VRAM_BYTES`, `DD_GPU_EXEC`, `DD_SHIM_DEBUG`, and `DD_SHIM_STRICT` policy. They expose different public ABIs and cannot be merged at the symbol layer, but internal device/transport/allocation/capability state should move into `dd-shim-common` or a new private crate.

Prerequisites: cross-API context ownership tests, driver/runtime allocation interoperability, identical strict/debug behavior, fatbin registration tests, and exported-symbol census. Expected benefit is reduced divergence, not runtime speed. Confidence medium; ABI risk high if attempted as file deletion.

## Keep: required compatibility or correctness

| Surface | Why it is not dead |
|---|---|
| Generated shim stubs | Guest loaders resolve exported Vulkan/CUDA/GL symbols independently of Rust callers. Removal requires changing the advertised API/profile or loader manifest first. False-success behavior should be fixed, not symbols casually removed. |
| `dd-gpu/src/software.rs` | Standing headless correctness fallback and test oracle. It protects unsupported/no-device execution and backend parity; deleting it harms portability and diagnosis. |
| `dd-gpu-wgpu` macOS cfg shell | Empty Linux crate is intentional: dependencies live under the macOS target block and the workspace remains buildable without Metal. |
| `dd-shim-common::transport` fallback socket | Shared runtime transport used when `DD_GPU_EXEC` is unset. Removing it would break normal packaged guests; env override is not the primary path. |
| Vulkan WSI offscreen fallback | `DD_VK_NO_WL_PRESENT` has only an internal read, but off-guest tests/tools require fallback swapchain images. Validate loader/WSI journeys before changing. |
| GLES ES3 gate | `DD_SHIM_ES3` has many test and guest consumers. ES3 remains intentionally opt-in because advertised mandatory coverage is not complete. Removing the gate by always advertising ES3 would be a capability lie; removing ES3 would break explicit probes. |
| CPU/GPU upload/readback fallbacks | Exact-copy and readback paths are slower but are correctness paths for unsupported formats, verification, headless runs, and device limitations. Replace only with proven equivalent native operations. |
| macOS-only tests | Linux sees cfg-empty tests, but they exercise Metal hardware contracts. Their apparent nonexecution off-mac is not deadness. |

## Environment gates and diagnostics

### Candidates to retire after evidence capture

- `DD_TILE_TRACE` and `DD_TEXTURE_DUMP_DIR` in `dd-shim-gl/src/tiletrace.rs`: instrumentation-only, but referenced by `CHROME-FIX-PLAN.md` and the launcher forwards the dump directory. Remove after the Chrome rendering investigation is closed and archive one representative trace format. Confidence medium.
- `DD_DISPLAY_POPUP_WINDOWS`: tested and still gates native popup windows. Promote to default only after live menu/modal tests, then remove the composite fallback in a later release. Do not merely delete the flag.
- `DD_DISPLAY_WINDOW_DECORATIONS`, `DD_DISPLAY_FRACTIONAL_SCALE`, `DD_DISPLAY_DMABUF`, `DD_DISPLAY_HIDPI`: behavior/capability gates, not diagnostics. Each affects guest negotiation or geometry; keep until its enabled behavior is proven default-safe.
- `DD_DISPLAY_DEBUG`, `DD_DISPLAY_INPUT_DEBUG`, `DD_DISPLAY_PRESENT_DEBUG`, `DD_SHIM_DEBUG`, `DD_SHIM_STRICT`, dump/profiling variables: consolidate naming and parsing, but retain at least one low-cost diagnostic route per boundary. Off-state cost is generally one cached/env branch; profile before cutting for speed.

### Packaging/forwarding mismatch to verify

`dd-cli/src/ddjit_launcher.rs` forwards `WAYLAND_DEBUG`, `DD_SHIM_DEBUG`, `DD_SHADER_DUMP_DIR`, and `DD_TEXTURE_DUMP_DIR`, while many display diagnostics are host-side only. Document which process owns every flag. Remove forwarding only when the guest shim no longer reads it; otherwise packaged diagnostics silently stop working.

## Manual wire and duplicated protocol code

`dd-display/src/wire.rs` is used by manual compositor code and test/guest protocol clients. Even after `server.rs` retirement it may remain a lightweight behavioral-test client. Decide separately:

- Keep if tests use raw protocol journeys to avoid adding a client dependency.
- Remove after migrating every consumer to `wayland-client`/Smithay test helpers and comparing malformed-wire coverage.

Do not combine wire-client removal with compositor migration; that obscures regressions and eliminates the differential harness.

## Examples and tests

- `dd-display/examples/live_geometry_popup.rs` and `render_pattern.rs` are manual/live proof tools. If CI/scripts never invoke them, move their behavior into Rust integration tests, then remove examples. Validate native popup geometry and PNG bytes first. Confidence medium.
- `dd-gpu-wgpu/examples/verify_ir.rs` and `iosurface_interop.rs` overlap macOS integration tests but uniquely demonstrate legacy payload/builtin fallback and IOSurface interop. Convert to tests before deletion; currently keep.
- Source-inspection tests should be replaced when found with behavioral ABI/protocol tests. Existing metal and wgpu tests are cfg-gated behavior tests, not evidence of deadness merely because Linux skips them.

## Dependency and feature cuts

- Smithay is configured correctly with `default-features = false, features = ["wayland_frontend"]`; do not prune its modules manually. Cargo/upstream vendor coherence outweighs source-size savings.
- `dd-gpu`'s `runtime` feature gates executor integration. Before removing the feature split, compare consumers that need IR/types without runtime dependencies.
- `dd-gpu-wgpu` target-specific dependencies are required to keep Linux lightweight. Moving them to unconditional dependencies is a regression; removing them eliminates the backend.
- Run `cargo machete`-style analysis only as a lead. Target-specific, build-script, FFI, generated-code, and example-only dependencies require manual confirmation.

## Highest-value sequence

1. Remove the two proven dead wgpu symbols after macOS all-target check.
2. Decide and document ownership of every diagnostic env flag; retire closed-investigation tracers.
3. Make Smithay default behind a release rollback, run transcript/pixel parity, then retire `server.rs` in a dedicated change.
4. Migrate all shims to negotiated current shader payloads, collect old-image telemetry, then remove builtin legacy payload fallback.
5. Consolidate CUDA/CUDART internals without changing either exported ABI.

This order minimizes compatibility and performance risk: it cuts isolated dead state first, preserves correctness fallbacks, and requires behavioral migration evidence before deleting legacy paths.
