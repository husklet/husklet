# Cargo dependency and crate-boundary audit (wave X, 2026-07)

Scope: every workspace manifest, resolved normal/build/dev edges, target-specific dependencies,
`build.rs`, examples and tests. The audit used `cargo metadata --no-deps`, workspace duplicate-tree and
feature-tree inspection plus source consumers. A macOS-target feature-tree expansion attempted to fetch
uncached target crates and could not complete offline; therefore macOS feature removals below require the
existing `make mac-crates` gate rather than being asserted from a Linux-only resolution.

## Finding: no proven unused direct dependency

Every direct dependency currently has a source, test, example, generated-binding or platform consumer.
Removing an edge merely because the same package is transitive would make the manifest incorrect and
fragile. In particular:

- `dd-shim-{cuda,cudart}` need direct `dd-gpu` access to `cuda`, `ptx`, `software` and `replay`, which
  `dd-shim-common` intentionally does not re-export. `dd-shim-vk` uses only `backend`, `ir` and
  `GpuError`, but those are its object-model contract; retaining the direct edge is clearer than relying
  on a re-exporting transport crate and adds no duplicate compiled package.
- `dd-compositor` directly consumes `dd-display` presenter types and `dd-gpu-wgpu::selected`; neither is
  redundant. Its `libc` and macOS `objc2*` edges have production consumers.
- `dd-display` uses `dd-term-core`'s PNG encoder in production debug-output paths, examples and golden
  tests; moving the encoder to avoid this edge would duplicate code, not reduce the resolved graph.
- `dd-daemon` directly uses `hyper`, `hyper-util`, `tower` and `futures-util` APIs. They are not removable
  just because Axum also depends on them. `dd-client` directly uses `bytes` in its public log adapter and
  `futures-util::StreamExt`.
- CLI/GUI serialization, hashing, Tokio and libc dependencies all have direct production consumers.
- `ash` is correctly `default-features = false`; Smithay and wgpu are also already constrained to their
  required frontend/Metal features. Target-only objc/wgpu/naga dependencies must remain visible to Cargo
  even though a Linux build does not compile them.

## Feature cuts worth validating

| Priority | Current edge | Candidate | Confidence and required proof |
|---|---|---|---|
| P0 | `relm4 = "0.9"` | `default-features = false, features = ["macros"]` | High. Relm4 defaults enable `css`, `macros`, and `gnome_42`. The project uses Relm component/view macros, while its CSS is loaded through GTK's `CssProvider`, not `relm4-css`; direct `gtk4` already requests v4.10. Confirm `cargo tree -p dd-gui -e features` loses `relm4-css`, then build/package and exercise every view. This removes a proc-macro/helper edge without runtime policy change. |
| P1 | `bollard = "0.21"` defaults `http,pipe` | Test `default-features = false, features = ["pipe"]` | Medium. The only constructor is `Docker::connect_with_unix`; `pipe` supplies the local transport, while no TCP constructor is used. Bollard feature internals may still require `http` on Unix despite the feature declaration, and public compatibility may intentionally include TCP later. Validate all targets, daemon API integration and Unix socket operation before accepting; otherwise keep defaults. |
| P1 | `naga` has defaults plus explicit six front/back ends | Set `default-features = false` if the pinned Naga 24 manifest confirms no required validation/runtime feature is default-only | Medium-low until resolved on macOS. Compile every wgpu test/example and replay GLSL, SPIR-V, WGSL and MSL paths. Do not infer safety from Linux, where the dependency is target-gated away. |
| P2 | broad `objc2-*` feature lists | Remove features only from an item-level generated-API inventory | Low as a batch edit. Generated bindings frequently gate impls and superclass methods under features not obvious from a path search. Change one feature at a time and run `make mac-crates`, examples and packaging. The likely build-time win is small because GTK already brings much of objc2 into the app graph. |

`vte4` has an empty default set; its explicit `v0_72` is used by scrollback APIs. `clap`'s derive
feature, serde derives, Tokio feature lists, Smithay `wayland_frontend`, wgpu Metal/WGSL, and ash's
disabled defaults are already intentional minimums. Do not spend maintenance effort rewriting them.

## Resolved duplicates are not actionable removals

The duplicate tree reports `getrandom` 0.2/0.3/0.4 and two roles for `smallvec`. `getrandom` versions are
owned by independently versioned transitive stacks (`flume`/Relm4, Smithay/rand, Smithay/tempfile); no
workspace direct dependency can unify them. `smallvec` is one version used by GTK's build graph, Smithay,
Wayland and HTTP dependencies, so it is shared, not duplicated code. Upgrading vendored Smithay or GUI
frameworks solely to collapse these versions is higher compatibility risk than their build-size cost.

## Boundary and utility consolidation

1. Keep `dd-gpu` as the source of IR/backend/CUDA/PTX behavior and `dd-shim-common` as transport plus
   narrow IR re-exports. Do not expand the latter into a façade for all of `dd-gpu`; doing so hides real
   coupling and does not alter compilation.
2. Centralize the duplicated dmabuf identity constants/decoder and presentation conversion in
   `dd-display`, already a dependency of `dd-compositor`. This removes utility duplication without a new
   crate or dependency edge; the exact mechanics are specified in the wave-V compositor audit.
3. Centralize Cocoa event normalization in the same existing boundary. Do not create a generic utility
   crate: an extra package increases feature/cfg surfaces and cannot reduce runtime dependencies.
4. Keep the PNG encoder where it is until terminal rendering and display debug output have a deliberately
   named shared media crate. A move solely to make the dependency graph look layered has no build or
   runtime benefit.
5. Audit examples before dependency removal. `dd-gpu-wgpu`'s raw `metal`/`wgpu-hal` and objc dev edges
   support the IOSurface example and the production zero-copy seam; they are not disposable demo-only
   packages. Build scripts in CUDA shims likewise consume their declared contract during export generation.

## Acceptance sequence

For each feature cut, compare `cargo tree -e features` before/after, run `cargo check --workspace
--all-targets`, crate tests, examples, and the macOS `make mac-crates` gate. For GUI or client changes,
also package the app and perform Unix-socket connection and GUI view smoke tests. Require Cargo.lock to
lose an actual package or feature edge; a manifest-only spelling change with an identical resolved graph
is not a maintenance win. No source-string test or benchmark is acceptance evidence.
