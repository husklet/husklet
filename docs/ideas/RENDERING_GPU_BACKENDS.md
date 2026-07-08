# dd GPU backends — the host-GPU-agnostic forward IR + `GpuBackend` abstraction

Status: **design + dependency-light prototype.** Resolves the "Open question, validate first" that
[`RENDERING.md`](RENDERING.md) §GPU and [`RENDERING_PLAN.md`](RENDERING_PLAN.md) §6 left as rung-3's
pivotal fork: **at what level do we forward the guest GPU stream, such that the SAME stream replays on
Apple Metal *and* on an NVIDIA/CUDA host?** Companion doc [`CUDA_ON_METAL.md`](CUDA_ON_METAL.md) covers
the *compute*/CUDA-device-simulation story that rides this same substrate.

The executable artifact is the **`dd-gpu`** crate (`../../dd-gpu`): the IR + wire (de)serialization +
resource-handle table + `GpuBackend` trait + a recording mock backend + a real CPU software backend + a
command-ring model + the CUDA translation shim — **pure `std`, no serde, hand-rolled**, so it builds and
`cargo test`s **headless on the Linux dev host** (17 tests + a doc-test, no GPU/display/CUDA/network).
The real `MetalBackend`/`CudaBackend` are feature-gated and built on their respective hosts.

---

## 1. The forward-level decision: Metal vs Vulkan vs a neutral dd-GPU IR

The requirement — *one guest-side producer, replayable on both a Metal executor and a CUDA/NVIDIA
executor* — is the whole ballgame. Three candidate levels:

### (a) Metal-level forwarding — **rejected for the agnostic goal**
Guest does Vulkan→Metal (KosmicKrisp guest-side), forward **Metal** commands; host re-issues Metal.
- On a Mac: near-trivial.
- On an NVIDIA host: you would need **Metal→CUDA**, which means reimplementing Metal's *entire graphics
  pipeline* (render passes, rasterization, blending, samplers, MSL shaders) on top of CUDA — CUDA is
  **compute-first**; graphics only exists via CUDA↔Vulkan/GL **interop**. That is writing a Metal driver
  on CUDA: **research-grade, not feasible.** Also Metal has **no mature remoting/serialization** protocol.
- **Verdict: fails the "same stream on both" test at the first hurdle.**

### (b) Vulkan-level forwarding (Venus/virtio-gpu style) — **strong, the pragmatic near-universal**
Guest emits serialized Vulkan (SPIR-V shaders untranspiled); host is *any conformant Vulkan driver*.
- On a Mac: **MoltenVK** or **KosmicKrisp** → Metal.
- On an NVIDIA host: the **native NVIDIA Vulkan driver** — first-class, fully conformant, sits directly
  on the GPU. "CUDA host" really means "a machine with an NVIDIA GPU," whose *graphics/Vulkan* is native
  and whose *CUDA* enters only for compute interop. **No Metal→CUDA translation is ever needed.**
- Fallback: **lavapipe** (software Vulkan), runs on arm64.
- This is exactly what `RENDERING_PLAN.md` already picked (Venus/vtest). Pros: near-native, reuses Vulkan
  conformance, SPIR-V passes through. Con: the "backend" is effectively fixed to *"something that speaks
  Vulkan"* — a **direct-Metal** backend (no MoltenVK, best Mac perf) or a **compute-only CUDA** backend
  doesn't fit cleanly, and the executor abstraction becomes "a Vulkan device," which is leaky.

### (c) A neutral mid-level dd-GPU IR (WebGPU/wgpu-hal-shaped) — **the committed ABI shape**
Define dd's own compact command IR — device/resource/pipeline/render-pass/compute-pass/draw/dispatch/
present — with **SPIR-V as the shader ABI**, and let each host implement it. This is precisely the
**WebGPU / wgpu-hal `Api`** model, which is the *proven* common denominator of Metal + Vulkan + D3D12 +
GL, already has a Metal backend, a Vulkan backend, and software options, and ships a cross-backend shader
compiler (naga / SPIRV-Cross). We are **not inventing a graphics model** — we adopt the one that
demonstrably maps to both Metal and Vulkan.

**Recommendation — commit to (c) as the ABI, implement it leaning on (b) where that's fastest to
correctness:**

