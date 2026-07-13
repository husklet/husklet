# Guest GPU-shim architecture (Rust)

The guest-side GPU drivers dd injects into Linux apps — the GLES/EGL driver today, Vulkan and CUDA
next — are first-class Rust crates, not a test fixture. They cross-compile to the guest ELF targets
and export the Khronos/vendor C ABI, so an unmodified app that links `-lEGL -lGLESv2` (or Vulkan, or
CUDA) loads them as its driver.

This supersedes the single hand-rolled C file `dd-tests/guests/gl_shim.c`, which shipped as product
code from a test directory and re-implemented, by hand, the dd-gpu IR wire that the host already
defines. The Rust crates fix both problems: product code lives in product crates, and the IR is one
shared Rust type both sides compile against.

## Crates

```
dd-gpu              host-side: the IR (ir::Cmd), wire codec, GpuBackend trait, Metal/software executors
  └── dd-shim-common   guest-side foundation (rlib): re-exports dd-gpu's IR as the shared contract +
        │              owns the host-exec transport, completion wake, and GPU-memory registration
        ├── dd-shim-gl     GLES2/EGL front-end (cdylib) → libEGL.so.1 / libGLESv2.so.2
        ├── dd-shim-vk     Vulkan ICD (cdylib) → libvk_dd.so + icd.json (loader-discovered driver)
        ├── dd-shim-cuda   CUDA Driver API driver (cdylib) → libcuda.so.1
        └── dd-shim-cudart CUDA Runtime API library (cdylib) → libcudart.so.1 (peer over dd-gpu)
```

`dd-shim-common` is the only-place-the-wire-lives layer lifted out of `gl_shim.c`:

- **The shared IR contract.** It does *not* redefine the IR — it `pub use`s `dd_gpu::{ir, wire, id}`.
  A shim builds a `dd_gpu::ir::Cmd` stream and serializes it with `dd_gpu::ir::encode_stream`; the
  host decodes it with the *same* `dd_gpu::ir::Cmd::decode` (`dd_gpu::replay::replay_stream`). One Rust
  type, one encode/decode implementation, shared by value — the guest and host **cannot drift**. This
  is what `gl_shim.c`'s hand-rolled `iu8/iu32/istr` and its bare magic tag numbers (`iu8(8)` ==
  `CreateShader`) used to duplicate against `dd-gpu/src/ir.rs`'s `tag`/`etag` tables.
  (`dd_shim_common::transport::FrameBuilder` is the thin accumulator; `transport.rs`'s
  `framebuilder_encodes_the_shared_contract` test round-trips shim-encoded bytes through the host
  decoder as the anti-drift gate.)
- **The transport** (`transport::ExecConn`): the persistent Unix-socket channel to the host GPU-exec
  service (`$DD_GPU_EXEC`), the per-frame `[surface.id, w, h, ir.len()][ir]` header + 1-byte-ack frame
  protocol, lazy reconnect, and the residency-reset signal (a fresh host backend has an empty resource
  cache → re-emit all). Faithful port of `gl_shim.c`'s `exec_stream`.
- **Guest GPU-memory registration** (`transport::renderd::alloc`): the `renderD128`
  `DD_IOCTL_GPU_ALLOC` that mints the rung-2 IOSurface/dma-buf a frame renders into.
- **The completion-wake seam** (`transport::Doorbell` eventfd, `transport::futex_wake`): the primitives
  the future shared-memory command ring will signal on. The working path today blocks on the socket
  ack; the seam is here so a ring-mode shim (and `dd-shim-vk`) don't re-invent it.

The syscall surface is hand-declared `extern "C"` against libc — no external crates — matching the
`dd-gpu` / `dd-term-core` dependency-free discipline.

## Surface-completeness from the Khronos registry

`dd-shim-gl` exports the **complete GLES2 + EGL entry-point set** (358 GLES2 across ES 2.0–3.2 + 44
EGL = 402 symbols), not a hand-picked few. The surface is code-generated:

- `dd-shim-gl/registry/gles2_egl.manifest` — the compact, committed list of every entry point (name,
  return type, params), **generated from the Khronos API registry** `gl.xml` / `egl.xml` by
  `registry/extract_from_khronos.py` (filtering `feature api="gles2"` GL_ES_VERSION_2_0..3_2 and
  `feature api="egl"` EGL_VERSION_1_0..1_5). The manifest is committed (the 2.7 MB `gl.xml` is not) so
  the build needs no XML and no network; regenerate it when bumping the API level.
- `dd-shim-gl/build.rs` — reads the manifest, maps each C type to its Rust C-ABI type (pointer forms
  handled generally; an unknown *base* scalar panics — a fail-loud ABI generator), and emits a
  `#[no_mangle] extern "C"` function for every entry point **not** in its `IMPLEMENTED` set.
- Hand-written bodies (`src/egl.rs`, `src/gles.rs`) implement the semantics for the entry points
  listed in `IMPLEMENTED`; the generator skips those names (no duplicate symbols). Everything else is a
  spec-faithful **default stub**: correct ABI so the app links and runs, a `DD_SHIM_DEBUG`-gated
  "unimplemented entry point" trace (so we can see exactly which calls a given app makes, in order, to
  prioritize porting), and a benign default return. Stubs are replaced by real bodies incrementally —
  the shrinking long tail — with the exported surface never changing.

The generated census constants (`GLES2_ENTRYPOINTS`, `EGL_ENTRYPOINTS`, `GENERATED_STUBS`) back a
completeness test (`surface_is_complete_and_large`).

## Toolchain: cross-compiling Rust to the guest ELF

The key mechanic — a Rust `cdylib` cross-compiled to the guest target with a C ABI + the right
soname — is verified:

- **aarch64 guest: works today, no setup.** This dev host *is* `aarch64-unknown-linux-gnu`, which is
  the guest aarch64 target, so a native `cargo build -p dd-shim-gl --release` produces a valid aarch64
  Linux `.so`. `#[no_mangle] extern "C"` symbols export as `GLOBAL FUNC` in `.dynsym` (verified: 402
  `gl*`/`egl*` symbols). The soname is set via `cargo:rustc-cdylib-link-arg=-Wl,-soname,libEGL.so.1`
  in `build.rs` (verified `readelf -d` → `SONAME libEGL.so.1`). A plain C `dlopen` of the `.so` calls
  the entry points and gets the expected values (verified: `eglQueryString`/`glGetString` return the
  same strings as `gl_shim.c`; `eglGetProcAddress` resolves entry points to their real addresses).
- **x86_64 guest: needs a one-time toolchain add.** `x86_64-unknown-linux-gnu` `rust-std` is not
  installed and there is no `rustup` on this box (rustc is a source-tarball install). The cross linker
  (`x86_64-linux-gnu-gcc`) is present. To build the x86_64 guest `.so`, install the matching
  `rust-std` for `x86_64-unknown-linux-gnu` (via `rustup target add x86_64-unknown-linux-gnu`, or drop
  the std tarball into the sysroot) and link with `-C linker=x86_64-linux-gnu-gcc`. **Action item for
  the maintainer** — the aarch64 path needs nothing.

`dd-shim-common` / `dd-shim-gl` are workspace members but kept **out of `default-members`** (like
`dd-gpu-wgpu`), so the engine gate's `cargo build` surface is unchanged (verified: a default build
does not compile them). Build/test explicitly: `cargo test -p dd-shim-common`,
`cargo build -p dd-shim-gl --release`.

### Packaging the three sonames

