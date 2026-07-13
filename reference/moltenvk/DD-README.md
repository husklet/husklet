# MoltenVK — pinned read-only reference subset for `dd-shim-vk`

This is **not** a full MoltenVK checkout. It is a size-trimmed, **read-only** copy of
only the source subtrees that `dd-shim-vk` cites line-by-line, vendored so those
citations are reproducible and upstream drift is reviewable (`docs/codex-rendering.md`
§9.1).

- **Upstream:** https://github.com/KhronosGroup/MoltenVK.git
- **Pinned commit:** `5a4e526ee8f46ac0cebdd516b8b900562b80828e` (MoltenVK v1.4.2, 2026-07-11)
- **License:** Apache-2.0 — see `LICENSE.md`.
- **Vendored:** 2026-07-12. See `../LOCK.md` for the full pin table and the update rule.

## Read-only rule

Do not modify any file here to make dd tests pass. To refresh: bump the pin in
`../LOCK.md`, re-copy the cited subtrees at the new SHA, regenerate manifests, run the
semantic-diff checklist, then port behavior into `dd-shim-vk`. Never mix an upstream
refresh with behavioral dd changes.

## What was copied (cited subtrees only)

- `MoltenVK/MoltenVK/GPUObjects/` — device/memory/image/buffer/queue/swapchain/surface/
  pipeline/renderpass/descriptor/shader-module objects.
- `MoltenVK/MoltenVK/Commands/` — command buffer + command objects (draw, dispatch,
  rendering, etc.).
- `MoltenVK/MoltenVK/API/` — MoltenVK public API headers (version, config).
- `MoltenVK/MoltenVK/Vulkan/vulkan.mm` — the Vulkan/ICD entry-point translation unit.
- `MoltenVKShaderConverter/` — the SPIRV→MSL converter source (`SPIRVToMSLConverter.*`)
  and its public headers.
- `LICENSE.md`, `README.md` — upstream license and readme.

## What was intentionally excluded (to keep repo size sane)

`Externals/` (SPIRV-Cross, SPIRV-Tools, glslang, cereal submodules), `Demos/`,
`Docs/` images, Xcode projects (`*.xcodeproj`), build scripts and build artifacts,
and the `Layers/`, `OS/`, `Utility/` subtrees that dd does not cite. Vendored size:
~2.4 MB.

## Citation coverage (`dd-shim-vk/src` → vendored file)

Every MoltenVK file cited in `dd-shim-vk/src` is present here. Two citations use the
old upstream filename and now map to a renamed file (recorded here as drift):

| Cited in dd-shim-vk | Vendored file | Note |
|---|---|---|
| `MVKDevice.mm` | `GPUObjects/MVKDevice.mm` | |
| `MVKVulkanAPIObject.h` | `GPUObjects/MVKVulkanAPIObject.h` | |
| `MVKDeviceMemory.mm` | `GPUObjects/MVKDeviceMemory.mm` | |
| `MVKBuffer.mm` | `GPUObjects/MVKBuffer.mm` | |
| `MVKImage.mm` | `GPUObjects/MVKImage.mm` | |
| `MVKQueue.mm` | `GPUObjects/MVKQueue.mm` | |
| `MVKPipeline.mm` | `GPUObjects/MVKPipeline.mm` | |
| `MVKRenderPass.mm` | `GPUObjects/MVKRenderPass.mm` | |
| `MVKShaderModule.mm` | `GPUObjects/MVKShaderModule.mm` | |
| `MVKSwapchain.mm` | `GPUObjects/MVKSwapchain.mm` | |
| `MVKSurface.mm` | `GPUObjects/MVKSurface.mm` | |
| `MVKDescriptorSet.mm` | `GPUObjects/MVKDescriptorSet.mm` | |
| `MVKDescriptor.mm` | `GPUObjects/MVKDescriptorSet.mm` | **drift**: no standalone `MVKDescriptor.mm` at this pin; descriptor logic lives in `MVKDescriptorSet.mm` |
| `MVKCmdDraw.mm` | `Commands/MVKCmdDraw.mm` | |
| `MVKCmdDispatch.mm` | `Commands/MVKCmdDispatch.mm` | |
| `MVKCmdRenderPass.mm` | `Commands/MVKCmdRendering.mm` | **drift**: upstream renamed `MVKCmdRenderPass.mm` → `MVKCmdRendering.mm` |
| `vulkan.mm` | `MoltenVK/MoltenVK/Vulkan/vulkan.mm` | |
| `SPIRVToMSLConverter.cpp` | `MoltenVKShaderConverter/MoltenVKShaderConverter/SPIRVToMSLConverter.cpp` | |

`vk_icd.h` is also cited in `dd-shim-vk/src`; it is a Vulkan-Headers file (pin-only in
`../LOCK.md`), not a MoltenVK file.
