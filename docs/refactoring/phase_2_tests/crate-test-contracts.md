# Crate-owned test contracts

These contracts define what `cargo test -p <package>` must mean after the split. Exact command flags may
change during implementation, but ownership and required evidence do not.

| Package | Owns | Required evidence | Platform/job |
|---|---|---|---|
| `dd-tests` | generic helper behavior only | timeout/kill, compiler command, temp cleanup, lane selection, result/report/oracle failures | headless all hosts |
| `dd-jit` | host-neutral container builder/runtime API | typed configuration, env/user/mount/device request, supervision/pump/handle results | headless Rust |
| `dd-jit-darwin` | all engine translation and Linux/Darwin guest behavior | C guest matrix, syscall/ABI/opcode/procfs/VFS/network/IPC, loader, pcache, forkserver, checkpoint, overlay | mac engine lanes; C fixtures orchestrated by Rust |
| `dd-images` | OCI/image/archive/build-cache mechanics | ref/auth/manifest/layer safety, pull/push fixtures, archive metadata/ownership/xattr/sparse round trips, discovery/cache | offline HTTP fixtures first; separate network job |
| `dd-daemon` | Docker API and container product journeys | route/state/restart, build, image, exec/attach, lifecycle, network, volume, compose, PostgreSQL/Redis/languages/real images | offline integration plus quick/long oracle jobs |
| `dd-client` | client transport and view models | fake Unix Docker endpoint, request paths/query/body, streaming/errors, conversions | headless Unix |
| `dd-cli` | command and workspace UX | parsing, paths, install/context plans, workspace persistence, daemon/engine launch configuration | headless planning tests; mac install smoke separate |
| `dd-term-core` | terminal engine | parser/grid/input/layout/session/workspace/PTY model/PNG bytes | headless; PTY integration on Unix |
| `dd-gui` | application models/controllers | daemon discovery, install/update plans, workspace actions, view state; native window smoke | mac GTK/Nix gate |
| `dd-gpu` | shared IR/wire/software execution | exhaustive variants, malformed/atomic replay, resource lifetimes, PTX CPU execution, pixels/present, capabilities | headless Rust |
| `dd-gpu-wgpu` | wgpu/Metal host executor | shader formats/failure, CUDA/SPIR-V/Vulkan execution, textures/resolve/copy, IOSurface, presentation | mac Metal gate; required device preflight |
| `dd-shim-common` | guest transport/common ABI | framing, reconnect, acknowledgements, completion, registration/ioctl failure, malformed peer | headless/unit + guest target compile |
| `dd-shim-gl` | EGL/GLES implementation | contexts/share groups/thread current, error lifecycle, state/objects, GLSL lowering, C ABI calls, pixel parity | Rust headless + C guest ABI + host replay |
| `dd-shim-vk` | Vulkan ICD/WSI implementation | loader negotiation/proc lookup, objects/descriptors/commands/memory/pipelines/sync/WSI, C loader clients | Rust/C guest; host Metal execution consumed externally |
| `dd-shim-cuda` | CUDA Driver API | contexts/memory/module/PTX/kernel/stream/event/error, dlopen ABI/export surface | Rust + independent C ABI client |
| `dd-shim-cudart` | CUDA Runtime API | driver mapping, registration/fatbin, launch config, memory/stream/event/last error, dlopen ABI | Rust + compiler-style C fixture |
| `dd-display` | legacy compositor and presenter seam while supported | wire/parser, shm/framebuffer, legacy protocols, presenter timing/errors, Metal backend path | headless core + mac legacy parity |
| `dd-compositor` | Smithay compositor product | lifecycle, composition pixels, popup/subsurface, input/text/clipboard, outputs/scales, dmabuf/sync/presentation, robustness | headless Smithay + mac live gate |

## Cross-crate journey rule

A journey has exactly one result owner. Supporting packages may expose test artifacts but do not duplicate
the assertion:

- Vulkan API → shared IR → Metal is owned by `dd-gpu-wgpu`; `dd-shim-vk` separately proves its ICD/IR
  contract without claiming Metal correctness.
- EGL/GLES → compositor present is owned by `dd-compositor`; `dd-shim-gl` separately proves API state,
  lowering and emitted pixels/IR.
- image pull → container start is owned by `dd-daemon`; `dd-images` separately proves the pulled image
  representation.
- CLI → daemon is owned by CLI acceptance only for user command semantics; daemon API correctness remains
  daemon-owned.

## Test directory convention

```text
<crate>/src/**                 small unit tests beside implementation
<crate>/tests/**               Rust integration entry points
<crate>/tests/support/**       crate-specific adapters, never a standalone test target
<crate>/testdata/<domain>/**   C clients, configs, archives, wire bytes, images and goldens
```

Do not place helper modules directly under `tests/` where Cargo treats each `.rs` as an integration crate;
use `tests/support/mod.rs` or a non-Rust extension/path. Fixture binaries are never mixed with source
without a provenance manifest.

## Executed-count contract

Every crate/job emits: selected, executed, passed, failed, skipped, xfailed and xpassed. Required platform
jobs fail on zero execution, missing artifact/device/toolchain, xpass, unexpected skip and unknown filter.
Optional local jobs can skip only after a preflight reports the exact missing capability.
