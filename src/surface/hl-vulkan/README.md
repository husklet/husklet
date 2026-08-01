# hl-vulkan

Guest Vulkan ICD that lowers Vulkan objects and command buffers to neutral `hl-gpu` commands.
SPIR-V shader modules pass directly into the GPU IR.

The crate owns Vulkan models, validation, lowering, and its guest ICD artifacts. It does not know
about containers or engine implementations. Husklet selects it as part of its graphics device,
projects `libvk_hl.so.1` and the ICD manifest, and supplies `VK_ICD_FILENAMES` declaratively.

```text
shim/       guest Vulkan ICD and manifest
src/model/  Vulkan objects and state
src/service creation, recording, submission, and presentation
src/adapter SPIR-V adaptation
tests/      lowering and compatibility coverage
```

Build and test with:

```sh
cargo test -p hl-vulkan
```

Where this driver stands against Khronos VK-GL-CTS — what is measured, what is a capability gap
rather than a defect, and what to do first — is in [CONFORMANCE.md](CONFORMANCE.md).