`gl_shim.c` is deployed as one `.so` under three sonames: `libEGL.so.1` carries the code, and
`libGLESv2.so.2` / `libwayland-egl.so.1` are thin `DT_NEEDED → libEGL.so.1` stubs. The Rust cdylib
bakes `SONAME libEGL.so.1`; the deploy step (renaming `libdd_shim_gl.so` → `libEGL.so.1` and creating
the two stub libraries) is unchanged. Note the cdylib currently also `DT_NEEDED`s `libgcc_s.so.1`
(Rust's unwinder) and `libc.so.6`; the glibc guest rootfs provides both. A `panic = "abort"` +
`-C link-arg=-Wl,--as-needed` build can drop `libgcc_s` if a leaner dependency set is wanted.

## Adding `dd-shim-vk` / `dd-shim-cuda`

Both are sibling cdylib crates on `dd-shim-common`:

1. `cargo new --lib dd-shim-vk`, `crate-type = ["cdylib"]`, `dependencies.dd-shim-common = { path = .. }`.
2. Generate the export surface from the registry: Vulkan from `vk.xml` (the Khronos Vulkan registry,
   same extractor pattern → a `vk_commands.manifest`); CUDA/NVML from the driver headers (the existing
   `dd-gpu/cuda`, `dd-gpu/nvml` hand-lists are the seed). Set the soname (`libvulkan_dd.so` + an ICD
   JSON for Vulkan; `libcuda.so.1` / `libnvidia-ml.so.1` for CUDA — matching `dd-gpu/cuda/build.sh`).
3. Encode work as `dd_gpu::ir::Cmd`s (the IR is already WebGPU/Vulkan-family with SPIR-V as the shader
   ABI, so a Vulkan front-end maps closely) and ship them through `dd_shim_common::transport` — the
   same channel, ack protocol, and memory registration `dd-shim-gl` uses. No transport is re-invented.

## `dd-shim-cuda` — the CUDA Driver API scaffold (increment 1)

The third library is built exactly like `dd-shim-gl`: a registry-generated C-ABI surface over
`dd-shim-common`. It began as a scaffold (a handful of real entry points + a stubbed long tail) but is
now **surface-complete** — every one of the 132 `cu*` entry points has a real hand-written body at
parity with `dd-gpu/cuda/cuda_shim.c`, and the compute core executes PTX end-to-end on an embedded
`dd-gpu` software backend (no GPU). It is a functional, drop-in `libcuda.so.1` for dd's modeled subset.

- **Crate.** `dd-shim-cuda` (`crate-type = ["cdylib","rlib"]`), out of `default-members` like the other
  shims, so the engine gate's `cargo build` surface is unchanged. It depends on `dd-shim-common` (the
  transport + re-exported IR) and, directly, on `dd-gpu` for the CUDA→IR mapping (`cuda::CudaContext`)
  and the PTX front-end (`ptx::compile`) — the same `dd-gpu` path `dd-shim-common` uses, so the IR
  types are identical. The cdylib bakes `SONAME libcuda.so.1` (the CUDA Driver API drop-in name,
  matching `dd-gpu/cuda/build.sh`).
- **Surface-completeness, code-generated.** `registry/cuda_driver.manifest` lists every entry point
  (name, return type, params); it is **extracted from dd's own clean-room `dd-gpu/cuda/cuda_shim.c`**
  driver-API surface by `registry/extract_cuda_manifest.py` (the CUDA analogue of the Khronos-XML
  extractor — the CUDA API has no registry XML, so an open, clean-room C source of `cu*` definitions is
  the seed; "No NVIDIA source is used"). The manifest is committed (no header, no network). `build.rs`
  maps each C type to its Rust C-ABI type (opaque handles/`CUdeviceptr`/int-enums/struct-pointers
  handled generally; an unknown *base* scalar panics — a fail-loud ABI generator) and emits a
  `#[no_mangle] extern "C"` function for every entry point **not** in `IMPLEMENTED`. The result is the
  full **132-entry `cu*` surface** (init/device/context/module/memory/stream/event/launch/occupancy/
  pointer-attribute families, with the versioned `_v2`/`_v3` names), not a hand-picked few. The census
  constants (`CUDA_DRIVER_ENTRYPOINTS`, `GENERATED_STUBS`) back a completeness test.
- **The CUDA→IR mapping (real, and shared).** The compute model is mapped onto the existing dd-gpu IR
  by reusing `dd_gpu::cuda::CudaContext` — the host-authored translation is **not** redefined in the
  guest. `src/driver.rs` wires the exported entry points to it:
  `cuMemAlloc_v2`→`Cmd::CreateBuffer`, `cuMemFree_v2`→`Cmd::DestroyBuffer`,
  `cuMemcpyHtoD_v2`→`Cmd::WriteBuffer`, `cuModuleLoadData`→a parsed PTX module (module = PTX-as-shader),
  `cuModuleGetFunction`→an entry-point handle, and `cuLaunchKernel`→the compute path
  (`CreateShader`(kernel descriptor) + `CreateComputePipeline` + a flat kernel-parameter buffer +
  `CreateBindGroup` + a `Submit` of `BeginComputePass`/`SetPipeline`/`SetBindGroup`/`Dispatch`/
  `EndComputePass`). To interpret CUDA's untyped `void** kernelParams`, `cuLaunchKernel` runs the shared
  `ptx::compile` purely to recover each parameter's width + pointer-ness, then packs pointer args as
  `CUdeviceptr` device addresses and scalars by width — the exact `CudaContext::launch` ABI. Streams /
  synchronize are the ordering/flush seam (`cuCtxSynchronize`/`cuStreamSynchronize` flush the frame).
  Accumulated IR is encoded with the SAME `ir::encode_stream` the host decodes; an anti-drift test
  (`launch_path_encodes_the_shared_ir_contract`) drives the real exported entry points through
  alloc→H2D→module→launch and decodes the bytes with the host's own `dd_gpu::ir` decoder.
