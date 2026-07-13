# Render-stack deep audit — wave N: size and allocation quantification

Audit date: 2026-07-12. Documentation only.

## Measurement setup

Host release builds used an isolated target directory:

```sh
CARGO_TARGET_DIR=target-deep-audit-n cargo build --release \
  -p dd-shim-gl -p dd-shim-vk -p dd-shim-cuda -p dd-shim-cudart
size target-deep-audit-n/release/lib*.so
readelf -SW <library>
strings <library>
```

These are aarch64-Linux host cdylibs, not the final guest cross-build. Linker GC, string merging, target ABI, stripping, and LTO can change exact bytes. Source counts are exact; structure byte estimates are 64-bit upper-bound planning numbers until measured by a before/after build.

## Shipped shim baseline

| cdylib | File bytes | `size` text | data | `.rodata` section |
|---|---:|---:|---:|---:|
| `libdd_shim_gl.so` | 1,074,056 | 707,325 | 23,056 | 30,610 |
| `libvk_dd.so` | 1,196,304 | 820,053 | 20,856 | 42,346 |
| `libdd_shim_cuda.so` | 1,026,008 | 693,677 | 22,456 | 47,426 |
| `libdd_shim_cudart.so` | 989,672 | 647,088 | 20,576 | 47,426 |

Debug/symbol tables contribute substantially to file size; use stripped packaged artifacts for release claims.

## Generated capabilities, names, and notes

Committed manifest/source counts:

| Surface | Manifest-like records | NUL-inclusive symbol-name bytes |
|---|---:|---:|
| GLES/EGL | 402 | 7,408 |
| Vulkan command manifest records | 819 (693 generated capability rows in this build) | 22,167 |
| CUDA driver | 132 | 2,569 |
| CUDA runtime | 49 | 932 |

Generated Rust source sizes were 104,273 bytes (GL), 389,094 (Vulkan), 19,055 (CUDA), and 7,275 (CUDART). These are build artifacts, **not shipped byte counts**.

Source-level static inventory upper bounds on a 64-bit target, before strings and alignment:

- GL `Capability`: roughly 48 bytes × 402 ≈ 19.3 KiB.
- Vulkan `Entry`: roughly 56 bytes × 693 ≈ 37.9 KiB.
- CUDA `Entry`: roughly 40 bytes × 132 ≈ 5.2 KiB.
- CUDART `Entry`: roughly 40 bytes × 49 ≈ 1.9 KiB.
- Exact generated note characters observed: CUDA 1,991; CUDART 434; Vulkan 2,426. Names/origins/requirements add more, with linker string merging possible.

However, release `strings`/`nm` show most test-only capability inventory is already garbage-collected from production cdylibs: CUDA/CUDART/GL capability notes were absent, and no public `CAPABILITIES`/`DISPATCH_NAMES` symbol was retained. Vulkan retained at least runtime-required command/origin/extension strings through its proc-address/dispatch path.

Conclusion: gating capability inventories may improve compile time/generated source size but could yield near-zero shipped-size savings under current release GC. Require before/after section measurements; do not claim the source upper bound as product savings.

Vulkan command names are partly runtime-required by `dispatch_addr` string matching. Do not remove or compress them without loader benchmarks and proc-address tests.

## Diagnostic module with proven shipped cost

`dd-shim-gl/src/tiletrace.rs` is 140 lines / 6,459 source bytes. Release `libdd_shim_gl.so` contains:

- `[tiletrace]` message strings, `SAMPLED-EMPTY`, sampled/offscreen descriptions;
- `DD_TILE_TRACE`, `DD_TEXTURE_DUMP_DIR`, filename formatting;
- `trace_frame` and its static frame counter.

Unlike test-only capability inventories, this code is demonstrably shipped and called once per frame. Exact removable bytes require a branch build, but it occupies some of GL's 30,610-byte `.rodata` plus `.text` and adds an off-state environment check/function call.

Safe only after the Chrome investigation using it is closed. Validation: build before/after, compare exported symbols, run GL pixel/capability/Chrome matrix, and report stripped `.text`/`.rodata` delta. Removing it cannot harm GL ABI/capabilities, but removes diagnostics.

## Software opaque shader allocation

`dd-gpu::SoftwareBackend` copies every non-PTX opaque shader payload into `ShaderModule::Spirv(Vec<u32>)`, then never reads the words. Allocation cost per live shader is approximately:

```text
payload_words * 4 bytes + Vec allocation overhead/capacity slack
```

Changing the private variant to unit-like storage removes the allocation and copy while preserving shader ID/generation, pipeline validation, create/destroy behavior, capability bits, and current no-execution behavior. Savings scale with live shader payloads; for N shaders of S bytes, approximately N×S retained bytes plus allocator metadata.

This is an immediate safe memory/CPU cut. Do not combine it with rejecting non-PTX payloads, which is a behavioral capability correction.

## Unused generated GL aliases

`ADVERTISED_GLSL_VERSION_STR` and `ADVERTISED_GL_EXTENSIONS_STR` have no repository consumer beyond generated definitions. Removing their emission is source/build simplification. Because release GC likely already removes unreferenced aliases/strings or merges them with byte forms, expected shipped savings are zero to tens/hundreds of bytes, not a meaningful performance gain. Preserve runtime byte/list/count constants.

## Test-only shader builder duplication