> **dd-GPU IR = a WebGPU/Vulkan-family command IR with SPIR-V shaders.** It is a normalized *subset* of
> Vulkan expressed in dd's own stable wire format (not raw Vulkan structs, not Metal). The `GpuBackend`
> trait is expressed in this IR's vocabulary, so each host backend is a real, pluggable implementor:
>
> - **`MetalBackend`** (Apple host): IR → Metal directly; SPIR-V → MSL/AIR via SPIRV-Cross. Best Mac perf
>   (no MoltenVK layer). *This is also the host executor for CUDA-on-Metal — see `CUDA_ON_METAL.md`.*
> - **`VulkanBackend`** (bring-up + NVIDIA host): IR → Vulkan 1:1. On NVIDIA it's the native driver; on a
>   Mac it's MoltenVK/KosmicKrisp — a fast route to first-correctness before `MetalBackend` matures.
> - **`CudaBackend`** (NVIDIA "CUDA host", secondary): graphics via native Vulkan interop, compute via
>   CUDA; in practice a thin specialization of `VulkanBackend` + `VK_KHR_external_memory`/CUDA interop.
> - **`SoftwareBackend`** (standing fallback the architecture mandates): lavapipe on a real host; a
>   hand-rolled CPU executor (clear/copy/readback) in the prototype.

**Why (c) over (b), given the CUDA-host mandate:** the neutral IR (1) keeps the `GpuBackend` trait
**honest and pluggable** instead of "a Vulkan passthrough"; (2) lets the Mac path go **direct to Metal**
for performance *and* serve the CUDA-on-Metal compute forwarding with the same executor; (3) folds NVIDIA
in as "just another backend"; (4) leaves the door open to a compute-only CUDA backend and the software
fallback. The cost — *defining the IR* — is paid once and is prototyped now. **Honest caveat:** for a
*Mac-only* world, plain Vulkan-passthrough (b) is less code; the neutral IR earns its keep precisely
because the mandate is host-agnostic (Metal **and** CUDA) and because the same IR carries the CUDA-device
simulation.

**Verdict on true Metal→CUDA:** *not recommended and not needed.* Agnosticism is achieved by choosing an
**IR both backends can implement** (WebGPU-shaped, SPIR-V shaders) — the common denominator is the IR,
**not** a Metal-to-CUDA transpiler, which would be research-grade.

### The committed ABI (what to build against)
- **Wire:** length-prefixed little-endian frames in a shared-memory command ring; OOB handles (shm fd via
  `SCM_RIGHTS`, IOSurface mach-port, dma-buf fd) correlated by id. (`dd-gpu::wire`, `dd-gpu::ring`.)
- **Commands:** resource create/destroy (buffer/texture/sampler/shader/pipeline/bind-group/surface/fence),
  `WriteBuffer`, a `Submit` carrying a command-buffer of render/compute-pass encoder ops, `WaitFence`,
  `Present`. (`dd-gpu::ir::Cmd` / `Enc`.)
- **Shader ABI:** **SPIR-V** words (`CreateShader`). Metal transpiles to MSL; Vulkan/NVIDIA consume
  natively; CUDA compute kernels arrive as SPIR-V via `PTX→SPIR-V` (`CUDA_ON_METAL.md §5`).
- **Ids:** guest-assigned `u32` per kind; host keeps the id→object map (`dd-gpu::ResourceTable`).

---

## 2. The `GpuBackend` trait (host executor abstraction)

`dd_gpu::backend::GpuBackend` — object-safe (`&mut dyn GpuBackend`), dependency-free (no `ash`/Metal
types leak in, so a direct-Metal or CUDA-compute backend implements it without a Vulkan runtime). Shape:

```
trait GpuBackend {
    fn capabilities(&self) -> Capabilities;                 // name, unified_memory, compute/graphics, present_kinds
    // resources (guest-assigned ids; backend owns the id→object map)
    fn create_buffer / write_buffer / read_buffer / destroy_buffer
    fn create_texture / read_texture / destroy_texture
    fn create_sampler / destroy_sampler
    fn create_shader(id, spirv: &[u32]) / destroy_shader     // SPIR-V is the shader ABI
    fn create_render_pipeline / create_compute_pipeline / destroy_pipeline
    fn create_bind_group / destroy_bind_group
    fn create_surface / destroy_surface                      // presentation targets (one per DDP Surface)
    // sync
    fn create_fence / wait_fence / destroy_fence             // timeline fences (MTLSharedEvent / VkSemaphore)
    // work + present
    fn submit(&mut self, cb: &CommandBuffer)                 // a recorded render/compute command buffer
    fn present(&mut self, surface, texture) -> PresentToken  // hands a buffer to DDP (§3)
}
```

- **Resource table**: each backend uses a `ResourceTable<T>` per kind — enforcing *no duplicate create*,
  *no use-after-free*, *no double-free* at the trait boundary, turning what would be driver UB into a
  typed `GpuError`. (Tested: `handle_table_lifecycle`, `mock_backend_enforces_lifecycle_through_the_trait`.)