- **What is real vs stub — the long tail is now fully ported.** All **132** entry points have real
  hand-written bodies in `src/driver.rs` at parity with `dd-gpu/cuda/cuda_shim.c` (the parity oracle);
  `GENERATED_STUBS == 0`. The families, and how each is backed:

  | Family | Entry points | Backing (parity with `cuda_shim.c`) |
  | --- | --- | --- |
  | init / version / errors | `cuInit`, `cuDriverGetVersion`, `cuGetError{String,Name}` | real values; 12.2 driver version |
  | device presence | `cuDeviceGet*`, `cuDeviceGetAttribute` (full switch), `cuDeviceComputeCapability`, `cuDeviceTotalMem_v2`, `cuDeviceGetProperties`, `cuDeviceGetUuid[_v2]`, `cuDeviceGetPCIBusId`, `cuDeviceGetByPCIBusId`, `cuDeviceCanAccessPeer` | `CudaDeviceDesc::apple_default` + oracle-matched attribute table; `cuDeviceGetLuid`→`NOT_SUPPORTED` |
  | context | `cuCtxCreate_v2/v3`, `cuCtxDestroy_v2`, `cuCtx{Push,Pop}Current_v2`, `cuCtx{Set,Get}Current`, `cuCtxGetDevice`, `cuCtxSynchronize`, `cuCtxGetApiVersion`, `cuCtx{Get,Set}{Flags,Limit,CacheConfig,SharedMemConfig}`, `cuCtxGetId`, `cuCtxGetStreamPriorityRange`, `cuCtxResetPersistingL2Cache`, `cuCtx{Enable,Disable}PeerAccess` | current-ctx + push/pop stack + per-ctx flags + a limits table; peer access → the spec-correct `PEER_ACCESS_*` errors |
  | primary context | `cuDevicePrimaryCtx{Retain,Release_v2,Reset_v2,GetState,SetFlags_v2}` | ref-counted singleton primary context |
  | memory | `cuMemAlloc_v2`, `cuMemAllocManaged`, `cuMemAllocPitch_v2`, `cuMemFree_v2`, `cuMemAllocHost_v2`/`cuMemHostAlloc`/`cuMemFreeHost`, `cuMemHostRegister_v2`/`cuMemHostUnregister`, `cuMemHostGetDevicePointer_v2`, `cuMemHostGetFlags`, `cuMemGetInfo_v2`, `cuMemGetAddressRange_v2`, `cuMem{Alloc,Free}Async`, `cuMemPrefetchAsync`, `cuMemAdvise` | device allocs → IR `CreateBuffer`; a registry (base/size/kind) backs GetInfo/AddressRange/pointer-attrs; host allocs return real host memory |
  | copy / fill | `cuMemcpy[Async]`, `cuMemcpyDtoD[Async]_v2`, `cuMemcpyHtoD[Async]_v2`, `cuMemcpyDtoH[Async]_v2`, `cuMemcpyPeer[Async]`, `cuMemsetD{8,16,32}[_v2,Async]` | H2D/fill → `WriteBuffer`; D2H → backend readback; D2D → readback-then-write — all execute on the embedded backend |
  | module / function | `cuModuleLoad[Data,DataEx,FatBinary]`, `cuModuleUnload`, `cuModuleGetFunction`, `cuModuleGetGlobal_v2`, `cuModuleGet{Tex,Surf}Ref`, `cuModuleGetLoadingMode`, `cuFuncGet{Attribute,Module,Name}`, `cuFuncSet{Attribute,CacheConfig,SharedMemConfig}` | PTX parsed into modules/entries; globals/tex/surf → `NOT_FOUND` (PTX-entry-only model) |
  | launch | `cuLaunchKernel`, `cuLaunchKernelEx`, `cuLaunchCooperativeKernel`, `cuLaunchHostFunc` | compute pipeline + dispatch via `CudaContext::launch`; host-func runs inline |
  | occupancy | `cuOccupancyMaxActiveBlocksPerMultiprocessor[WithFlags]`, `cuOccupancyMaxPotentialBlockSize[WithFlags]`, `cuOccupancyAvailableDynamicSMemPerBlock` | computed from the modeled SM limits |
  | pointer attrs | `cuPointerGet{Attribute,Attributes}`, `cuPointerSetAttribute` | answered from the allocation registry |
  | stream / event | `cuStreamCreate[WithPriority]`, `cuStreamDestroy_v2`, `cuStream{Synchronize,Query,WaitEvent,AddCallback,GetFlags,GetPriority,GetCtx,GetId,AttachMemAsync,IsCapturing}`, `cuThreadExchangeStreamCaptureMode`, `cuEvent{Create,Record[WithFlags],Query,Synchronize,Destroy_v2,ElapsedTime}` | synchronous-executor tokens with per-stream flags/priority; events timestamp with the monotonic clock so `cuEventElapsedTime` is truthful; callbacks fire inline |
  | proc table / profiler | `cuGetProcAddress[_v2]`, `cuProfiler{Initialize,Start,Stop}` | `cuGetProcAddress` resolves the object's own `cu*` exports via `dlsym(RTLD_DEFAULT)` with the base→`_v2` alias map |

  The handful that cannot be served by the dd-gpu model return the **spec-correct error** the oracle
  returns (`NOT_SUPPORTED` / `NOT_FOUND` / `PEER_ACCESS_*`), never a fake success. The one partial-parity
  item is **fatbin/cubin** input: `cuModuleLoad*` treat the image as PTX text (the oracle's `fatbin.h`
  extraction and its ELF→`INVALID_IMAGE` guard have no Rust port yet), so a real fatbin container is not
  unpacked — plain PTX loads fully.
- **Execution is real, in-process.** The accumulated IR is executed on an embedded `dd_gpu::software`
  `SoftwareBackend` (the same PTX interpreter the oracle mirrors), so alloc / H2D / launch / **DtoH
  readback** / memset / DtoD run end-to-end with numerically correct results and no GPU. On a real host
  the same bytes ship over `$DD_GPU_EXEC` to the host Metal executor (`PTX → SPIR-V → MSL → AIR`,
  research-grade — see `docs/codex-rendering.md` §2.3 and §8).
- **Validation.** `cargo build -p dd-shim-cuda --release` produces a valid aarch64 ELF with
  `SONAME libcuda.so.1` exporting exactly the 132 `cu*` symbols (no duplicates; `.dynsym` matches the
  manifest byte-for-byte). The unit tests cover the completeness census (132 real, 0 stubs), two
  anti-drift round-trips (launch and memset/DtoD encode → the host `dd_gpu::ir` decoder), an
  end-to-end vector-add through the exported API, a memset→DtoD→DtoH + `cuMemGetInfo`/`AddressRange`
  functional check, context management (push/pop/limits/primary-ctx), and function/occupancy/event
  queries. A default `cargo build` does not compile the crate, and `dd-gpu`'s 70 tests stay green.

### Phase 0 — CUDA capability inventory + truthful failures

Symbol count is not the acceptance criterion (`docs/codex-rendering.md` §2.3): a body that returns
`CUDA_SUCCESS` for an operation the IR/PTX executor cannot represent is a *lie* — it leaves output
handles unwritten and moves the failure away from its cause. Phase 0 makes CUDA **truthful**:

- **Generated capability inventory.** `build.rs` emits `capability::CAPABILITIES` — one record per
  exported entry point tagging it `full` / `partial` / `unsupported`, with the exact CUDA error each
  unsupported (or out-of-supported-domain `partial`) path returns and a supported-domain note. The
  crate test `capability_inventory_is_complete_and_truthful` asserts the inventory covers every
  manifest entry, its counts reconcile with the total surface, and every `unsupported` entry carries a
  real (nonzero) CUDA error. **Driver census (132): full 105, partial 21, unsupported 6.** **Runtime
  census (49): full 44, partial 5, unsupported 0.** (Counts are asserted, not hand-maintained; regenerate
  by reading `CAP_FULL`/`CAP_PARTIAL`/`CAP_UNSUPPORTED`.)
  - *Unsupported (driver, always a defined error):* `cuCtxEnablePeerAccess`→`PEER_ACCESS_UNSUPPORTED`,
    `cuCtxDisablePeerAccess`→`PEER_ACCESS_NOT_ENABLED`, `cuDeviceGetLuid`→`NOT_SUPPORTED`,
    `cuModuleGetGlobal_v2`/`cuModuleGetTexRef`/`cuModuleGetSurfRef`→`NOT_FOUND` (no `.global`/texture/
    surface in the PTX-entry-only model).
  - *Partial (bounded supported domain):* the launch family (`cuLaunchKernel`, `cuLaunchKernelEx`,
    `cuLaunchCooperativeKernel`; runtime `cudaLaunchKernel`) executes **only the modeled PTX subset**
    (`dd_gpu::ptx`: no warp intrinsics/`shfl`/`vote`, f64, textures/surfaces, inline `asm`, dynamic
    parallelism); module-load treats the image as PTX text (SASS/compressed fatbin not unpacked);
    stream wait/query, prefetch/advise, peer copy, capture query, `cudaFuncGetAttributes`,
    `__cudaRegisterVar` degrade within the synchronous single-device / unified-memory model.
- **Truthful launch failures.** A kernel outside the modeled PTX subset used to return `CUDA_SUCCESS`
  as a "traced no-op" that never wrote the output buffer. It now returns the accurate error —
  `CUDA_ERROR_NOT_SUPPORTED` for an unsupported instruction/feature, `CUDA_ERROR_INVALID_PTX` for
  malformed PTX (`cudaErrorNotSupported`/`cudaErrorInvalidPtx` in the runtime; a SASS-only fatbin is
  `cudaErrorInvalidKernelImage`). Classified from `dd_gpu::ptx`'s read-only `Display` text ("outside
  the subset" rejections are phrased with "unsupported"). Proven by
  `unsupported_ptx_launch_returns_error_not_success` in both crates.
