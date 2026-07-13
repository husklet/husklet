# Render-stack deep audit — wave H: immediate cuts and off-state cost

Audit date: 2026-07-12. Documentation only.

## Decision rule

An immediate cut must preserve exported C ABI, Rust-visible behavior used by workspace consumers, advertised capabilities, protocol event ordering, test meaning, and hot-path performance. Memory/text-only changes are separated from behavioral corrections. Environment reads are external interfaces even when repository search finds no setter.

## Software backend shader state

### Safe memory-only cut

`dd-gpu/src/software.rs::ShaderModule::Spirv(Vec<u32>)` stores the complete opaque payload at `create_shader`, but every later match ignores the bytes. `run_dispatch` returns success without execution for that variant; render-pipeline validation checks only shader identity/generation. No test reads stored words.

Safe group:

1. Change `ShaderModule::Spirv(Vec<u32>)` to a unit-like opaque variant.
2. Keep shader table insertion, generation/stale-resource semantics, destroy behavior, and pipeline identity validation unchanged.
3. Remove the field-level `allow(dead_code)`.

Effect: reduces heap allocation/copy and retained memory proportional to submitted opaque shader size. It does not change public C ABI, wire encoding, capability bits, command acceptance, draw/dispatch counters, or output bytes. Validate software shader create/destroy, stale reuse, pipeline validation, transactional rollback, and full `dd-gpu` tests.

### Not a safe deletion: unsupported acceptance

Software capabilities advertise only `shader_payload::PTX`, yet direct `create_shader` currently accepts valid-magic SPIR-V plus `LegacyMsl` and `DemoBuiltin`. Existing software validation tests directly create `DemoBuiltin` shaders. Dispatch with opaque shader silently returns `Ok(())`; draws are counted but not rasterized.

This is not dead storage. It is a correctness/capability decision:

- Rejecting non-PTX at `create_shader` makes direct API behavior match negotiation but changes tests and any caller bypassing handshake.
- Keeping acceptance requires documenting “identity-only validation” and must not imply execution support.
- Returning success from dispatch with an opaque shader risks false execution success; a typed `Unsupported` is more truthful but behavioral.

Treat as a separate fix with caller census and updated behavioral tests. Do not bundle it with the payload-memory cut.

## Exact immediate safe groups

| Group | Files/symbols | Effect | Required proof |
|---|---|---|---|
| Dead legacy parser helper | `dd-gpu-wgpu/src/shader.rs::legacy_msl` and only its helper-specific test assertion/setup | Text size only; no production/example/cfg caller | macOS all-target compile; retain `spirv_to_wgsl(non-SPIR-V) == None` and malformed-SPIR-V tests |
| Opaque software shader bytes | `ShaderModule::Spirv(Vec<u32>)` payload only | Lower allocation/copy/retained memory; behavior unchanged | `dd-gpu` all-target tests and resource-generation/rollback tests |
| Stale warning suppressions | `TexEntry.format` and Cocoa `Win.window` `allow(dead_code)` attributes only | Text/warning hygiene; zero runtime effect | macOS all-target check; fields must remain because both are live |
| Closed diagnostic tracer, conditional | `dd-shim-gl/src/tiletrace.rs`, module export, frame call, `DD_TILE_TRACE`, `DD_TEXTURE_DUMP_DIR` forwarding/docs | Removes one off-state per-frame function/env check and diagnostic text | Only after Chrome trace investigation is formally closed and replacement diagnostics exist |

The first three groups cannot reduce compatibility or speed. The tracer cut may marginally improve off-state frame lowering but removes diagnostics, so it needs ownership approval.

## Repeated environment lookups and off-state cost

### Per-frame/present paths

- `dd-display/src/metal.rs::present`: parses `DD_DISPLAY_PNG_EVERY`, checks `DD_DISPLAY_SYNC_PRESENT`, and checks `DD_DISPLAY_TEST_TRIANGLE` during presentation. Cache immutable configuration in the presenter/context constructor. This removes repeated environment locking/string parsing; it preserves behavior under the normal process-start configuration model but removes undocumented runtime toggling.
- `dd-shim-gl/src/tiletrace.rs`: checks `DD_TILE_TRACE` on every lowered frame and conditionally checks the dump directory. A `OnceLock<bool>` reduces off-state cost if tracer remains.
- `dd-compositor::popup_windows_enabled` reads `DD_DISPLAY_POPUP_WINDOWS` during commit/root selection. Store policy in `DdState`; tests currently set the variable before state creation, so instance-local caching preserves intended test isolation better than a process-global `OnceLock`.
- `dd-compositor::surface_fractional_scale` parses `DD_DISPLAY_FRACTIONAL_SCALE` per notification. Parse once per compositor/output configuration, but retain explicit output-change recomputation.

### Per-input-event paths

`DD_DISPLAY_INPUT_DEBUG` is repeatedly read in legacy server motion and several Cocoa event paths. Cache per presenter/server instance. Off-state savings are small but deterministic; do not globally cache if tests create differently configured instances in one process.

