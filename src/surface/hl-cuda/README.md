# hl-cuda (WIP stub)

Self-contained CUDA guest driver. Dissolves `hl-shim-cuda` + `hl-shim-cudart` and
absorbs the CUDA-specific bits leaving `hl-gpu` (`cuda.rs` → `context.rs`, `ptx.rs`).
Everything lowers to `hl_gpu` IR. **Stubs only** — header comments + minimal bodies,
no Cargo.toml, not a workspace member. See `../hl_wip-OVERVIEW.md` §2 and §4.

## Uniform shape (mirrors hl-vulkan / hl-gl)

```
src/        Rust only — the driver + lowering
  lib.rs driver.rs lower.rs frame.rs state.rs result.rs
  context.rs (CUDA→IR, moved from hl-gpu)  ptx.rs (PTX→kernel-IR, moved from hl-gpu)
  fatbin.rs runtime.rs nvml.rs             (one file per API family)
shim/       guest cdylib sub-crates — the deployed drop-in .so(s)
  cuda/lib.rs   cudart/lib.rs   nvml/lib.rs   (one soname each)
build.rs    cross-build each shim for aarch64 + x86_64
references/ non-Rust first-party support (registry sidecars + C oracles)
```

## Artifacts (3 guest sonames)

| shim sub-crate | soname             | install path              | API family        |
|----------------|--------------------|---------------------------|-------------------|
| `cuda`         | `libcuda.so.1`     | `~/.hl/cuda/<arch>/`      | cu* (driver API)  |
| `cudart`       | `libcudart.so.1`   | `~/.hl/cuda/<arch>/`      | cuda*/__cuda*     |
| `nvml`         | `libnvidia-ml.so.1`| `~/.hl/nvml/<arch>/`      | nvml*             |

## Driver seam

`Cuda::new(spec)` implements `hl_jit::Driver`. Registered via `engine.add(Cuda::new(..))`
(OVERVIEW §4). `inject()` binds the three sonames + `LD_LIBRARY_PATH`, sets `HL_CUDA_*`
env, and names the exec socket. The guest shims speak the one `hl_gpu::transport::ExecConn`
over `$HL_GPU_EXEC`, carrying `hl_gpu::ir::Cmd`.

## Cargo shape (to add on confirm — no Cargo.toml in the stub)

- **hl-cuda** — the driver `rlib`. `deps: hl-gpu, hl-jit`. Contains `src/`, `build.rs`.
- Three shim **`cdylib` sub-crates** — `hl-cuda-shim-cuda`, `-cudart`, `-nvml`. Each has
  `crate-type = ["cdylib"]`, one `shim/<lib>/lib.rs` root, `deps: hl-cuda`. `build.rs`
  cross-compiles them for both guest arches.

## Open decisions

D1 (shim mechanism: Rust cdylib ships / C = oracle) — see `../hl_wip-OVERVIEW.md` §6.
Also D2 (transport home), D3 (kernel-IR split, why `ptx.rs` moves here), D5 (references/).
