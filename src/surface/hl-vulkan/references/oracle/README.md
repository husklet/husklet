# references/oracle — Vulkan parity oracles (first-party, tracked)

Clean-room reference sources + conformance harnesses used as parity oracles when
implementing the driver. Per OVERVIEW: clone a real open-source impl and PORT from
source — never guess semantics.

Clones / harnesses that live here on implementation:

- **Vulkan-Loader** clone — `docs/LoaderDriverInterface.md` (negotiation, entry-point
  discovery, the version-5 API-version compatibility rule behind
  `VK_ERROR_INCOMPATIBLE_DRIVER`) + `include/vulkan/vk_icd.h` (`ICD_LOADER_MAGIC`,
  `set_loader_magic_value`). Source for `src/icd.rs`.
- **MoltenVK** clone — the object model ported across the driver: `MVKInstance`,
  `MVKPhysicalDevice`, `MVKDevice`/`MVKQueue`, `MVKBuffer`/`MVKDeviceMemory`/`MVKImage`,
  `MVKShaderModule`/`MVKPipeline`, `MVKDescriptorSet`, `MVKQueryPool`, `MVKSync`, WSI
  (`MVKSurface`/`MVKSwapchain`), and `vulkan.mm` (`vk_icd*` entries).
- **Conformance harnesses** — vkcube / SPIR-V compute+triangle replay tests proving the
  IR stream replays on a real-Metal host backend (the parity oracle for each family).

Distinct from the top-level gitignored `reference/` external clones (D5).
