# CUDA on Metal — showing a Linux dd container a CUDA device, backed by the host Apple GPU

Status: **design + dependency-light prototype, now with a working software-backed compute path.**
Companion to [`RENDERING_GPU_BACKENDS.md`](RENDERING_GPU_BACKENDS.md) (the host-GPU-agnostic backend
abstraction this rides on) and the rung-3 forwarding in [`RENDERING.md`](RENDERING.md) /
[`RENDERING_PLAN.md`](RENDERING_PLAN.md). The executable part is the `dd-gpu` crate (`../../dd-gpu`) —
IR + wire + resource table + `GpuBackend` trait + a **PTX → dd-GPU kernel-IR front-end + CPU
interpreter** (`dd-gpu/src/ptx.rs`) + the `cuda` translation shim + the **`libcuda.so.1` C shim**
(`dd-gpu/cuda/`) — which builds and `cargo test`s **headless on the Linux dev host with no GPU/CUDA**.

**What now works end-to-end, headless, with no GPU** (the tier-1→tier-2 milestone): a real PTX
**vector-add** kernel runs the whole guest-facing path — `cuInit → cuMemAlloc → cuMemcpyHtoD →
cuModuleLoadData(PTX) → cuModuleGetFunction → cuLaunchKernel → cuMemcpyDtoH` — and produces
**numerically correct results** on the CPU `SoftwareBackend` (the standing correctness oracle). Proven
two ways: (1) the Rust `dd-gpu` tests (`cuda_vecadd_executes_end_to_end_on_software_backend`, plus a
full-wire variant) drive `CudaContext` → dd-GPU IR → `SoftwareBackend` and assert `c[i]==a[i]+b[i]`;
(2) the `dd-gpu/cuda` **dlopen ABI test** drives the same sequence against the real `libcuda.so.1`
export surface and verifies 2000 elements. The Metal executor is a `GpuBackend` swap behind the same
trait — see [§5.2](#52-the-gpubackend-seam--software-oracle-today-metal-drop-in-later). The gaps are
honest and large: only the **modeled PTX subset** runs (see §5.2); real kernels need the broader
PTX→IR/SPIR-V coverage; the `libcuda` launcher injection is designed-not-wired; frameworks are tier 3.

## The ask, precisely

> Simulate a CUDA device *inside* a Linux dd container. Let the user `apt install` the CUDA toolkit and
> have `nvidia-smi`, `torch.cuda.is_available()`, and — the north star — a real **ML workflow** work,
> with the actual GPU math running on the **host Apple-silicon Metal GPU**. dd's JIT runs the Linux
> guest but *cannot call Metal directly* (Metal is a macOS framework), so the CUDA calls are intercepted
> in-guest and **command-forwarded** to a host Metal executor.

This doc decides **where to intercept**, **how each CUDA concept maps to Metal**, the **PTX→Metal**
core, and — bluntly — **what is realistic short-term vs research-grade**, with the ML-workflow goal as
the yardstick.

## TL;DR verdict

- **Do NOT emulate the NVIDIA *kernel* driver.** The `/dev/nvidia*` ioctl ABI is closed, enormous, and
  pointless to reproduce. Intercept in **user space**: ship a dd **`libcuda.so` shim** (the Driver API —
  the same seam ZLUDA replaces) plus `libcudart`, `libnvidia-ml` (NVML), and `libnvrtc` shims. The user
  installs the CUDA *toolkit* (headers, `cudart`, math libs); dd *substitutes the driver*. Synthesize
  `/dev/nvidia*` nodes only for presence probes.
- **The universal kernel IR is SPIR-V**, reached via **PTX → SPIR-V** in/near the guest, then
  **SPIR-V → MSL → AIR** in the host Metal executor (SPIRV-Cross/naga for the middle hop; Apple's Metal
  compiler for the last). This reuses dd's graphics forwarding path — the compute kernel is just a
  `dd-gpu` compute pipeline whose shader module is SPIR-V.
- **Feasibility, honestly, in three tiers** (see [§7](#7-feasibility-verdict--honest-phasing)):
  1. **Device presence + memory + copies** — *realistic now.* NVML/`nvidia-smi`/`cudaGetDeviceProperties`
     report a plausible device; `cudaMalloc`/`cudaMemcpy` map to `MTLBuffer`; on Apple Silicon's unified
     memory H2D/D2H collapse toward **zero-copy**. This makes `torch.cuda.is_available()` true and driver
     probes pass. Prototyped in `dd-gpu::cuda` + `SoftwareBackend` today.
  2. **Custom / simple PTX kernels** — *feasible but a long tail.* PTX→SPIR-V→MSL runs many hand-written
     kernels; warp intrinsics, dynamic shared memory, exotic atomics, `printf`, and `__nv_*` intrinsics
     are where it breaks per-kernel. This is where "ZLUDA-on-AMD after years" lives: *works for many, not
     all.*
  3. **Real framework performance (PyTorch/TF)** — *large, bounded-but-multi-quarter, partly
     research-grade.* PyTorch's speed lives in **closed cuBLAS/cuDNN/cuFFT kernels** you cannot translate;
     the only sane road is to **redirect those library APIs to Apple MPS/MPSGraph/MLX**, plus an **NVRTC**
     shim for JIT'd kernels. Full, correct, fast PyTorch-CUDA-on-Metal is **not** a short-term
     deliverable. Be honest with users: tier 1 ships, tier 2 grows kernel-by-kernel, tier 3 is a program.

---

## 1. The interception surface (what to shim in the guest)

CUDA in a Linux container is a stack of user-space `.so`s over the kernel driver. dd replaces the
**bottom user-space layer and up**, never the kernel ABI.

| Component (guest `.so` / node) | Role | dd strategy |
|---|---|---|
| **`libcuda.so.1`** (Driver API) | `cuInit`, `cuDevice*`, `cuCtx*`, `cuStream*`, `cuMemAlloc`, `cuMemcpy*`, `cuModuleLoadData` (PTX/cubin), `cuLaunchKernel`. The lowest stable CUDA ABI. | **Primary shim — exists** (`dd-gpu/cuda/`, C, cross-built aarch64+x86_64 like `dd-nvml`). Implements the Driver-API subset above; ingests PTX; runs kernels on the embedded software oracle today (numerically-verified vecadd), and on a real host forwards `dd-gpu` IR to the Metal executor. Unimplemented tail returns `CUDA_ERROR_NOT_SUPPORTED`. This is the ZLUDA seam. |
| **`libcudart.so`** (Runtime API) | `cudaMalloc`, `cudaMemcpy`, `cudaLaunchKernel`, the `<<<>>>` launch glue, device management. Sits *on top of* the driver API. | Shim too (some apps statically link cudart and call the driver for kernels; some call cudart only). Thin — mostly re-expresses onto our driver shim. |
| **`libnvrtc.so`** (runtime compiler) | CUDA C++ → PTX at runtime. **PyTorch/JAX JIT many kernels here.** | Shim: hand the CUDA C++ to our PTX→SPIR-V front (or a CUDA-C++→MSL path). Critical for frameworks. |
| **`libnvidia-ml.so`** (NVML) + `nvidia-smi` | Device enumeration, name, memory, utilization, temps. | Shim NVML to report the simulated device (`dd-gpu::cuda::CudaDeviceDesc`). `nvidia-smi` is just an NVML client. |
| **`libcublas`, `libcudnn`, `libcufft`, `libcusparse`, `libcusolver`, `libcurand`, `libnccl`** | Closed, hand-tuned BLAS/DNN/FFT kernels — **where ML spends its time.** | **Redirect, don't translate**: reimplement the API surface backed by **Apple MPS/MPSGraph/MLX**. Large but bounded; the only path to framework perf. (tier 3) |
| **`/dev/nvidia0`, `/dev/nvidiactl`, `/dev/nvidia-uvm`** | Kernel-driver device nodes the *real* libcuda ioctls. | **Synthesize nodes** (dd's `openat`/`/dev` handler) for presence/`stat`; our shim libcuda never issues the real ioctls, so we don't emulate them. |
| **`/proc/driver/nvidia/*`, `/sys/.../nvidia`** | Some tools read version/UUID here. | Synthesize a minimal tree (version, GPU dir) mirroring `CudaDeviceDesc`. |

**Delivery of the shims:** dd already controls the guest's dynamic linker environment and syscall
personality. Inject the dd shim `libcuda.so.1` ahead of any installed one (rootfs placement +
`ld.so.conf`/`LD_LIBRARY_PATH`; `LD_PRELOAD` for a hard override). The "install CUDA drivers and they
work" experience = *the toolkit installs normally; dd's driver shim is what actually answers.*

**Why user-space (like ZLUDA), not kernel-driver emulation:** the CUDA **Driver API** is a documented,
stable, user-space contract; the `/dev/nvidia*` **ioctl** protocol is undocumented, versioned to the
exact driver build, and colossal. Every serious "CUDA elsewhere" project (ZLUDA, chipStar) intercepts at
`libcuda`. dd does the same, then forwards to Metal instead of Level Zero/ROCm.

---

## 2. Prior art — study and tradeoffs

| Project | Approach | What it proves / where it stops | Lesson for dd→Metal |
|---|---|---|---|
| **ZLUDA** | Drop-in `libcuda.so`; **PTX → target ISA** (originally Intel **Level Zero/SPIR-V**, later AMD **ROCm**). | Ran real apps (Blender CUDA, some ML/GEMM) unmodified on non-NVIDIA GPUs — *app-by-app*, never the whole ecosystem. Tangled NVIDIA-EULA/relicensing history. | **Validates the exact architecture** (libcuda shim + PTX recompile). Its target ISAs both **ingest SPIR-V**; Metal does **not** — dd needs an extra SPIR-V→MSL hop. |
| **SCALE** (Spectral Compute) | **Compiler**: builds CUDA C++ *source* directly for AMD (nvcc-compatible front end) + reimplemented CUDA libs. | Source-level → cleaner semantics than PTX round-tripping, but needs the build path; AMD-targeted. | Argues for grabbing **CUDA C++ at NVRTC/nvcc time** when source exists (better than PTX), with PTX as the fallback for precompiled cubins. |
| **gpuocelot** | Research **PTX emulator/JIT**: PTX → LLVM → NVIDIA/AMD/**x86 CPU**. | Proved **PTX→LLVM→any-backend** is tractable; but CUDA-4-era, unmaintained. | The **PTX→LLVM→backend** shape is sound; AIR is LLVM-based, but its bitcode is closed → we still exit through **MSL text**, not AIR directly. |
| **chipStar / HIPCL, POCL** | CUDA/HIP → **SPIR-V** → OpenCL/Level Zero. | More evidence SPIR-V is the portable compute IR for non-NVIDIA. | Reinforces **SPIR-V as dd's kernel ABI**; Metal is the odd one needing SPIRV-Cross. |
| **Apple/NVIDIA history** | NVIDIA dropped macOS CUDA after macOS 10.13; no modern native path. | There is **no** off-the-shelf CUDA-on-Apple to lean on. | dd is building the missing seam; expect to own the whole PTX→MSL + library-redirect stack. |

**Why Metal is a *harder* target than AMD/Intel:**
- **No runtime SPIR-V/PTX ingest.** Intel (Level Zero) and AMD (ROCm/HIP) consume SPIR-V/LLVM directly.
  Metal consumes **AIR**, produced by Apple's compiler from **MSL text** (or a precompiled `.metallib`).
  So dd's kernel path is `PTX → SPIR-V → MSL → AIR` — one more lossy hop (SPIRV-Cross→MSL) than ZLUDA
  needs, with MSL semantic gaps.
- **Graphics-first API.** Metal *has* first-class compute (`MTLComputePipelineState`, `dispatchThreadgroups`),
  but the surrounding model (address spaces, no arbitrary function pointers, limited recursion, static-ish
  threadgroup memory, no Volta-style independent thread scheduling) is narrower than CUDA's.
- **Closed shader ABI.** AIR/`.metallib` internals are undocumented, so you cannot emit AIR directly and
  must go through MSL, inheriting the Metal front end's restrictions.
- **The upside:** Apple GPUs' **SIMD-group width is 32 = CUDA `warpSize`**, and **unified memory** makes
  the CUDA copy model nearly free — two genuine structural wins (see §4, §6).

---

## 3. API mapping — CUDA concepts → Metal

| CUDA | Metal | Notes / gaps |
|---|---|---|
| `CUdevice` / `cudaGetDeviceProperties` | `MTLDevice` | dd fabricates NVIDIA-looking props (`CudaDeviceDesc`) over the real `MTLDevice`. |
| `CUcontext` | implicit (device + queues) | CUDA contexts ≈ our per-container executor state. |
| `CUstream` / `cudaStream_t` | `MTLCommandQueue` (+ ordering) | Streams → queues; default stream = one implicit queue. In-stream ordering = command-buffer order. |
| `cudaMalloc` (device ptr) | `MTLBuffer` (`storageModeShared` on Apple Silicon) | **Unified memory** → the buffer's bytes are CPU+GPU visible; back the guest device pointer with the *same pages* → §4. |
| `cudaMallocManaged` (UVM) | `MTLBuffer` shared | Managed memory is the *natural* Apple-Silicon case. |
| `cudaMemcpy H2D/D2H` | `memcpy` / no-op | On unified memory, largely **zero-copy** (§6) — a headline win over discrete NVIDIA. |
| `cudaMemcpy D2D` | blit encoder copy | `MTLBlitCommandEncoder`. |
| `cudaEvent_t` / `cudaEventRecord` | `MTLSharedEvent` / `MTLFence` | Timeline sync → `dd-gpu` `Fence` (`WaitFence`). |
| `cudaStreamSynchronize` | `commandBuffer.waitUntilCompleted` | Maps to `WaitFence`. |
| `__global__` kernel launch `<<<grid,block,shmem,stream>>>` | `dispatchThreadgroups(grid, threadsPerThreadgroup=block)` | `dd-gpu` `Dispatch{grid}` + a compute pipeline whose `local_size` = `block` (baked into the translated shader). |
| **grid / block / thread** | **grid(threadgroups) / threadgroup / thread** | Direct structural match. |
| `warp` (32) | **SIMD-group (32 on Apple GPU)** | Convenient equality; but `__shfl_*`/`__ballot`/`__activemask` → `simd_shuffle*`/`simd_ballot` with different semantics/edge cases. |
| `__shared__` | `threadgroup` address space | Static size maps cleanly; **dynamic shared memory** (`extern __shared__`) is awkward in MSL → a gap. |
| `__syncthreads()` | `threadgroup_barrier(mem_threadgroup)` | OK. |
| `__constant__` | `constant` address space | OK. |
| global memory | `device` address space | OK; pointer casts across address spaces are restricted in MSL. |
| atomics (`atomicAdd`, 64-bit, system-scope) | `atomic_*` in `metal::` | Partial: not all types/scopes; 64-bit atomics HW-dependent. |
| `printf` in kernel | (none native) | Gap; needs a buffered emulation. |
| dynamic parallelism, device-side launch | (none) | **Unsupported** on Metal — hard stop for kernels that use it. |

---

## 4. Memory model — the zero-copy win (and its caveats)

On a **discrete NVIDIA** GPU, `cudaMalloc` is VRAM and `cudaMemcpy` is a PCIe DMA. On **Apple Silicon
unified memory**, an `MTLBuffer(storageModeShared)` is one allocation visible to **both** CPU and GPU.

dd exploits this: `cudaMalloc(n)` →
1. host executor allocates `MTLBuffer(length:n, options:.storageModeShared)`;
2. dd maps that buffer's backing into the **guest** address space (via the same shm/IOSurface handoff
   the graphics path uses — see `RENDERING_GPU_BACKENDS.md` §Optimization), returning a guest **device
   pointer** into those exact pages;
3. `cudaMemcpyHostToDevice` becomes a `memcpy` **within the guest** (or *nothing* if the app wrote
   straight into the mapped region) — no cross-device copy, no readback.

This is modeled today in `dd-gpu::cuda::CudaContext`: `mem_alloc` returns a `DevicePtr` bound to a
`BufferId`; `memcpy_htod` emits `WriteBuffer`; `resolve(ptr+off)` does pointer arithmetic back to
`(BufferId, offset)` so a D2H read is `GpuBackend::read_buffer`. The `cuda_memory_path_runs_on_software_backend`
test runs the whole alloc→H2D→D2H loop headless.

**Caveats requiring a real Mac to validate:**
- **`MTLBuffer(bytesNoCopy:)` alignment**: 16 KiB page-aligned base, page-multiple length, single VM
  region — same constraints as the graphics shm path (`RENDERING_PLAN.md` risk #3).
- **Flat unified addressing**: CUDA's 64-bit unified VA lets a kernel dereference *any* device pointer;
  Metal buffers are **separate** allocations bound to argument slots. Kernels that chase pointers across
  allocations (linked structures, pointer-heavy code) don't fit the per-buffer binding model. Mitigations:
  one large arena `MTLBuffer` sub-allocated (fake a flat space), or Metal **argument buffers** / bindless.
  This is a real semantic gap, not just tuning.
- **Coherence/visibility**: shared storage is coherent on Apple GPUs, but the guest↔host mapping must
  respect Metal's command-completion boundaries (a fence before the CPU reads results).

---

## 5. The hard core — PTX → Metal

The kernel *body* is the crux. A CUDA app ships either **PTX** (virtual ISA, forward-portable) or
**SASS/cubin** (real ISA, GPU-specific) — usually a fat binary with PTX for JIT. dd targets **PTX**
(SASS is per-arch machine code — reversing it is out of scope; force PTX JIT via the fat-binary PTX or
NVRTC).

**Chosen pipeline:** `PTX → SPIR-V (front end) → MSL (SPIRV-Cross/naga) → AIR (Apple Metal compiler)`.

- **PTX → SPIR-V** is the missing front end dd must own (or adapt from ZLUDA's `ptx` crate / an LLVM
  NVPTX-reverse). PTX is a typed virtual ISA; the structured pieces (arithmetic, control flow, address
  spaces, barriers, most intrinsics) map to SPIR-V compute. The **hard parts**: warp-level primitives
  (`shfl`, `vote`, `match`), `bar.warp`, dynamic shared memory, `printf`/`vprintf`, texture/surface
  instructions, inline PTX `asm`, and `__nv_*` device-function intrinsics.
- **SPIR-V → MSL** is well-trodden (**SPIRV-Cross**, **naga**) — dd reuses it, the same hop the graphics
  MetalBackend uses. MSL restrictions (address-space-qualified pointers, no recursion, limited function
  pointers) surface here as compile failures for the gnarlier kernels.
- **MSL → AIR** is Apple's closed compiler — reliable, but the input must be valid MSL.

**Where the translation runs:** either in the guest shim (guest emits SPIR-V into `dd-gpu` `CreateShader`)
or host-side in the Metal executor (guest forwards PTX, host translates). Host-side keeps the heavy,
Apple-specific toolchain (SPIRV-Cross + Metal compiler) off the guest and lets dd cache
`PTX-hash → .metallib`. **Recommendation: forward PTX, translate + cache host-side.**

**What the `dd-gpu` prototype does today:** it goes well beyond entry-name parsing. `dd-gpu/src/ptx.rs`
is a real **PTX → dd-GPU kernel-IR front-end**: it parses a bounded, tested subset of PTX (special
registers `tid`/`ctaid`/`ntid`/`nctaid`, `ld.param`, `cvta.to.global`, integer + f32 ALU incl.
`mad`/`mul.wide`/`fma`, `setp` + predicated `bra` for the bounds guard, `ld.global`/`st.global`, `ret`),
classifies pointer parameters, and lowers to a compact op list; malformed/unsupported PTX is a typed
`GpuError::Ptx`. `CudaContext::launch` compiles the kernel (cached per `(module, entry, block)`), packs
the flat kernel-parameter blob, binds the pointer buffers, and emits a compute `Dispatch`. The
`SoftwareBackend` **actually executes** that kernel per-thread over the launch grid and writes results
back — so `vecadd` runs numerically correct end-to-end here (the oracle). The **PTX→SPIR-V→MSL** path
for the *Metal* backend remains the research-grade, Mac-host core; the software interpreter is its
standing correctness oracle. The honest boundary now: **which PTX we model** (the long tail — warp
intrinsics, shared memory, atomics, f64, textures — is unmodeled and rejected), not "compute doesn't run."

### 5.2 The `GpuBackend` seam — software oracle today, Metal drop-in later

The whole point of routing through `dd-gpu` IR + the `GpuBackend` trait is that **`software.rs` and a
future `metal.rs` are interchangeable behind one trait**. A `cuLaunchKernel` lowers to the *same* dd-GPU
IR regardless of backend: `CreateShader` (the kernel module), `CreateComputePipeline`, a `CreateBuffer`
+ `WriteBuffer` for the parameter blob, a `CreateBindGroup` over the arguments, and a `Submit` carrying
one `Dispatch`. The only per-backend difference is the **shader module's ABI**, and that is made
explicit and self-describing:

- **Software oracle:** `CreateShader.spirv` words carry a **kernel descriptor** (magic `0xDD6B0001` +
  the forwarded PTX text + entry + block dims). `SoftwareBackend::create_shader` compiles it via
  `ptx::compile` and interprets it on the CPU.
- **Metal host (future):** the same `CreateShader` slot instead carries real **SPIR-V** (produced by the
  host-side PTX→SPIR-V front end); `MetalBackend::create_shader` transpiles SPIR-V→MSL→AIR. A backend
  validates the magic to know which ABI it holds.

Everything else — the resource table, the memory model, the bind-group argument convention (binding 0 =
flat parameter blob, bindings 1..=k = per-pointer storage buffers, mirroring Metal's per-argument
`device T*` bindings), fences, dispatch — is backend-agnostic and already exercised headless. Dropping
in Metal is: implement `GpuBackend` for `MetalBackend` and hand it SPIR-V instead of the descriptor. The
guest `libcuda` shim, `CudaContext`, IR, wire, and ring do **not** change.

**Unified-memory note in the C `libcuda`:** because Apple-silicon `cudaMalloc` returns host-visible
unified memory, the C shim models a device pointer as a **real host pointer** (`cuMemAlloc` = `malloc`,
H2D/D2H = `memcpy`), so a kernel dereferences device addresses directly — no per-buffer rebasing. The
Rust `SoftwareBackend` instead uses the per-binding region model (each pointer arg is its own storage
buffer), which is the shape a Metal backend needs. Both compute the same result; the divergence is the
documented CUDA-flat-VA-vs-Metal-per-binding gap from §4, made concrete.

### 5.1 "ZLUDA is in Rust — can we extract and reuse it directly?"

**Yes, partially — reuse its front half, replace its back half.** This is the single biggest head-start
available and it maps cleanly onto the architecture above.

- **Reuse (the crown jewels):** ZLUDA is Rust + LLVM and its shape is *exactly* ours — a drop-in
  `libcuda.so` implementing the CUDA **Driver API**, plus a real, maintained **PTX front end** (its
  `ptx` / `ptx_parser` crates: **PTX → LLVM IR**). Owning a correct PTX parser + lowering is the hardest,
  most error-prone piece of §5; ZLUDA hands it to us. Its **libcuda dispatch skeleton** and CUDA FFI/type
  definitions are also directly liftable, saving months of boilerplate.
- **Replace (the backend):** ZLUDA lowers **to AMD (ROCm/AMDGPU LLVM)** and formerly **Intel (Level
  Zero/SPIR-V)** — it has **no Metal backend**, and its runtime/memory layer assumes ROCm/HIP. dd swaps
  that out: take ZLUDA's **PTX → LLVM IR**, then add **LLVM IR → SPIR-V** (Khronos `LLVM-SPIRV`
  translator) **→ MSL** (SPIRV-Cross) **→ AIR** (Apple's compiler); and route its runtime/memory calls to
  dd's **`dd-gpu` IR + `MetalBackend`** instead of ROCm. So we keep ZLUDA's *front* (libcuda + PTX→LLVM)
  and graft dd's *back* (LLVM→SPIR-V→MSL + Metal executor + unified-memory model).
- **Caveats / due diligence:**
  - **Heavy build**: ZLUDA drags in **LLVM** (a large native dependency) — it will **not** build on this
    crate-fetchless Linux box; it is a **Mac-host build**, alongside the Metal executor.
  - **Licensing/optics**: ZLUDA is open (MIT/Apache-dual in its current AMD-revived form), but it has a
    **fraught history** (funded then dropped by Intel and AMD; an NVIDIA-EULA takedown scare around
    *distributing NVIDIA-derived* artifacts, not ZLUDA's own clean code). **Get a legal read before
    shipping** anything derived from it.
  - **Coverage still a long tail**: reusing ZLUDA does **not** change the §7 feasibility tiers — its PTX
    coverage is broad but not total, and it does **nothing** for the closed **cuBLAS/cuDNN** libraries
    (tier 3 still needs the MPS/MPSGraph redirect). It accelerates tiers 1–2, not tier 3.
- **Verdict:** **fork ZLUDA's libcuda + PTX→LLVM front end; discard its AMD/Intel backend for a
  LLVM→SPIR-V→MSL Metal path feeding `dd-gpu`.** This is the recommended way to reach tier-2 custom-kernel
  execution far faster than writing a PTX front end from scratch — provided the LLVM build lives on the
  Mac host and the licensing review clears.

---

## 6. Optimization — where the wins are

Shared substrate with the graphics path (details in `RENDERING_GPU_BACKENDS.md §Optimization`):

- **Zero-copy unified memory (biggest win).** `cudaMemcpy*` H2D/D2H → `memcpy`-or-nothing on Apple
  Silicon (§4). On discrete NVIDIA these are PCIe-bound; dd deletes them. *Quantify on a real Mac.*
- **Shared-memory command ring + batching.** Many CUDA calls per frame (`cuLaunchKernel` streams) are
  batched into one ring flush, not one socket round-trip each; a doorbell (futex/eventfd) wakes the host
  (`dd-gpu::ring` models the SPSC framing + backpressure). Coalesce consecutive `WriteBuffer`s and
  same-pipeline dispatches.
- **Async submission + fence pacing.** Don't block the guest on every launch; only `cudaStreamSynchronize`
  / `cudaEventSynchronize` waits (a `WaitFence`). Keep the Metal queue full.
- **Pipeline / `.metallib` cache.** `PTX-hash → MTLComputePipelineState` cached across launches (the
  prototype's pipeline cache: repeat launches of a kernel emit **no** new shader/pipeline — see
  `cuda_launch_emits_compute_dispatch`).
- **Residency / avoid readback.** Keep `MTLBuffer`s resident; never round-trip results to host unless the
  app copies them (`MTLResidencySet` on newer OSes).
- **SIMD-group width 32 = warp 32** → warp-tuned kernels keep their occupancy assumptions.

---

## 7. Feasibility verdict + honest phasing

Measured against the **ML-workflow** north star:

**Tier 1 — Device presence & memory (realistic now; partly prototyped headless).**
`cuInit`, device enumeration, `CudaDeviceDesc` via NVML/`nvidia-smi`/`cudaGetDeviceProperties`,
`cudaMalloc`/`cudaFree`/`cudaMemcpy` on unified memory. Outcome: `torch.cuda.is_available() == True`,
`nvidia-smi` lists the device, tensors *allocate* on "cuda". **Caveat:** `is_available()` passing is not
`is_useful()` — the first real kernel is tier 2.

**Tier-1 status — the NVML shim exists (`dd-gpu/nvml/`).** `dd-gpu/nvml/libnvidia-ml.so.1` is a real C
implementation of the documented public NVML ABI (versioned symbols `nvmlInit_v2`,
`nvmlDeviceGetCount_v2`, `nvmlDeviceGetHandleByIndex_v2`, `nvmlDeviceGetMemoryInfo(_v2)`,
`nvmlDeviceGetCudaComputeCapability`, `nvmlDeviceGetPciInfo_v3`, `…RunningProcesses_v3`, temps/power/clocks,
PCIe link, architecture, `--query-gpu` fields, etc.), seeded from `DD_CUDA_NAME`/`DD_CUDA_CC`/`DD_CUDA_VRAM`;
unimplemented queries return `NVML_ERROR_NOT_SUPPORTED`. It cross-builds to Linux aarch64 + x86_64 and passes
a `dlopen` ABI test that drives the exact `nvidia-smi` sequence with no GPU. The `dd-cli` launcher injects it
(+ the real, user-supplied `nvidia-smi`) when a workspace has a `cuda` device
(`ddcli workspace create … --cuda`).

**The stock NVIDIA `nvidia-smi` binary runs against our shim and reports the dd device** — verified by
running the genuine `nvidia-smi 535.230.02` (aarch64) directly against `dd-gpu/nvml/libnvidia-ml.so.1`:

    $ nvidia-smi -L
    GPU 0: dd Metal (CUDA-sim) Device (UUID: GPU-dd000000-0000-4d64-0000-000000000000)
    $ nvidia-smi --query-gpu=name,driver_version,pstate,memory.total,utilization.gpu,temperature.gpu,power.draw,compute_cap --format=csv,noheader
    dd Metal (CUDA-sim) Device, 535.230.02, P0, 4096 MiB, 0 %, 35, 25.00 W, 8.6

Standard NVML clients (gpustat, `pynvml`/`nvidia-ml-py`, `torch`'s NVML probe, `nvitop`) work the same way —
all via the public API.

**The one remaining gate — the DEFAULT dashboard's private internal ABI (the "dark API").** `nvidia-smi`
with no args (and `nvidia-smi -q`) does NOT use the public API for its device table; it drives NVIDIA's
*private, undocumented* `nvmlInternalGetExportTable` (table GUID `c4fe3e6c-c98f-6c4e-a327-ee696e12f7c4`).
Reverse-engineered behaviour (dumping the REAL `libnvidia-ml.so.535.230.02`'s table + tracing the stock
binary):
- The table is 245 × 8-byte slots: `slot[0]` = header `0x7a8` (table byte size); slots `[1],[2]` and ~33
  others are NULL; the rest are function pointers (some the public `nvmlDevice*` symbols, most internal
  statics). Our shim now returns this **exact shape** with populated slots as `NOT_SUPPORTED` stubs — that
  clears the handshake and steers `-L`/`--query-gpu` onto the public API (above).
- The default dashboard + `-q` run through **nvidia-smi's own version-gated C++ command dispatcher (binary
  offset `0x447558`)** — it caches a per-NVML-version handler pointer and tail-calls it; that handler's
  render pipeline consumes the private internal-table slots directly (device **count** + **handle** still
  come from the *public* API — confirmed by ptrace-tracing the stock binary). With our slots as
  `NOT_SUPPORTED` stubs the handler returns `NVML_ERROR_UNKNOWN (999)` → "Internal NVML error".
- **Slot contract, partially reversed** (disassembling the real lib's slot functions + ptrace): e.g.
  **`slot[81]` is get-PCI-bus-id-by-index** (real lib `+0x34b88`:
  `snprintf(buf, size, "%08x:%02x:%02x.0", domain, bus, dev)`; args = index / out-buffer / buffer-size).
  The full dashboard pulls many more private slots, each with its **own** undocumented arg/output-struct
  signature — returning a blind success from all of them **SIGSEGVs** the stock binary, proving they are not
  uniform. Reproducing the whole pipeline is slot-by-slot RE of NVIDIA's closed internal device-info ABI
  from a stripped binary. (Populating a *partial* set only makes the render crash/misbehave, so the shim
  keeps every slot `NOT_SUPPORTED` — clean public-API fallback + clean failure on the default box.)
- **ZLUDA does NOT crack this**: its `zluda_ml` implements a minimal public NVML and returns
  `NOT_SUPPORTED` for `nvmlInternalGetExportTable` (ZLUDA's `dark_api` is the *CUDA driver*'s
  `cuGetExportTable`, not NVML); the GUID `c4fe3e6c…` appears nowhere in ZLUDA. No public source documents
  the NVML internal device-struct ABI.

So: **the stock `nvidia-smi` reports the dd device for real in its list/query modes; the default full-screen
dashboard is gated on NVIDIA's closed internal device-info ABI** — the same closed-ABI class this doc's
TL;DR scopes out (cf. `/dev/nvidia*` ioctls), reproducible only by hand-implementing nvidia-smi's private,
driver-build-locked internal table + validator. (`torch.cuda.is_available()` and NVML clients do not depend
on it.)

**Tier 2 — Custom / simple PTX kernels (feasible, long tail; the entry rung is now proven headless).**
PTX→SPIR-V→MSL for hand-written kernels (`saxpy`, elementwise, reductions, tiled GEMM). Many work; each
warp-intrinsic / dynamic-shared-mem / atomic / `printf` kernel is a per-kernel battle. This is a
*capability that grows*, never a clean "done." Matches ZLUDA-on-AMD's lived reality.

**Tier-2 status — the vector-add entry rung executes end-to-end on the software oracle.** `dd-gpu`'s
PTX front-end (`ptx.rs`) compiles the modeled subset (global-index + guarded `ld/st.global` +
integer/f32 ALU) and the `SoftwareBackend` runs it per-thread over the launch grid; `vecadd` is
numerically verified through both the Rust `CudaContext → IR → backend` path and the `libcuda.so.1`
dlopen ABI test. This validates the *whole architecture* — `libcuda → PTX → dd-GPU IR → GpuBackend` —
so the Metal backend is a trait swap (§5.2). **What this is not:** the modeled PTX is deliberately
narrow (no warp intrinsics, shared memory, atomics, f64, textures, inline `asm`, dynamic parallelism —
all rejected with a typed error), and the real-GPU path still needs the host-side PTX→SPIR-V→MSL front
end. The oracle is the yardstick that the future Metal output must match, not the Metal path itself.

**Tier 3 — Real framework performance (large; partly research-grade).**
PyTorch/TF performance is in **closed cuBLAS/cuDNN/cuFFT** kernels that **cannot** be translated. The
viable road is **API redirection to Apple MPS/MPSGraph/MLX** (reimplement the cuBLAS/cuDNN/cuFFT surfaces
over Metal's own tuned kernels), plus an **NVRTC** shim for JIT'd kernels. This is a multi-quarter program
per library, not a translation. A blunt strategic note: because PyTorch already has a **native Metal (MPS)
backend**, a *pragmatic alternative* to full CUDA emulation for the ML use-case is to steer the container's
framework onto a Metal-backed device — but that is not "a CUDA device," which is what was asked, so the
CUDA-shim road above is what this doc commits to, with library-redirect as the perf mechanism.

**Bottom line to set expectations:** dd can *show a convincing CUDA device and run real memory + many
custom kernels* on Metal in a tractable timeframe; making *arbitrary unmodified PyTorch training fast and
correct* is a research-grade, ongoing effort gated on the cuBLAS/cuDNN→MPS redirect. Ship tier 1, grow
tier 2, treat tier 3 as a roadmap, and say so in the UI.

---

## 8. How this maps onto dd (concrete seams)

- **Guest shims** (`os/linux/` + rootfs): dd `libcuda.so.1` (**built — `dd-gpu/cuda/`**) / `libcudart` /
  `libnvidia-ml` (**built — `dd-nvml/`**) / `libnvrtc`; synthesized `/dev/nvidia*` + `/proc/driver/nvidia`.
  Each call → `dd-gpu` IR onto the command ring. **Launcher injection is designed-not-wired:** the
  `dd-cli` launcher already injects `dd-nvml`'s `libnvidia-ml.so` for `--cuda` workspaces; injecting
  `libcuda.so.1` the same way (rootfs placement + `ld.so.conf`/`LD_LIBRARY_PATH`, seeded `DD_CUDA_*`,
  `build.sh install` → `~/.dd/cuda/<arch>/`) is the mirror-image follow-up and is intentionally left to
  the launcher owner — this work stays inside `dd-gpu/` and does not touch `ddjit_launcher.rs`.
- **Transport**: the shared-memory command ring + DDP-socket control/fences (`dd-gpu::ring`, and
  `RENDERING_PLAN.md §6`), reusing the graphics forwarding channel.
- **Host executor** (`dd-display`/sibling `dd-gpu` service): the **`MetalBackend`** (feature `metal`,
  mac-only) implementing `dd_gpu::GpuBackend` — resource table, `MTLBuffer`/`MTLTexture`/compute
  pipelines, `PTX/SPIR-V → MSL → .metallib` cache, `MTLSharedEvent` fences, unified-memory mapping.
- **Model + UI**: a per-workspace **Device** setting (simulated-CUDA toggle + device name / compute
  capability / VRAM) persisted in `workspaces.conf`, surfaced in the dd GUI **Device** tab, which arms
  the shim injection + Metal forwarding at launch. (Model/UI plumbing is dependency-light and built now;
  see the `Workspace` `cuda` field and the term.rs Device section.)
- **CUDA *host* case (secondary):** on an NVIDIA host the same `dd-gpu` IR is served by a `CudaBackend`
  (native Vulkan for graphics + CUDA interop for compute) — that is the *host*-agnostic axis of
  `RENDERING_GPU_BACKENDS.md`, orthogonal to *simulating* CUDA for the guest here.

## 9. What needs a real Mac / GPU to validate (assumptions)

1. `PTX → SPIR-V` coverage on real kernels (the front end dd must build/adapt) — **unproven here**. What
   *is* proven here: a `PTX → dd-GPU kernel-IR` front end for the modeled subset, executed on the CPU
   oracle (`vecadd` numerically verified). The SPIR-V/MSL emission for Metal, and coverage beyond the
   modeled subset, remain the Mac-host research-grade work.
2. `SPIR-V → MSL` (SPIRV-Cross) fidelity for compute intrinsics (warp ops, atomics, shared mem).
3. Unified-memory `MTLBuffer` mapped into the guest VA with correct alignment/coherence (§4 caveats).
4. `MTLSharedEvent` as the CUDA event/stream-sync primitive under real concurrency.
5. cuBLAS/cuDNN → MPS/MPSGraph numerical + perf parity (tier 3).
6. Whether unmodified `libcudart`/frameworks tolerate the driver-shim's version/UUID answers.

All CUDA/Metal execution is **mac/GPU-only**; on this Linux host only the IR, wire, resource table,
ring, device-presence data, PTX entry parsing, and the memory-path translation are built and tested.