- **`DD_SHIM_STRICT=1`.** Aborts the process at the *first* unsupported CUDA call, printing the command,
  the object/context detail (function/entry/context + the executor's reason), and a recent-call history
  ring (module-load → get-function → launch). Validated in-process (`cfg(test)` records the abort
  decision as a trip flag; the shipped library calls `std::process::abort()`). A once-per-name
  `DD_SHIM_DEBUG` trace stays for exploratory runs.
- **Accurate advertisement.** `cuDriverGetVersion` / `cudaRuntimeGetVersion` / `cudaDriverGetVersion`
  and the advertised compute capability (sm_86) report exactly the inventory's single-source-of-truth
  `capability::SUPPORTED_{DRIVER,RUNTIME}_VERSION` / `SUPPORTED_COMPUTE_CAPABILITY` (CUDA 12.2 ABI);
  `advertised_version_matches_the_inventory` asserts the identity so the library never advertises a
  version the modeled surface does not back. The 12.2/sm_86 numbers are the ABI a CUDA app compiles
  against; the *executed* capability is the bounded PTX subset the `partial` launch records enumerate.

**Phase-0 exit gate for CUDA: no unsupported CUDA call silently reports success** — enforced by the
truthful-failure tests, the inventory census, and the strict-mode gate.

## `dd-shim-cudart` — the CUDA Runtime API library

Real CUDA apps and frameworks (PyTorch, TensorFlow) link the **Runtime** API (`libcudart`, the
`cuda*` / `__cuda*` surface), not the driver. `dd-shim-cudart` is the fourth library, built exactly
like the others — a registry-generated C-ABI surface over `dd-shim-common` — and deployed as
`libcudart.so.1`. It is **surface-complete**: every one of its **49** entry points has a real
hand-written body at parity with dd's C oracle `dd-gpu/cuda/cudart_shim.c`; `GENERATED_STUBS == 0`.

- **Crate + topology.** `dd-shim-cudart` (`crate-type = ["cdylib","rlib"]`), out of `default-members`
  like the other shims. The C oracle layers cudart on libcuda via a runtime `DT_NEEDED`; the Rust crate
  instead is a **PEER of `dd-shim-cuda` over the same `dd-gpu` core** — it depends on `dd-shim-common` +
  `dd-gpu` (the CUDA→IR mapping `cuda::CudaContext`, the PTX front-end `ptx::compile`, and the embedded
  `software::SoftwareBackend` executor), NOT on `dd-shim-cuda`. That keeps `libcudart.so.1`'s SONAME and
  export table clean (static-linking the `dd-shim-cuda` rlib would leak all 132 `cu*` symbols into
  libcudart's `.dynsym` and drag in that crate's `-cdylib-link-arg` SONAME). The runtime maps to the
  same IR — reuse, don't redefine.
- **Surface-completeness, code-generated.** `registry/cudart.manifest` is the extracted base surface of
  `cudart_shim.c` (37 entry points, via `registry/extract_cudart_manifest.py` — the runtime analogue of
  the driver extractor, matching the several runtime return types `cudaError_t`/`const char*`/`void`/
  `void**`/`unsigned int`) plus a hand-curated tail of the standard driver-backed runtime surface
  (`cudaMallocManaged`, `cudaMallocHost`/`cudaHostAlloc`/`cudaFreeHost`, `cudaHostGetDevicePointer`,
  `cudaDeviceReset`, `cudaThreadSynchronize`, `cudaStreamWaitEvent`/`cudaStreamQuery`, `cudaEventQuery`,
  `cudaFuncGetAttributes`, `cudaDeviceGetPCIBusId` — 12 more) so the shipped library is genuinely
  complete, not a minimal subset. `build.rs` maps each C type to its Rust C-ABI type and emits a stub
  for anything not in `IMPLEMENTED`; the whole surface is implemented, so it emits none. The census
  (`CUDART_ENTRYPOINTS`) backs a completeness test.
- **What cudart owns (vs the driver).** The `CUresult → cudaError_t` map (`result::map_err`, byte-for-
  byte `cudart_shim.c`'s switch — cudart returns a *different* enum), a **thread-local last-error cell**
  (`cudaGetLastError`/`PeekAtLastError` drain/peek it), the **nvcc registration glue**
  (`__cudaRegisterFatBinary/Function/Var` + `__cuda{Push,Pop}CallConfiguration`, a thread-local
  `<<<grid,block,shmem,stream>>>` stack), and a clean-room Rust port of the **fatbin → PTX walker**
  (`src/fatbin.rs`, a faithful port of `dd-gpu/cuda/fatbin.h`: extracts the first uncompressed PTX entry
  from the `__fatBinC_Wrapper_t` container; SASS-only / compressed / malformed → `None` →
  `cudaErrorInvalidKernelImage`, never a crash).
- **Real vs stub — fully ported, driver-parity bodies.** `src/state.rs` owns the shared compute core
  (`CudaContext` + `FrameBuilder` + embedded `SoftwareBackend`, with `flush()` = `replay_stream` into
  the backend), exactly as `dd-shim-cuda`'s `state.rs`. `src/runtime.rs` wires the surface to it:

  | Family | Entry points | Backing (parity with `cudart_shim.c`) |
  | --- | --- | --- |
  | device | `cudaGetDeviceCount`, `cuda{Set,Get}Device`, `cudaGetDeviceProperties[_v2]`, `cudaDeviceSynchronize`, `cudaDeviceReset`, `cudaThreadSynchronize`, `cudaDeviceGetPCIBusId` | one simulated device; `cudaDeviceProp` filled from `CudaDeviceDesc` + modeled constants; current-device TLS |
  | error / version | `cudaGet{Last,PeekAt}Error`, `cudaGetError{Name,String}`, `cuda{Driver,Runtime}GetVersion` | thread-local last-error cell; string tables + 12.2 versions |
  | memory | `cudaMalloc`, `cudaMallocManaged`, `cudaFree`, `cudaMallocHost`/`cudaHostAlloc`/`cudaFreeHost`, `cudaHostGetDevicePointer`, `cudaMemGetInfo` | alloc → IR `CreateBuffer` (+ alloc registry for `MemGetInfo`); host allocs return real host memory |
  | copy / fill | `cudaMemcpy[Async]` (H2D/D2H/D2D/H2H/Default), `cudaMemset[Async]` | H2D/fill → `WriteBuffer`; D2H → backend readback; D2D → readback-then-write; bad kind → `InvalidMemcpyDirection` |
  | stream / event | `cudaStreamCreate[WithFlags]`, `cudaStream{Destroy,Synchronize,WaitEvent,Query}`, `cudaEvent{Create[WithFlags],Destroy,Record,Synchronize,Query,ElapsedTime}` | synchronous-executor tokens; events timestamp with the monotonic clock so `ElapsedTime` is truthful |
  | launch + attrs | `cudaLaunchKernel`, `cudaFuncGetAttributes` | host-stub → registered fatbin → walk→PTX→`module_load` → `module_get_function` → `CudaContext::launch` (compute pipeline + dispatch); `FuncGetAttributes` = modeled defaults |
  | nvcc glue | `__cudaRegisterFatBinary[End]`, `__cudaUnregisterFatBinary`, `__cudaRegisterFunction`, `__cudaRegisterVar`, `__cuda{Push,Pop}CallConfiguration` | fatbin/function registries + the `<<<>>>` call-config stack |

  Two items are honest modeled-value bodies rather than device queries (the dd-gpu model does not track
  them): `cudaFuncGetAttributes` returns the driver-parity constants (maxThreadsPerBlock 1024, numRegs
  32, …), and `__cudaRegisterVar` is a no-op (dd's PTX model parses kernel entries, not `.global`
  variables) — matching the oracle. `cudaDeviceGetAttribute` is intentionally **not** shipped: its
  `cudaDeviceAttr` enum numbering differs from the driver's `CUdevice_attribute`, so a clean forward is
  not possible and a subtly-wrong body would be worse than its absence (the oracle omits it too).
- **Execution is real, in-process.** cudart lowers to the same IR and executes it on the embedded
  `SoftwareBackend`, so a runtime vecadd through the full nvcc path (register fatbin → `cudaMalloc` →
  `cudaMemcpy` H2D → `__cudaPushCallConfiguration` → host stub → `cudaLaunchKernel` → `cudaMemcpy` D2H)
  reads back numerically-correct `c[i] = a[i] + b[i]` with no GPU. On a real host the same bytes ship
  over `$DD_GPU_EXEC` to the host Metal executor.
- **Validation.** `cargo build -p dd-shim-cudart --release` produces a valid aarch64 ELF with
  `SONAME libcudart.so.1` exporting **exactly** the 49 `cuda*`/`__cuda*` symbols (no duplicates, no
  foreign `cu*` — `--exclude-libs`-free because it does not static-link the driver). Unit tests cover
  the completeness census (49 real, 0 stubs), a tier-0 device+memory round-trip with last-error
  behavior, an anti-drift round-trip (`cudaMalloc`/`cudaMemcpy` H2D IR → the host `dd_gpu::ir` decoder),
  the end-to-end fatbin vecadd, clean rejection of a SASS-only fatbin + an unregistered host function,
  and stream/event queries. A plain C `tests/compute.c` `dlopen`s the deployed `libcudart.so.1` and
  drives the full runtime vecadd (proving the ABI + fatbin path). A default `cargo build` does not
  compile the crate, `dd-gpu`'s 70 tests and `dd-shim-cuda`'s stay green, and `dd-shim-gl` is untouched.

## `dd-shim-vk` — the Vulkan ICD (increment 1, foundation)

The fifth library is a **Vulkan Installable Client Driver (ICD)**: the shared object the standard
Vulkan **loader** (`libvulkan`) discovers via an `icd.json` manifest and drives as a real Vulkan
driver. Unlike the GL/CUDA shims (which an app `dlopen`s directly by soname), a Vulkan app links
`libvulkan`, and the loader — not the app — loads our `.so`, negotiates a private loader↔ICD
interface, and routes every `vk*` call to us. This increment establishes the ICD interface + the full
`vk.xml`-generated surface + the bring-up entry points + the Vulkan→IR seam; real command execution is
a later increment (the host SPIR-V→Metal seam it targets is already proven in `dd-gpu-wgpu`).

**Everything is ported from the authoritative open-source references, not invented:**

- **The loader↔ICD interface** (`src/icd.rs`, `src/handle.rs`) — ported from the Khronos
  **Vulkan-Loader** (loader/ICD interface and `vk_icd.h`, revision pinned in `reference/LOCK.md`) and **MoltenVK**
  (`MoltenVK/Vulkan/vulkan.mm`, `GPUObjects/MVKVulkanAPIObject.h`). Three exported ICD entry points:
  `vk_icdNegotiateLoaderICDInterfaceVersion` (agree on interface version ≤ 5, exactly as MoltenVK
  does), `vk_icdGetInstanceProcAddr` (resolve the whole `vk*` surface; special-case the two ICD hooks
  then defer to `vkGetInstanceProcAddr`), and `vk_icdGetPhysicalDeviceProcAddr` (version-4 physical-
  device disambiguation).
- **Why the prior attempt failed with `VK_ERROR_INCOMPATIBLE_DRIVER`, and the fix.** The loader rejects
  a driver (its devices never enumerate) if any of: it can't find `vk_icdGetInstanceProcAddr`;
  negotiation returns `VK_ERROR_INCOMPATIBLE_DRIVER` or a version the loader dropped; **or a
  dispatchable object handed back lacks the loader-magic slot**. That last one is the classic trap:
  per `vk_icd.h`, every dispatchable object (`VkInstance`/`VkPhysicalDevice`/`VkDevice`/`VkQueue`/
  `VkCommandBuffer`) must be a pointer to a struct whose **first pointer-sized word** the loader owns
  — the ICD stamps it with `ICD_LOADER_MAGIC` (`0x01CDC0DE`) at creation and never reads it back (the
  loader overwrites it with its dispatch-table pointer). `handle::Dispatchable<T>` is that layout
  (`#[repr(C)]`, `loader_data: usize` in field 0), mirroring MoltenVK's `MVKDispatchableObjectICDRef`.
  We satisfy all three requirements; the loader accepts us and `vulkaninfo` enumerates the device.
- **The object model + reported device** (`src/instance.rs`, `src/device.rs`, `src/state.rs`) — ported
  from MoltenVK's `MVKInstance`/`MVKPhysicalDevice`/`MVKDevice`/`MVKQueue`/`MVKCommandPool` and its
  Apple-silicon reporting: `"dd Metal (Vulkan)"`, Apple vendor id `0x106b` (`kAppleVendorId`),
  `INTEGRATED_GPU`, **unified memory** (one `DEVICE_LOCAL` heap; one `DEVICE_LOCAL|HOST_VISIBLE|
  HOST_COHERENT` type), one graphics+compute+transfer queue family.
- **The type/ABI surface** — the Khronos **`ash`** bindings (`ash::vk`, `default-features = false`, types
  only) give the bring-up entry points spec-exact `#[repr(C)]` struct layouts (`VkPhysicalDeviceProperties`
  + `Limits`, `VkInstanceCreateInfo`, …) so what a real loader/app reads back is byte-correct — no
  hand-transcribing ~100-field structs.

**Surface-completeness, code-generated** (same pattern as GL/CUDA). `registry/extract_vk_manifest.py`
parses the Khronos **`vk.xml`** with ElementTree into a committed `registry/vk_commands.manifest`:
`T<TAB>type<TAB>kind` records classify by-value types (dispatchable handle → pointer, non-dispatchable
→ `u64`, enum → `i32`, `VkFlags` → `u32`, …) and `C<TAB>name<TAB>ret<TAB>params` records are the
commands, aliases resolved and `api="vulkansc"`-only variants filtered out. `build.rs` reads it, lowers
each C type to its Rust C-ABI type (every pointer → a `c_void` pointer — a pointer is a pointer;
fail-loud on an unclassified by-value base), and emits a `#[no_mangle] extern "C"` stub for every
command **not** in its `IMPLEMENTED` set plus a `dispatch_addr(name)` resolver over the whole surface.
Result: **693 `vk*` entry points** (full core + extensions; only 4 Windows/NVIDIA-SciBuf platform-
extern commands skipped) + the 3 `vk_icd*` hooks = **696 exported symbols**, no duplicates, soname
`libvk_dd.so.1`. The census constants (`VK_ENTRYPOINTS`, `GENERATED_STUBS`, `DISPATCH_NAMES`) back the
`surface_is_complete_and_large` test.

**Real vs. stub (increment 1; extended in increment 2 below).** 21 hand-written bring-up bodies: the 2 proc-addr resolvers;
`vkEnumerateInstanceVersion` + instance/device extension/layer enumeration; `vkCreateInstance` /
`vkDestroyInstance`; `vkEnumeratePhysicalDevices`; `vkGetPhysicalDeviceProperties`/`Features`/
`MemoryProperties`/`QueueFamilyProperties`/`FormatProperties`; `vkCreateDevice`/`vkDestroyDevice`/
`vkGetDeviceQueue`; `vkCreateCommandPool`/`vkDestroyCommandPool`/`vkAllocateCommandBuffers`/
`vkFreeCommandBuffers`. The rest are `DD_SHIM_DEBUG`-traced default stubs, ported to real bodies
incrementally — the shrinking long tail, exactly like the siblings. **(Since Phase 0 — see the
"Phase 0" section below — those stubs fail *truthfully* with the API-defined error rather than
returning `VK_SUCCESS`.)**

**The Vulkan→IR seam** (`src/ir_seam.rs`) sketches the mapping onto the shared `dd_gpu::ir` (re-exported
by `dd-shim-common`) and round-trips what it encodes. The keystone: a `VkShaderModule` **is** SPIR-V,
and the IR's shader ABI is **also** SPIR-V (`Cmd::CreateShader{ spirv }`, lowered host-side to MSL by
naga in `dd-gpu-wgpu`), so Vulkan shaders forward with **zero translation** — the thinnest possible
guest seam. `VkDeviceMemory`+`VkBuffer` → `CreateBuffer`, `VkImage` → `CreateTexture`, compute
`VkPipeline` → `CreateComputePipeline`, `vkCmdDispatch` → `Enc::Dispatch`, `vkQueueSubmit` → `Submit`,
`VkFence` → `CreateFence`/`WaitFence`. The `vk_compute_seam_encodes_the_shared_ir_contract` test
encodes a representative compute stream with the guest producer and decodes it with the host's own
`dd_gpu::ir` decoder (same bytes, same code path — guest and host cannot drift), mirroring
`dd-shim-gl`'s and `dd-shim-cuda`'s anti-drift gates.

**Validation.** The crate builds a valid aarch64 ELF (soname `libvk_dd.so.1`, 696 exports, no
duplicate symbols). A plain C `tests/smoke.c` `dlopen`s it and drives the ICD exactly as the loader
would (negotiate → `vk_icdGetInstanceProcAddr` → `vkCreateInstance` → `vkEnumeratePhysicalDevices` →
`vkGetPhysicalDeviceProperties`), reading back `"dd Metal (Vulkan)"`. And — the real test — the
**standard Vulkan loader** (`libvulkan`), with `VK_ICD_FILENAMES` pinned at our `icd.json` alone, makes
`vulkaninfo` **enumerate the dd device with no `VK_ERROR_INCOMPATIBLE_DRIVER`**: `GPU0 … deviceName =
dd Metal (Vulkan)`, `apiVersion 1.0.0` (Phase 0 — truthfully capped, see below), `vendorID 0x106b`,
`INTEGRATED_GPU`, the queue family (GRAPHICS|COMPUTE|TRANSFER), and the unified-memory heap/type. A
default `cargo build` does not compile
the crate (out of `default-members`), `dd-gpu`'s tests stay green, and `dd-shim-gl`/`-cuda`/`-cudart`
are untouched.

The committed `icd.json` uses `"library_path": "./libvk_dd.so"` (relative to the manifest, per the
loader spec) with `"api_version": "1.0.0"` (Phase 0 — truthful; the loader then treats us as a 1.0 ICD
and, per `Vulkan-Loader loader.c` `terminator_CreateInstance`, substitutes the app's requested
apiVersion down to 1.0 before calling our `vkCreateInstance`, so a `vulkaninfo` that requests 1.3 still
enumerates our device). Deployment places `libvk_dd.so` next to the manifest and points
`VK_ICD_FILENAMES`/`VK_DRIVER_FILES` at it.

### Increment 2 — functional execution (Vulkan → IR → real Metal)

Increment 2 makes the driver **functional**: the exported `vk*` API now translates a real workload into
a `dd_gpu::ir` stream that executes on the host SPIR-V→Metal seam (`dd-gpu-wgpu`, proven in
`spirv_compute.rs`/`spirv_triangle.rs`). **82 of the 693 entry points now have real bodies** (up from
21); the other 611 remain spec-faithful `DD_SHIM_DEBUG`-traced stubs. Each functional body is
**port-cited from MoltenVK** (the canonical Vulkan-over-Metal driver):

- **Memory** (`src/memory.rs`, from `MVKBuffer`/`MVKDeviceMemory`/`MVKImage`): `vkCreateBuffer`
  → `Cmd::CreateBuffer`; `vkAllocateMemory`/`vkBindBufferMemory`/`vkMapMemory`/`vkUnmapMemory` model
  host-visible|coherent unified memory as a staging `Vec<u8>` that flushes to the bound buffer as an IR
  `WriteBuffer` on unmap; a `COLOR_ATTACHMENT` `VkImage` is a host-owned render target (its IR texture
  id is referenced but the shim never emits `CreateTexture`, matching the render-target flip-scratch
  contract in the backend).
- **Shaders/pipelines** (`src/pipeline.rs`, from `MVKShaderModule`/`MVKPipeline`): `vkCreateShaderModule`
  forwards the SPIR-V **verbatim** into `Cmd::CreateShader` (zero translation); `vkCreateComputePipelines`
  → `CreateComputePipeline`, `vkCreateGraphicsPipelines` → `CreateRenderPipeline` (vertex-input stride +
  attributes → `VertexLayout`, `VkFormat`→vertex-format code, input-assembly topology, color target);
  `vkCreateRenderPass`/`vkCreateFramebuffer` fold the attachment load/clear/store.
- **Descriptors** (`src/descriptor.rs`, from `MVKDescriptorSet`): layout/pool/allocate/update record the
  `(set,binding) → buffer` table, materialized as `Cmd::CreateBindGroup` at `vkCmdBindDescriptorSets`.
- **Commands + submit** (`src/command.rs`, from `MVKCmdDispatch`/`MVKCmdDraw`/`MVKCmdRenderPass`/
  `MVKQueue`): `vkCmdDispatch` → a `BeginComputePass`/`SetPipeline`/`SetBindGroup`/`Dispatch`/
  `EndComputePass` block; `vkCmdBeginRenderPass`/`vkCmdDraw`/`vkCmdEndRenderPass` → the IR render pass +
  `Draw`; `vkCmdBindVertexBuffers`/`vkCmdCopyBuffer`; `vkQueueSubmit` wraps the recorded encoder in
  `Cmd::Submit`. `src/reg.rs` is the recording registry (Vulkan handle → IR id maps + the `ir_log`).

**End-to-end validation on REAL Metal** (`dd-gpu-wgpu/tests/vk_compute.rs` + `vk_triangle.rs`, run via
the mac bridge), driving ONLY the exported `vk*` API — an app creates instance/device/buffers/shaders/
pipelines/descriptors, records `vkCmd*`, and `vkQueueSubmit`s; the test (playing the host exec service,
as `$DD_GPU_EXEC` does in production) drains the shim-produced IR and replays it on the `WgpuBackend`:

- **Compute** — a SPIR-V `c[i]=a[i]+b[i]` vecadd (GLSL 450 → SPIR-V via naga, glslang's step): 9 IR
  commands, runs on the live Metal GPU, `c == a+b` (spot `c[3]=513.5`). PASS.
- **Triangle** — a SPIR-V vertex+fragment pipeline: 6 IR commands, rasterizes a green triangle,
  `center(32,32)=[0,255,0,255]` over the gray clear, 722 green pixels. PASS.

To keep the Linux guest `cargo build -p dd-shim-vk` offline-buildable, dd-shim-vk keeps its tiny
always-cached dep set (`dd-shim-common`/`dd-gpu`/`ash`); the Metal validations live in `dd-gpu-wgpu`
(which dev-depends on dd-shim-vk under its existing macOS-only target block, next to the `spirv_*`
seam they replay). The `-Wl,-soname` cdylib link-arg is Linux-gated (macOS `ld` rejects it; the ICD
only ships on the guest, and the macOS build exists solely to run the validations). The default build,
`dd-gpu`'s tests, and `dd-shim-gl`/`-cuda`/`-cudart` are all unaffected.

### Increment 3 — WSI + present (the windowed-app path toward live vkcube)

Increment 3 adds the WSI surface so a **windowed** Vulkan app (vkcube) can render through dd-shim-vk.
**94 of 693 entry points are real now** (up from 82). Port-cited from MoltenVK (`MVKSurface.mm`,
`MVKSwapchain.mm`) and mirroring dd-shim-gl's present half (`src/wayland.rs` / `gl_shim.c`):

- **`VK_KHR_surface` + `VK_KHR_wayland_surface`** (`src/wsi.rs`): `vkCreateWaylandSurfaceKHR` (stores the
  app's `wl_display`/`wl_surface`), `vkGetPhysicalDeviceWaylandPresentationSupportKHR`,
  `vkGetPhysicalDeviceSurfaceSupportKHR`/`Capabilities`/`Formats`/`PresentModes` (B8G8R8A8_UNORM,
  FIFO), `vkDestroySurfaceKHR`.
- **`VK_KHR_swapchain`**: `vkCreateSwapchainKHR` allocates each presentable image as a `renderd`
  IOSurface/dma-buf (`transport::renderd::alloc` — the rung-2 buffer the host Metal executor renders
  into; an offscreen fallback is used off-guest / in tests); `vkGetSwapchainImagesKHR`,
  `vkAcquireNextImageKHR` (round-robin), `vkQueuePresentKHR` terminates the frame with
  `Cmd::Present{ surface, texture }` and ships the render IR to the host GPU-exec over
  `transport::ExecConn` (the `[surface.id,w,h,len][ir]` protocol `eglSwapBuffers` uses).
- Both extensions are advertised in `vkEnumerateInstance`/`DeviceExtensionProperties` so the loader and
  app find them. The graphics-pipeline color-target format now derives from the render pass attachment
  (so a Bgra8 swapchain validates on Metal).

**Validated on REAL Metal** (`dd-gpu-wgpu/tests/vk_present.rs`), driving ONLY the exported WSI API:
`vkCreateWaylandSurfaceKHR` → `vkCreateSwapchainKHR` → `vkGetSwapchainImagesKHR` →
`vkAcquireNextImageKHR` → render a triangle into the acquired presentable image → `vkQueuePresentKHR` —
the presented swapchain image reads back the correct frame (center `BGRA=[0,255,0,255]`, 722 green px).

**Remaining for a LIVE vkcube-on-dd-display render:** the present's host-forward half (render IR →
Metal → the swapchain IOSurface) is wired and Metal-validated; the outstanding piece is the
**foreign-connection wayland commit** — attaching that IOSurface dma-buf to *vkcube's own*
`wl_surface`/`wl_display` (the app owns the connection, so this needs libwayland `wl_proxy` +
`zwp_linux_dmabuf_v1` marshalling on the app's connection, unlike dd-shim-gl which drives its own
socket) — plus standing up the full live stack (engine + dd-display GPU-exec + the `vkself` workspace
with this ICD deployed). That is the next step to the headline milestone.

### Phase 0 — make the Vulkan surface truthful (advertise only what is backed)

An exported symbol that returns `VK_SUCCESS` without doing the work is a *false* success: it can leave
output handles unwritten, let invalid state advance, and move the crash far from the unsupported call.
Phase 0 (`docs/codex-rendering.md` §6, §2.2, §5.1) makes the Vulkan surface honest — the same
principle already applied to GL and CUDA. Nothing about the ABI surface changes (still 696 exports);
what changes is that a stub now **fails truthfully**, the ICD advertises only Vulkan 1.0, and every
command carries a machine-checkable capability record.

- **Truthful failure stubs (`build.rs` + `src/stub.rs`).** A generated `VkResult` stub returns the
  API-defined error instead of `VK_SUCCESS`: `VK_ERROR_EXTENSION_NOT_PRESENT` when the command comes
  from an extension the ICD does not advertise, `VK_ERROR_FEATURE_NOT_PRESENT` for an unimplemented
  core command (or a still-unimplemented command of an advertised extension). A `vkCreate*`/
  `vkAllocate*` stub also nulls its output handle (`VK_NULL_HANDLE`) before failing, so a caller never
  reads uninitialized handle storage. A `void` stub is a no-op, a `VkBool32` stub returns `VK_FALSE`,
  a pointer stub returns NULL — all truthful. Which error each stub returns is derived from the
  command's origin (see the inventory below).

- **Advertise Vulkan 1.0 and reject newer (`src/state.rs`, `src/instance.rs`, `icd.json`).**
  `DD_API_VERSION` is now `VK_API_VERSION_1_0`; `vkEnumerateInstanceVersion` and the physical-device
  `apiVersion` both report 1.0.x, `icd.json` says `"1.0.0"`, and the reported `conformanceVersion` is
  1.0.0 (we make no CTS claim). `vkCreateInstance` now refuses **any** apiVersion newer than 1.0
  (variant/major/minor, patch ignored per spec) with `VK_ERROR_INCOMPATIBLE_DRIVER` — the prior gate
  rejected only `major > 1`, so a 1.4 request slipped through and was accepted. 1.1+ promoted-core
  semantics (bind_memory2, dynamic rendering, timeline semaphores, the `...2` device queries, …) are
  still stubs, so advertising 1.1/1.2/1.3 would let an app select a version whose calls do nothing.
  (`vkcube` requests 1.0, and the real loader substitutes a 1.0 ICD's apiVersion down to 1.0, so both
  keep working.)

- **Truthful extension enumeration (allow-list).** `vkEnumerateInstanceExtensionProperties` advertises
  exactly `VK_KHR_surface` + `VK_KHR_wayland_surface` + `VK_KHR_get_physical_device_properties2` (the
  `...2` physical-device queries this ICD implements), and `vkEnumerateDeviceExtensionProperties`
  exactly `VK_KHR_swapchain` — the WSI stack + what is actually backed, not everything `vk.xml` lists.
  These are pinned to `capability::ADVERTISED_{INSTANCE,DEVICE}_EXTENSIONS` and gated by a test.

- **Generated capability inventory (`src/capability.rs` + `build.rs` + `registry/`).** A companion
  extractor `registry/extract_vk_origins.py` reads `vk.xml` into a committed provenance sidecar
  `registry/vk_command_origins.manifest` (command → `core:1.0`..`core:1.3` or `ext:VK_...`) — this
  never affects the ABI surface (that stays pinned by `vk_commands.manifest`). `build.rs` joins it with
  the `IMPLEMENTED` set and a `partial` override table to emit `capability::CAPABILITIES`: one record
  per exported command tagging it `full` / `partial` / `stub`, the exact `VkResult` each stub returns,
  and its core-version/extension origin. Compile-time counts (`CAP_FULL` / `CAP_PARTIAL` / `CAP_STUB`,
  the Vulkan-1.0 census `CORE_1_0_TOTAL` / `CORE_1_0_IMPLEMENTED`) and a crate test enforce that **every
  exported command has a record** and **no stub advertises a false `VK_SUCCESS`**. Runtime debug output
  and this document draw from the same census, so advertised vs. truthful cannot drift.

  Current census (HEAD): **693 commands = 79 `full` + 21 `partial` + 593 `stub`**. Of the **215**
  cumulative core commands (137 core:1.0, 28 core:1.1, 13 core:1.2, 37 core:1.3), the advertised
  **Vulkan-1.0 mandatory core is 137 commands, 82 of them bodied** (55 remain stubs, each failing with
  `VK_ERROR_FEATURE_NOT_PRESENT`). The 21 `partial` bodies are the bring-up simplifications the audit
  flags (fixed-FIFO/round-robin swapchain, already-signaled fences, binary-only semaphores, the single
  unified memory type, the Metal-class limit/feature subset) — each carries a supported-domain note.

- **`DD_SHIM_STRICT=1` (`src/stub.rs`).** Every stub call funnels through `stub::hit`, which records a
  recent-call history ring, once-logs the name under `DD_SHIM_DEBUG`, and — under `DD_SHIM_STRICT=1` —
  prints command + object + recent history and aborts at the **first** unsupported call, so an
  exploratory app run stops exactly where dd cannot honestly act instead of silently mis-executing.
  (Under `cfg(test)` the abort sets a thread-local flag instead of killing the process, so it is
  assertable.) Same machinery as dd-shim-cuda / dd-shim-gl.

- **Tests (`src/lib.rs`).** Eight Phase-0 gates: the inventory covers every exported command and the
  counts partition the surface; no stub advertises false success; a real stub call
  (`vkCreateSampler`) returns `VK_ERROR_FEATURE_NOT_PRESENT` *and* nulls its output while an
  unadvertised-extension stub returns `VK_ERROR_EXTENSION_NOT_PRESENT`; the ICD advertises 1.0
  consistently; a 1.4 / 1.1 / 2.0 `vkCreateInstance` is refused with `VK_ERROR_INCOMPATIBLE_DRIVER`
  while 1.0 (and a 1.0.x patch) succeed; extension enumeration equals the allow-list; `DD_SHIM_STRICT`
  trips the abort; and the generated Vulkan-1.0 mandatory-core census is self-consistent.

**Exit gate (Phase 0, Vulkan):** no advertised Vulkan command is a default stub returning success; the
ICD advertises 1.0 truthfully; a newer request is rejected. **Met.** The next steps are Phase 1
(build extension/feature/format/limit responses from one device profile) and Phase 5 (grow the
1.0 mandatory core from 82/137 toward complete vertical slices).

## Retiring `gl_shim.c` (the incremental cutover)

GLES must keep rendering the entire time, so `gl_shim.c` stays the deployed driver until `dd-shim-gl`
reaches pixel parity. The path:

1. **Foundation (done).** `dd-shim-common` (shared IR + transport, tested) and `dd-shim-gl` (complete
   registry-generated surface + query entry points, building the correct `.so`). `gl_shim.c` untouched
   → GLES unaffected.
2. **State machine (done — 112 entry points).** The GLES/EGL object + scalar state was ported into
   `dd-shim-gl` (`src/state.rs`, `src/gles.rs`, `src/egl.rs`), moving 105 names from generated stubs
   into `IMPLEMENTED`: buffers, textures (with the RGBA8 unpack-aware upload), shader/program objects,
   uniform-block storage, vertex-attrib/VAO state, blend/depth/cull/scissor/viewport scalar state and
   all the `glGet*`/`glIs*` queries, plus the EGL config/context/surface query + lifecycle
   (`eglChooseConfig`/`eglGetConfigAttrib`/`eglCreateContext`/`eglMakeCurrent`/`eglQuerySurface`/…).
   Like `gl_shim.c`, these accumulate state and emit no IR. The GL→wire enum/id maps
   (`src/wireenc.rs`) and the present-independent **resource lowering** (`src/lower.rs`:
   buffer/texture/sampler → `Cmd`) are ported and proven **byte-identical** to `gl_shim.c`'s emission.
   Still stubbed on purpose (owned by concurrent present-path work): `glClear`/`glDrawArrays`/
   `glDrawElements` recording, the GLSL-ES→(SPIR-V/MSL) translator (so `glGetUniformLocation`/
   `glGetAttribLocation` report "not found" for now), the residency cache, `eglCreateWindowSurface` +
   the wayland/dma-buf commit, and `eglSwapBuffers` (lower state → `Cmd` stream → `ExecConn::submit`).
3. **Present path — draw recording + surface bring-up + swap boundary (in progress).**
   `glClear`/`glDrawArrays`/`glDrawElements` now record into the frame draw-list (`src/state.rs`
   `DrawCall`), `eglCreateWindowSurface` brings up the presented surface, and `eglSwapBuffers`
   (`src/egl.rs` + `src/frame.rs`) lowers the draw-list to the dd-gpu IR and presents it (via
   `DD_IR_DUMP` in host-tool/parity mode, or `dd_shim_common::transport::ExecConn` in the deployed
   path; the wayland/dma-buf commit is the remaining display plumbing). The **clear-path frame is
   byte-identical to gl_shim.c**; the shader-bearing draw path is the remaining work (below).
4. **GLSL-ES → MSL translator (done — byte-verified).** `src/translate.rs` is a byte-for-byte port of
   gl_shim.c's `translate()` + ~20 helpers (strip-comments, collect decls, main-body extract,
   word-boundary/plain replace, type/builtin/relational/local-decl fixups, `fix_trunc`, sampler
   rewrites, `mod`/`mat3x2` helper injection, and the `uni_layout` uniform-block byte layout).
   `glLinkProgram` runs it → `CreateShader` MSL; `glGetUniformLocation`/`glGetAttribLocation` resolve
   against the layout + declaration order. Verified GREEN against gl_shim.c's own `-DDD_TR_TOOL gl_tr`
   over the whole `shader_translate/*.glsl` corpus (`tests/translate_parity.rs`).
5. **Draw-time emission (done — single-draw path).** `src/frame.rs` `build_single_draw_frame` lowers a
   non-clear draw to the exact `dd_gpu::ir::Cmd`/`Enc` sequence gl_shim.c's non-replay `eglSwapBuffers`
   emits: VBO/index/texture/uniform resources, `CreateShader` + `CreateRenderPipeline` (vertex layout,
   blend/depth, topology), `CreateBindGroup`, and the render pass (Begin/SetPipeline/Viewport/Scissor/
   SetBindGroup/SetVertexBuffer/Draw/End) with the Y-flipped viewport/scissor.
6. **Replay path (done — real-workload parity).** `src/frame.rs` `build_replay_frame` lowers a
   multi-draw / clear+draw frame exactly as gl_shim.c's `replay` branch: per-draw resource ids
   (`20+d`/`30+d`/`40+d`/`1000+d`, VBO snapshots `2000+`, index `10000+`, frame fallbacks `200+`/`300+`),
   render-pass **segmentation** by target + clear serial, and the load-vs-clear semantics between draws.
   `record_draw_call` now snapshots each draw's VBOs/IBO (`DrawCall::snap_vbo/snap_ibo`) as gl_shim.c
   does. `build_frame_ir` dispatches clear-only / single-draw / replay per gl_shim.c's `replay_draws`
   rule.
7. **Pixel/IR-parity harness — LIVE, all gates GREEN (`tests/pixel_parity.rs`).** Three black-box tests
   compile the SAME GLES workload against **both** shims' `.so` (gl_shim.c's `libEGL.so.1` +
   dd-shim-gl's cdylib), run each with `DD_IR_DUMP`, and assert identical IR:
   `full_frame_clear` (43 bytes), `full_frame_textured_triangle` (1316 bytes), and
   `full_frame_multi_draw_replay` (clear + 2 blended draws → gl_shim.c's replay path, **2592 bytes,
   byte-for-byte**). **IR parity is closed for real workloads.**
8. **Deployed display plumbing (done).** `src/wayland.rs` is a byte-for-byte port of gl_shim.c's
   hand-rolled wayland/dma-buf client: the registry handshake (`connect_and_handshake`), the per-frame
   dma-buf `commit` (with the `SCM_RIGHTS` fd pass), and frame-callback pacing. `eglCreateWindowSurface`
   now does the `renderD128` `DD_IOCTL_GPU_ALLOC` (`dd_shim_common::transport::renderd::alloc`) + the
   wayland handshake; `eglSwapBuffers` commits the rendered IOSurface/dma-buf to the compositor after
   the IR submit. The `wl_egl_window_*` libwayland-egl entry points (glmark2/Chrome's window path) are
   exported, with the `dd_wl_egl_window` magic-struct parse. (All gated so `DD_IR_DUMP`/host-tool mode
   stays pure — the parity gates are unaffected.)
9. **Cutover selector (done — default NOT flipped).** `dd-shim-gl/deploy.sh` installs the driver into
   `~/.dd/gui/<arch>/lib` gated by `DD_SHIM_IMPL`: unset/≠`rust` is a no-op (the C shim stays);
   `DD_SHIM_IMPL=rust` builds the cdylib and installs it as `libEGL.so.1` + thin `DT_NEEDED→libEGL.so.1`
   stubs `libGLESv2.so.2`/`libwayland-egl.so.1`. **Validated end-to-end:** an app compiled against the
   *deployed Rust stubs* (via `wl_egl_window_create`, the glmark2 link path) emits IR byte-identical to
   gl_shim.c. Flip the default only after a live glmark2/Chrome pixel-check → gl_shim.c retirement.

## GLES-still-works evidence (this increment)

- `gl_shim.c` is **unmodified** (`git diff` empty) and still compiles to the deployed `libEGL.so.1`
  (aarch64 `.so`, `SONAME libEGL.so.1`, clean build) and to its `-DDD_TR_TOOL` translator tool.
- `dd-gpu` (the shared IR) is unchanged and its 70 tests pass.
- The default `cargo build` (engine-gate surface) does not compile the new crates.
- `dd-shim-common` tests pass (shared-IR round-trip through the host decoder; exec-socket framing;
  `GpuAlloc` layout). `dd-shim-gl` builds the 402-symbol `.so` and a C `dlopen` drives it correctly.