- **Surface between IR and trait**: `dd_gpu::replay::apply` decodes each `Cmd` and calls exactly one trait
  method; `Present` yields a `PresentToken { surface, kind: Shm|IoSurface|DmaBuf, handle, w, h }` the DDP
  layer attaches. So the IR is transport; the trait is the executor; `replay` is the only glue.
- **Implementors today**: `RecordingBackend` (records the exact replayed sequence — the test oracle) and
  `SoftwareBackend` (materializes buffers/textures in host memory; executes **clear** + buffer/texture
  **copies** + **readback**; records draws/dispatches). `MetalBackend`/`CudaBackend` are `#[cfg]`-gated
  stubs to be filled on-host.

---

## 3. Window rendering — presenting through DDP

The GPU backend's rendered target reaches the screen via the existing DDP `Surface`/`Buffer` path and the
`dd-display` compositor — the GPU path differs from the software path *only in buffer kind*:

- **`Present(surface, texture)`** → `PresentToken`. The backend's render target is already a presentable
  resource:
  - **Mac (`MetalBackend`)**: an **IOSurface-backed `MTLTexture`**. `present` returns
    `PresentKind::IoSurface` + the mach-port name; DDP emits `BUFFER_ATTACH(IOSURFACE)` + `SURFACE_COMMIT`;
    `dd-display` wraps it as an `MTLTexture` on the surface's `CAMetalLayer` (one `NSWindow`+`CAMetalLayer`
    per DDP surface) — **zero copy, no readback** (`RENDERING_PLAN.md §4.2`).
  - **Software / Linux (`SoftwareBackend`, lavapipe)**: a shm region → `PresentKind::Shm` →
    `BUFFER_ATTACH(SHM)`; `dd-display` maps it (`MTLBuffer(bytesNoCopy)` on Mac).
  - **CUDA/NVIDIA host (`CudaBackend`)**: a `VkImage` exported via `VK_KHR_external_memory_fd` (dma-buf) →
    `PresentKind::DmaBuf`; the host compositor (a Vulkan/Wayland/DRM-KMS `dd-display`, *not* Metal on that
    host) imports it zero-copy or presents via `VK_KHR_swapchain`. This is the **CUDA-host analog** of the
    Mac IOSurface path; CUDA-produced compute results reach it via `cudaExternalMemory`↔Vulkan interop.
- **Vsync/pacing**: `Present` correlates with DDP `FRAME_DONE`; the backend signals a fence at
  frame completion and the guest paces to it (§4).
- **One surface = one window**: `CreateSurface(ddp_surface)` binds a GPU surface to a DDP `Surface`, so the
  compositor's per-surface `NSWindow`/`CAMetalLayer` (Mac) or Vulkan swapchain (CUDA host) is reused.

The prototype models this with `SurfaceDesc { ddp_surface }`, `present()` returning a `PresentToken`, and
a format-match check (`software_backend_present_format_check`).

---

## 4. Optimization strategy ("optimize this nicely") — where the wins are

| Lever | Mechanism | Metal analog | CUDA-host analog | Prototyped |
|---|---|---|---|---|
| **Zero-copy buffers** (biggest) | Shared pages guest↔host; no upload/readback | `MTLBuffer(bytesNoCopy)` / IOSurface on unified memory | pinned/managed mem, `cudaHostRegister`, VK/CUDA external-memory interop | memory-path tests; caps `unified_memory` |
| **Shared-memory command ring** | SPSC ring in shm; producer batches, one doorbell (futex/eventfd) wakes host | gfxstream ring model | same | `dd-gpu::ring` (framing + backpressure) |
| **Batching / coalescing** | Many `WriteBuffer`/same-pipeline draws/dispatches per flush, not per call | one `MTLCommandBuffer` | one `cuStream` submit | ring frames; encoder is a batch |
| **Async submit + fence pacing** | Never block the guest except on explicit sync | `MTLSharedEvent` + `waitUntilCompleted` only on sync | `cudaEvent`/`cudaStreamSynchronize` | `Fence` / `WaitFence` in IR + backends |
| **Pipeline/state cache** | `shader/PTX hash → compiled pipeline`, reused across submits | `MTLComputePipelineState`/`.metallib` cache | pipeline cache / cubin cache | CUDA launch pipeline cache (test) |
| **Residency / no readback** | Keep targets resident; present in place, never copy to host | `MTLResidencySet`; IOSurface present | keep VkImage resident; dma-buf present | present returns handle, not pixels |
| **Damage-tracked present** | Only changed rects uploaded/composited | per-surface damage | same | (DDP `SURFACE_DAMAGE`) |

