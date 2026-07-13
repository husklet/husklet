# Current-tree rebrand refresh (2026-07-13)

The 2026-07-07 inventories predate substantial rendering work. Current search finds project-brand surfaces
across all 19 `dd-*` workspace directories, with especially broad footprints in `dd-tests`, `dd-jit-darwin`,
`dd-daemon`, `dd-gui`, the compositor and new shim crates. Raw file counts are discovery aids, not rename
acceptance evidence.

## Workspace packages now in scope

`dd-jit`, `dd-jit-darwin`, `dd-images`, `dd-daemon`, `dd-client`, `dd-tests`, `dd-cli`, `dd-gui`,
`dd-term-core`, `dd-gpu`, `dd-display`, `dd-compositor`, `dd-gpu-wgpu`, `dd-shim-common`, `dd-shim-gl`,
`dd-shim-cuda`, `dd-shim-cudart`, and `dd-shim-vk` are explicit workspace members. The original three
inventories do not fully enumerate the later compositor/wgpu/shim package surfaces.

## Refresh queries required before execution

Run separate inventories so incidental `add`/padding text does not contaminate results:

```sh
rg -n 'DDJIT_[A-Z0-9_]+|DD_[A-Z0-9_]+|DDOCKERD_[A-Z0-9_]+' \
  --glob '!scratch-erl/**' --glob '!reference/**' --glob '!third_party/**'
rg -n '\bddjit_[A-Za-z0-9_]+|\bdd_[A-Za-z0-9_]+' --glob '*.{rs,c,h,m,mm}'
rg -n '(^|[/._-])dd([/._-]|$)|com\.dd\.|~/.dd|/tmp/\.?dd|/var/lib/dd' \
  Cargo.toml Makefile dd-* nix tools website docs .github
cargo metadata --no-deps --format-version 1
```

Classify every match as package/crate, executable, environment, symbol/ABI, wire identifier, service,
persisted path/format, user-visible copy, test fixture, generated output, vendored/reference, or false
positive. Store the generated manifest beside these research files when phase 3 begins.

## Surfaces added or materially changed

- Rust guest shims now generate EGL/GLES, Vulkan, CUDA Driver and CUDA Runtime exports from registries and
  manifests. Rename generator inputs, emitted symbol prefixes only where they are project-private, loader
  JSON/library names, test clients and packaging atomically. Khronos/CUDA API names remain standard.
- `dd-compositor` adds a second compositor binary, Smithay selection variables, protocol fixtures and shared
  dmabuf/presentation contracts. The legacy and Smithay paths must consume the same renamed constants.
- `dd-gpu-wgpu` adds Metal/wgpu backend labels, IOSurface seams and cross-shim integration tests.
- capability negotiation and the GPU IR are serialized contracts. Branding identifiers may change, but
  command tags/version bytes are technical protocol data and should not change cosmetically.
- phase 2 will relocate most `dd-tests` product fixtures. Rebrand their destination paths after the move,
  preventing rename conflicts from obscuring ownership changes.
- release/package docs contain stale `dd` versus `ddcli` ambiguity. Phase 3 must use the selected binary
  scheme consistently instead of preserving both accidentally.

## Exclusions

Do not rename Docker API field names, `docker.sock`, upstream project names, Khronos/CUDA symbols, vendored
Smithay, pinned reference trees, arbitrary test data that intentionally says `dd`, or archive keys without
the decision register's migration policy. Each exclusion belongs in the final compatibility manifest.