### Already cheap/cached

`DD_DISPLAY_DEBUG` in server/Metal paths, `DD_RENDER_NOASYNC`, and GL strict mode use `OnceLock` or equivalent. Keep. CUDA/CUDART state initialization reads VRAM/transport/debug during initialization rather than every command. Build-script environment reads are compile-time and irrelevant to runtime cost.

Caching environment configuration changes only runtime mutability, not ABI or advertised capability. Document flags as startup-only before applying it.

## Isolated environment flags

Repository-single-read flags are leads, not zero-consumer proof:

- `DD_DISPLAY_AUGMENTER`: debug-only registry advertisement, no repository launcher/test setter. Candidate removal after deployed guest registry trace.
- `DD_DISPLAY_PNG_EVERY`, `DD_DISPLAY_SYNC_PRESENT`, `DD_DISPLAY_TEST_TRIANGLE`: local Metal diagnostics/correctness controls. Cache rather than remove; sync present is a recovery control.
- `DD_DISPLAY_MIRROR_INPUT_GEOMETRY`, `DD_DISPLAY_WINDOW_DRAG`, present/debug dump flags: externally driven live-debug behavior. Document owner/process and last use.
- `DD_VK_NO_WL_PRESENT`: WSI/offscreen control; removing changes headless/tool behavior.
- version/interface names read once are build/runtime constants, not environment gates.

No isolated flag is proven unconditionally removable solely from repository references.

## Duplicate validation and constants

CUDA driver/runtime versions and compute capability appear in both `result.rs` and `capability.rs`. Equality tests currently prevent drift. Consolidating to one private source of truth is maintenance-safe only if public Rust constants remain re-exported or callers are migrated; C ABI values returned by `cuDriverGetVersion`, `cudaDriverGetVersion`, `cudaRuntimeGetVersion`, and device attributes must remain byte-identical.

Safe migration group:

1. Define version/compute capability once in a shared private module/crate.
2. Preserve existing public constant names as aliases during one release.
3. Keep runtime API tests and exported-symbol census.

This changes text/maintenance only, not speed. Removing aliases immediately could break Rust consumers even though C ABI is unchanged.

Test-only SPIR-V builders (`module_to_spirv`, GLSL/WGSL fallback helpers) are duplicated across wgpu Vulkan/compute/present tests. Consolidating them into `tests/common` reduces maintenance but does not affect product size because test code is not shipped. Preserve each test's fallback semantics and macOS cfg.

## Legacy compositor state: missing behavior versus dead storage

### Missing behavior — keep until migration/fix

- Legacy `dd-display::server::Surface.input_region` and `pending_input_region` are written by protocol requests and commit but ignored by hit testing. Deleting them would hide a conformance gap and prevent later behavior; Smithay path now consumes committed regions. Either implement legacy hit testing or retire legacy compositor after parity.
- `content_types` in Smithay is written and exposed through an accessor; no present policy consumes it yet. Because the protocol is advertised, this is retained client intent, not dead storage.
- idle inhibitors are consumed by host-visible `idle_inhibited`; not dead.
- retired zero-copy buffer uses await completion evidence and prevent premature release; memory retention is a correctness safeguard, not removable leakage.

### Potentially removable after default-path migration

All legacy-only double-buffered surface fields become removable with `server.rs`, but not individually while manual compositor remains default. Removing ignored state piecemeal saves little and increases divergence from Smithay/protocol semantics.

## Capability and ABI matrix

| Candidate | Public C ABI | Rust/workspace API | Capability behavior | Runtime effect |
|---|---|---|---|---|
| Remove opaque shader bytes only | None | Private enum layout only | None | Less allocation/memory |
| Reject non-PTX software shaders | None | Changes `GpuBackend` behavior/tests | Aligns direct calls with advertised PTX-only support | New typed failures |
| Remove `legacy_msl` helper | None | Public Rust function technically disappears; no workspace caller | None | Text only |
| Consolidate CUDA constants | Must preserve returned values | Preserve aliases | None | Text/maintenance only |
| Remove legacy input-region fields | Wayland semantics affected | Private fields | No registry change, but input behavior/migration harmed | Hides missing behavior |
| Cache env flags per instance | None | None | None | Removes repeated lookup; startup-only semantics |

## Recommended sequence

1. Land opaque shader payload storage removal alone and prove identical behavior.
2. Delete `legacy_msl` helper and stale allowances with macOS all-target proof.
3. Cache per-instance present/input/compositor configuration and document startup-only flags; profile before/after.
4. Decide software non-PTX direct-call policy as a correctness change, not cleanup.
5. Consolidate CUDA constants behind compatibility aliases.
6. Retire ignored legacy protocol state only with the full legacy compositor migration.

This sequence separates memory/text cuts from capability changes and preserves every compatibility and performance fallback.
