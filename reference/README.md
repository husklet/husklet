# `reference/` — pinned read-only upstream authorities

This directory holds pinned, **read-only** checkouts (or cited subsets) of the
external projects dd treats as authoritative. Provenance and the update rule live in
[`LOCK.md`](./LOCK.md). Do not edit anything under a reference tree to make dd tests
pass — reference changes are separate, reviewable pin bumps (see `LOCK.md` and
`docs/codex-rendering.md` §9.1 / §9.6).

## Reference ownership (§9.1)

Which upstream authority governs which dd crate / concern:

| Problem | Primary authority | dd implementation location |
|---|---|---|
| Vulkan API validity and synchronization | Vulkan spec + Vulkan-Headers `vk.xml` | generated inventory/types + `dd-shim-vk` state machines |
| Loader/ICD ABI and proc-address rules | Vulkan-Loader `LoaderDriverInterface.md`, loader tests | `dd-shim-vk/src/icd.rs`, dispatchable handle wrapper |
| Vulkan-on-Metal lowering decisions | pinned MoltenVK (MVK memory/image/queue/command/WSI objects) | `dd-shim-vk` lowering into explicit `dd-gpu` IR; host backends stay API-neutral |
| SPIR-V validation and translation | SPIR-V spec/tools + naga (+ SPIRV-Cross for MSL cross-ref) | shared shader module/reflection crate used by both front ends and executors |
| Wayland protocol state | protocol XML + vendored Smithay (`third_party/smithay-0.7.0`) | `dd-compositor` handler composition |
| CUDA/PTX behavior | CUDA docs + pinned ZLUDA reference | typed CUDA state and PTX lowering in `dd-shim-cuda`, `dd-gpu::ptx` |

## Layout

- [`LOCK.md`](./LOCK.md) — pinned SHAs, origins, licenses, and the update rule.
- [`moltenvk/`](./moltenvk/) — cited MoltenVK source subset for `dd-shim-vk`
  citations (see `moltenvk/DD-README.md`).
- `alacritty/`, `criu/`, `wezterm/`, `zluda/` — other reference checkouts.

Vulkan-Loader, Vulkan-Headers, SPIRV-Cross and ash are pin-only (recorded in
`LOCK.md`); their sources live in the local mirrors under `/Users/x/vk-refs/`.
