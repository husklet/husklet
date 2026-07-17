# hl-vulkan (WIP staging crate)

Self-contained Vulkan guest driver. Dissolves `hl-shim-vk` into one crate that lowers
intercepted `vk*` calls to the neutral `hl_gpu` IR. The keystone: a `VkShaderModule`
**is** SPIR-V and the IR's shader ABI (`Cmd::CreateShader{ spirv }`) is **also** SPIR-V,
so Vulkan shaders forward with **zero translation** — the thinnest possible guest seam.

Structurally identical to `hl-cuda` (v2 layering: `model/` `service/` `adapter/`);
tested against a `hl_gpu::RecordingSink`. Build/test:

```
cargo test --manifest-path hl-vulkan/Cargo.toml
```

## Uniform shape (mirrors hl-cuda / hl-gl)

```
src/        Rust only — the driver + lowering
  lib.rs result.rs
  model/    instance.rs device.rs memory.rs pipeline.rs descriptor.rs command.rs queue.rs
  service/  create.rs record.rs submit.rs present.rs   (one operation family per file)
  adapter/  spirv.rs   (SPIR-V passthrough → Cmd::CreateShader{ SpirV }, no translation)
shim/       guest cdylib — the deployed drop-in Vulkan ICD (.so + icd.json)
  vulkan/lib.rs   vulkan/icd.json
build.rs    cross-build the ICD shim for aarch64 + x86_64 (DEFERRED this pass)
references/ non-Rust first-party support (registry sidecars + C/MoltenVK oracles)
```

## Artifact (1 guest soname)

| shim sub-crate | soname          | install path            | API family     |
|----------------|-----------------|-------------------------|----------------|
| `vulkan`       | `libvk_hl.so.1` | `~/.hl/vulkan/<arch>/`  | vk* / vk_icd*  |

Plus an `icd.json` the Vulkan loader reads to discover the ICD.

## Driver seam (later pass)

`Vulkan::new(spec)` implements `hl_jit::Driver`. `inject()` binds the soname, writes
`VK_ICD_FILENAMES` at the shim's `icd.json`, and names the exec socket. The guest shim
speaks the one `hl_gpu::transport::ExecConn` over `$HL_GPU_EXEC`, carrying `hl_gpu::Cmd`.

## Scope of this staging pass

FULLY lowered: instance/physical-device/device create (object model + reported
props/limits), buffer/image + device-memory alloc, shader module (**SPIR-V forwarded
verbatim**), compute + graphics pipeline, descriptor set → bind group, command-buffer
recording (`vkCmd*` → `Enc`), `vkQueueSubmit` → `Cmd::Submit`, `vkQueuePresentKHR` →
`Cmd::Present`. DEFERRED (wiring, not lowering): the injectable ICD shim cdylib
(`shim/`), the `build.rs` dual-arch cross-compile, and the `hl_jit::Driver` plug.

Ported (semantics preserved) from `hl-shim-vk/src/` — `state.rs`/`reg.rs` (object model
+ device props, from MoltenVK), `memory.rs` (buffer/image/sampler + usage/format xlate),
`pipeline.rs` (shader/compute/graphics), `descriptor.rs`+`command.rs` (bind group +
`vkCmd*` recording), `command.rs` (submit), `wsi.rs` (present), `ir_seam.rs` (the vk→Cmd
map), `types.rs` (VkResult + handle typedefs).
