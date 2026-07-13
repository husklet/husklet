# dd-shim-cuda / dd-shim-cudart CUDA completeness audit

The third first-class GPU library dd injects into Linux guests, beside the GLES/EGL (`dd-shim-gl`) and
Vulkan (`dd-shim-vk`) drivers. Two cdylibs:

- **`dd-shim-cuda`** → `libcuda.so.1` — the CUDA **Driver API** (`cu*`).
- **`dd-shim-cudart`** → `libcudart.so.1` — the CUDA **Runtime API** (`cuda*`), a peer that lowers onto
  the same shared `dd-gpu` IR through [`dd_gpu::cuda::CudaContext`].

Both are byte-parity peers of the `dd-gpu/cuda/cuda_shim.c` / `cudart_shim.c` oracles (they serialize to
the SAME `dd-gpu` IR), and the compute core executes real PTX end-to-end on the embedded software
backend. The classification/reference authority is the pinned ZLUDA (CUDA/PTX) plus the C oracles.

## Capability census (build.rs-generated, machine-checked)

Each exported entry point carries a `full` / `partial` / `unsupported` record and the exact `CUresult`
an unsupported (or out-of-domain `partial`) path returns. `capability_inventory_is_complete_and_truthful`
asserts one record per exported symbol; the classification is deliberately conservative (an entry is
`full` only when its observable CUDA semantics are actually implemented for the modeled single-device /
synchronous-executor model).

| Library | full | partial | unsupported |
|---|---|---|---|
| `dd-shim-cuda` (Driver API) | 105 | 21 | 6 |
| `dd-shim-cudart` (Runtime API) | 44 | 5 | 0 |

Advertised: CUDA driver version **12.2** (`SUPPORTED_DRIVER_VERSION = 12020`), compute capability
**sm_86**. The advertised PTX ISA is the ABI; the *executed* PTX is the bounded subset enumerated below.

### Driver `partial` (21) — bounded but truthful

- **Kernel launch — modeled PTX subset:** `cuLaunchKernel`, `cuLaunchKernelEx`,
  `cuLaunchCooperativeKernel`. Executes the PTX subset the `dd_gpu::ptx` front-end models (integer/f32
  ALU, `ld/st.global`, thread/block indexing); a kernel using warp intrinsics / f64 / textures /
  inline-asm returns `CUDA_ERROR_NOT_SUPPORTED`, malformed PTX returns `CUDA_ERROR_INVALID_PTX` — never
  a false `CUDA_SUCCESS` that leaves the output buffer unwritten.
- **Module load — PTX text only:** `cuModuleLoad`, `cuModuleLoadData`, `cuModuleLoadDataEx`,
  `cuModuleLoadFatBinary`. The image is treated as PTX text; a SASS/compressed fatbin container is not
  unpacked (a later `cuModuleGetFunction` then returns `CUDA_ERROR_NOT_FOUND`).
- **Function attributes — modeled defaults:** `cuFuncGetAttribute` (per-kernel register/shared pressure
  not tracked), `cuFuncSetAttribute` (retains `MAX_DYNAMIC_SHARED_SIZE_BYTES`; others validated no-op),
  `cuFuncSetCacheConfig`, `cuFuncSetSharedMemConfig` (cache/shared config is a no-op on the modeled device).
- **Synchronous single-queue executor:** `cuStreamWaitEvent`, `cuStreamQuery`, `cuStreamAttachMemAsync`,
  `cuStreamIsCapturing`, `cuThreadExchangeStreamCaptureMode` — the always-ready / not-capturing answers
  are the correct observable semantics for the modeled synchronous executor.
- **Single-device / unified-memory degenerate forms:** `cuMemcpyPeer`, `cuMemcpyPeerAsync` (peer copy
  degrades to device-to-device), `cuMemPrefetchAsync`, `cuMemAdvise` (unified memory: a valid no-op),
  `cuPointerSetAttribute` (accepted no-op).

### Driver `unsupported` (6) — genuinely not modeled, truthful failure

- `cuCtxEnablePeerAccess` / `cuCtxDisablePeerAccess` → `CUDA_ERROR_PEER_ACCESS_*` (single simulated
  device: no peers).
- `cuDeviceGetLuid` → `CUDA_ERROR_NOT_SUPPORTED` (LUID is Windows/TCC-only).
- `cuModuleGetGlobal_v2` / `cuModuleGetTexRef` / `cuModuleGetSurfRef` → `CUDA_ERROR_NOT_FOUND` (the PTX
  model parses kernel *entries* only, not `.global` variables / texture / surface references).

### Runtime `partial` (5)

- `cudaLaunchKernel` (modeled PTX subset, as the driver), `cudaFuncGetAttributes` (modeled defaults),
  `cudaStreamWaitEvent` / `cudaStreamQuery` (synchronous executor), `__cudaRegisterVar` (the PTX model
  parses kernel entries only; `__device__`/`__constant__` globals are not bound).

## Real execution — the vector-add proof, end to end

A real PTX vecadd kernel (`dd_gpu::ptx::VECADD_PTX`, byte-identical to the C oracles' reference kernel)
allocates device memory, uploads inputs, loads the module, launches, and reads back **numerically
correct** results — the whole `libcuda / libcudart → PTX → dd-gpu IR (CreateShader{PtxKernel} +
CreateComputePipeline + Dispatch) → software backend` chain, headless, no NVIDIA GPU. Proven at every
layer:

| Test | Layer proven |
|---|---|
| `dd_gpu … cuda_vecadd_executes_end_to_end_on_software_backend` | `CudaContext` → IR → software oracle → correct numbers |
| `dd_gpu … cuda_vecadd_survives_the_wire` | the kernel descriptor + command stream serialize/decode across the ring |
| `dd-shim-cuda … vecadd_executes_end_to_end_through_the_shim` | the Driver `cu*` extern "C" ABI (`cuMemAlloc`→`cuModuleLoadData`→`cuLaunchKernel`→`cuMemcpyDtoH`) |
| `dd-shim-cudart … vecadd_executes_end_to_end_through_cudart` | the Runtime `cuda*` ABI |
| `dd-shim-cuda … deployed_libcuda_so_runs_vecadd_end_to_end` | the DEPLOYED `libcuda.so` via `dlopen` + `dlsym` (an unmodified CUDA app's exact path) |
| `dd-shim-cudart … deployed_libcudart_so_runs_vecadd_end_to_end` | the deployed `libcudart.so` via `dlopen` |

The two `deployed_lib*_so` tests compile the checked-in `tests/compute.c` and run it against the built
cdylib exactly as a guest does; they skip (do not fail) when the cdylib has not been built (a bare
`cargo test` builds the rlib + test binaries only) or when no C toolchain is present.

## Truthfulness controls

`DD_SHIM_STRICT` aborts on the first genuinely-unsupported call; otherwise an unsupported/out-of-domain
path returns its defined `CUresult` (`unsupported_ptx_launch_returns_error_not_success` asserts a
non-executable kernel returns the accurate error rather than a silent success). The advertised driver
version and compute capability are asserted against the inventory so the library never claims a surface
the modeled device does not back.