**Quantified expectation:** the dominant cost in a naive remoting design is per-call socket round-trips
and buffer copies. The ring (batching) removes the former; unified-memory zero-copy removes the latter.
On Apple Silicon, `cudaMemcpy`/texture uploads should approach *free* (same pages); the residual is
command encoding + fence latency. Absolute numbers need a real Mac (flagged §6).

---

## 5. Phased plan (slots into the existing M0–M5)

The graphics milestones M0–M3 (`RENDERING_PLAN.md §7`) are unchanged (they don't need the GPU backend).
This plan lands the GPU-backend substrate and threads it through M4–M5, and adds the CUDA track.

- **G0 — IR + trait + software backend (DONE here, headless).** `dd-gpu`: IR, wire round-trip, resource
  table, `GpuBackend`, `RecordingBackend`, `SoftwareBackend`, ring, CUDA shim. 17 tests + doc-test green
  on Linux. *This is the ABI other layers compile against.*
- **G1 — Metal executor skeleton (mac host).** `MetalBackend` (feature `metal`): `MTLDevice`/queue,
  `create_buffer/texture` (unified `storageModeShared`), `submit` for a clear pass, `present` via
  IOSurface. Validate against the `RecordingBackend`'s expected sequence, then a real triangle. *Lands in
  **M4** (IOSurface present, zero readback).*
- **G2 — SPIR-V shaders + real pipelines.** `create_shader(spirv)` → SPIRV-Cross → MSL → pipeline; render
  + compute pipelines; bind groups. A `vkcube`-equivalent through the IR. *Lands in **M5**.*
- **G3 — Vulkan backend (bring-up + NVIDIA).** `VulkanBackend` (feature `vulkan`): IR→Vulkan 1:1; on Mac
  via MoltenVK for cross-checking `MetalBackend`, on an NVIDIA host as the native path. *Enables the
  host-agnostic claim to be **tested**, once an NVIDIA host exists.*
- **G4 — CUDA-on-Metal, tier 1 (device presence + memory).** Guest `libcuda`/`libcudart`/NVML shims →
  `dd-gpu` IR → `MetalBackend`; `nvidia-smi`/`torch.cuda.is_available()` pass; `cudaMalloc`/`cudaMemcpy`
  on unified memory. Gated by the workspace **Device** setting. (`CUDA_ON_METAL.md §7` tier 1.)
- **G5 — CUDA tier 2 (PTX→SPIR-V kernels).** PTX front end + host `.metallib` cache; custom kernels run.
  Long tail, grows per-kernel.
- **G6 — CUDA tier 3 (library redirect).** cuBLAS/cuDNN/cuFFT → MPS/MPSGraph/MLX + NVRTC shim → framework
  performance. A program, not a milestone.

**Validation order (spike before committing host code):** (1) `MetalBackend` clear+IOSurface present on a
real Mac; (2) `MTLBuffer(bytesNoCopy)`/unified-memory mapping into the guest VA (alignment/coherence);
(3) SPIRV-Cross MSL fidelity for compute intrinsics; (4) `MTLSharedEvent` under real stream concurrency;
(5) PTX→SPIR-V coverage on real kernels; (6) on an NVIDIA host, IR→Vulkan parity vs Metal.

---

## 6. What is verified here vs designed vs unverifiable-on-this-host

- **Verified (built + tested headless on Linux):** the dd-GPU IR + wire round-trip (every command, plus
  per-frame ring framing, plus truncation/bad-tag rejection); the resource-handle table lifecycle; the
  recording mock backend's exact replay sequence + lifecycle enforcement through the trait; the software
  backend's clear/copy/readback + BGRA channel order + present format check; the command ring
  framing/backpressure; CUDA device-presence strings, PTX entry parsing, the alloc→H2D→D2H memory path,
  and launch→compute-dispatch with pipeline caching. **17 unit tests + 1 doc-test, 0 deps.**
- **Designed (not built here):** `MetalBackend`, `VulkanBackend`, `CudaBackend`; SPIR-V→MSL; PTX→SPIR-V;
  IOSurface/dma-buf present; the guest CUDA/NVML shims; cuBLAS/cuDNN→MPS redirect.
- **Unverifiable on this host (needs a real GPU/Mac/NVIDIA box):** all absolute performance numbers;
  `bytesNoCopy`/IOSurface/unified-memory behavior; SPIRV-Cross and Metal-compiler fidelity; whether
  unmodified `libcudart`/PyTorch accept the driver-shim's answers; Vulkan-on-NVIDIA parity.

This host has **no GPU, no display, no CUDA/NVIDIA, and no crates.io access**, so the real Metal/CUDA/
Vulkan executors cannot be built or run here by design — the executable deliverable is the dependency-light
IR/trait/protocol crate + its tests; the rest is design that lands on the appropriate host.