There are 12 definitions of `module_to_spirv`, `glsl_to_spirv`, `wgsl_to_spirv`, `stage_spirv`, or `wgsl_spirv` across wgpu tests, totaling approximately 103 function-body source lines by a simple brace scan. They compile only in macOS test targets and are not shipped in `dd-gpu-wgpu` production artifacts.

Consolidate into `dd-gpu-wgpu/tests/common` for maintenance and test compile-time reduction. Preserve per-test GLSL→WGSL fallback behavior and stage labeling. Product size/allocation/runtime savings: zero.

Examples are also non-shipped by default, but `cargo build --examples` artifacts are user tools; consolidate helpers without deleting example behavior.

## No-op Cargo features

The shim Cargo manifests declare no `[features]`, and searches find no shim `cfg(feature=...)` or `CARGO_FEATURE_*` branch. There is no no-op shim Cargo feature to remove.

`dd-gpu` differs:

- `runtime = ["dep:dd-jit"]` is live and gates the integration module/dependency. Keep it.
- `metal = []` and `cuda = []` are empty feature arrays with no `cfg(feature=...)`, optional dependency, CI, Make, package, or workspace consumer. Their comments claim host-executor boundaries that do not exist in code.

Therefore `metal` and `cuda` are immediate no-op feature/comment cuts. Removing them changes no compiled code, dependency graph, ABI, capability, allocation, or speed. The only compatibility check is undocumented external Cargo invocation using `--features metal`/`cuda`; search downstream automation or retain a short deprecation window if external users are expected.

`dd-gpu-wgpu` target-specific dependencies are cfg structure, not Cargo features. Do not count cfg-empty Linux code as no-op product configuration.

## Additional immediate cuts

| Candidate | Shipped? | Estimated effect | Compatibility |
|---|---|---|---|
| `dd-shim-vk/src/wl_present.rs::StaticPtr<T>` | Source compiler warns it is never constructed; generic has no caller | Tiny `.text`/metadata, likely GC already | Private; immediate safe deletion after all-target check |
| `dd-gpu-wgpu::legacy_msl` helper + helper-only test code | Production helper is unused; test calls it | Tiny source/text, likely production GC; test maintenance reduction | Public Rust helper but no workspace caller; verify external tools |
| Stale `allow(dead_code)` on live `TexEntry.format` and Cocoa window | Annotation only | Zero bytes | Remove annotation, keep fields |
| Generated capability prose comments | Build/generated source only | Compile/readability only | Safe wording update; no ABI |
| `dd-gpu` empty `metal` / `cuda` features and misleading comments | Manifest only; zero repository consumer or cfg branch | No binary/runtime change | Immediate safe cut after external Cargo invocation search; keep live `runtime` feature |

The `StaticPtr` compiler warning is stronger evidence than source search. Still run both guest architectures because cfg can change construction.

## Not immediate cuts

- Exported ABI stubs: required symbol compatibility even when profile negotiation makes calls unlikely.
- Vulkan `dispatch_addr` names: loader runtime.
- CPU software backend: correctness fallback/oracle.
- CUDA/CUDART result constants: public/internal callers and ABI values; consolidate with aliases, do not delete blindly.
- Legacy compositor ignored input-region state: missing behavior, not harmless storage.
- Metal/wgpu readback, builtin, and copy fallbacks: correctness/device fallback paths with performance tradeoffs.

## Validation commands

```sh
# Pure/core and shim behavior
CARGO_TARGET_DIR=target-cut cargo test -p dd-gpu
CARGO_TARGET_DIR=target-cut cargo check -p dd-shim-gl -p dd-shim-vk \
  -p dd-shim-cuda -p dd-shim-cudart --all-targets

# Guest ABI, both architectures
cargo build -p dd-shim-gl -p dd-shim-vk -p dd-shim-cuda -p dd-shim-cudart \
  --target aarch64-unknown-linux-gnu --release
cargo build -p dd-shim-gl -p dd-shim-vk -p dd-shim-cuda -p dd-shim-cudart \
  --target x86_64-unknown-linux-gnu --release
readelf -Ws <before.so> > before.symbols
readelf -Ws <after.so>  > after.symbols
diff -u before.symbols after.symbols

# macOS-only wgpu/test-helper changes
mac bash -lc 'cd <worktree> && CARGO_TARGET_DIR=target-cut-mac \
  cargo check -p dd-gpu-wgpu --all-targets'

# Quantify rather than infer
strip -o before.stripped before.so
strip -o after.stripped after.so
size before.stripped after.stripped
readelf -SW before.stripped
readelf -SW after.stripped
```

For internal-only cuts, exported symbols and behavioral outputs must be identical. Record actual delta; source bytes and generated-array estimates are not release-size proof.

## Priority

1. Remove software opaque shader payload copies (measurable allocation/copy win).
2. Remove empty `dd-gpu` `metal`/`cuda` features and misleading comments; retain `runtime`.
3. Remove unused `StaticPtr` and `legacy_msl`; clean stale allowances.
4. Consolidate test-only shader builders (maintenance only).
5. Retire tiletrace only after diagnostic ownership closes, then measure actual cdylib delta.
6. Avoid capability-inventory gating unless before/after binaries prove linker GC leaves savings.

This ordering yields compatibility-neutral wins first and avoids mistaking test/generated source volume for shipped product weight.
