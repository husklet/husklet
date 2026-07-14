# hl-gpu-wgpu (WIP stub)

The **cross-platform host GPU backend**: a `hl_gpu::GpuBackend` executor implemented on **wgpu**.
wgpu selects the native API per host — **Metal on macOS, Vulkan on Linux, DX12 on Windows** (GL
fallback) — so this ONE crate is the portable host renderer behind BOTH present paths:
`surface/macos` (IOSurface) and `surface/linux` (DRM/GBM → KMS scanout). Guests speak hl-GPU
command IR; this crate resolves it against a real device, lowering forwarded SPIR-V/GLSL to WGSL via
`naga`. It is the concrete impl of the trait `hl-gpu` (protocol) only declares.

The hand-written Metal executor (hl-compositor `surface/macos/backend.rs`) is a **mac-only ALT** that
also `impl GpuBackend`; the registry picks. **wgpu is the primary + portable path.**

> Outline only — stub bodies (`todo!`), no `Cargo.toml`, not a workspace member. See
> `../hl_wip-OVERVIEW.md` §1 (crate graph) and §3 (hl-gpu registry).

## Layout (decomposed — see OVERVIEW §8)

```
src/
  lib.rs        crate root; re-export WgpuBackend; register() with hl_gpu::registry (all hosts)
  backend/      the impl, domain-split (thin GpuBackend router + per-domain wgpu workers)
  shader.rs     naga lowering: spirv_to_wgsl / glsl_to_wgsl / module_to_wgsl
  interop.rs    per-host shared-device interop (macOS MTLDevice / Linux Vulkan+dmabuf), target-gated
```

## Key change from today: macOS-only → cross-platform + registry

Today the crate gates `wgpu`/`naga`/`metal` under `cfg(target_os = "macos")` and exposes an
`HL_GPU_BACKEND` env-if (`selected()`); hl-display branched on that env var. That gating was only
because Linux host wasn't a target. Now:
- **Cross-platform**: wgpu deps are unconditional (Metal/Vulkan/DX12 backends compiled per host).
- **Registry**: hl-gpu owns a small backend registry; this crate **self-registers** (`register()`
  installs a `"wgpu"` factory). Selection is pluggable, one place — no env-if in the present path.

## Interop with hl-compositor

`interop.rs` adopts the compositor's existing device so present shares ONE queue (no second device):
- macOS: wrap the Cocoa/Metal `MTLDevice`; IOSurface → MTLTexture.
- Linux: share the Vulkan device; export the rendered target as a dmabuf/GBM bo for KMS scanout.

## Cargo shape (when wired)

- **lib** crate, lib name `hl_gpu_wgpu`. Depends on `hl-gpu` (protocol: `GpuBackend` + IR + registry).
- **Unconditional** deps: `wgpu`, `naga` (features `spv-in`, `glsl-in`, `wgsl-out` + `msl-out` on mac),
  `pollster`. wgpu pulls the right native backend per target automatically.
- Per-target interop deps: macOS `metal`/`objc2`; Linux `ash`/`drm`/`gbm` for the dmabuf/KMS bridge.
