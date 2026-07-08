# libcudart.so — the CUDA Runtime API shim, layered on dd's driver shim

Status: **Tier 0 + Tier 1 SHIPPED** (2026-07, `dd-gpu/cuda/libcudart.so`). Prep for a later ML goal (run real PyTorch on dd). This doc

> ## Implemented status (Tier 0 + Tier 1)
>
> `dd-gpu/cuda/libcudart.so.1` is a real clean-room CUDA Runtime-API shim, cross-built for
> aarch64 + x86_64 alongside `libcuda.so.1` (`build.sh`), DT_NEEDED-linked to the driver and
> dlopen-clean. **37 exported symbols** (`cuda*` + the `__cuda*` registration glue); the driver shim
> stays at its 435-symbol surface (unchanged behavior; only `cuModuleLoadData` gained fatbin→PTX).
> `cargo test -p dd-gpu` stays green — this is C-only, gate-neutral (dd-gpu/cuda/, not the engine).
>
> - **Tier 0 (stateful API):** `cudaGetDeviceCount/SetDevice/GetDevice/DeviceSynchronize`,
>   `cudaGetDeviceProperties(_v2)` (fills a faithful clean-room `cudaDeviceProp` from the driver's
>   `cuDeviceGetAttribute`/name/totalmem/CC), `cudaMalloc/Free`, `cudaMemcpy(kind)`/`Async`,
>   `cudaMemset`/`Async`, `cudaMemGetInfo`, streams, events, `cudaDriver/RuntimeGetVersion`, the
>   `cudaGetErrorString/Name` tables, the cudart-owned **`CUresult→cudaError_t` map**, and a
>   **thread-local last-error** cell (`cudaGetLastError` drains, `cudaPeekAtLastError` doesn't).
> - **Tier 1 (launch + registration glue + fatbin→PTX):** `__cudaRegisterFatBinary/End/Function/Var/`
>   `Unregister` + the thread-local `__cudaPush/PopCallConfiguration` stack + `cudaLaunchKernel`
>   (host-stub pointer → registered fatbin → `cuModuleLoadData` → `cuModuleGetFunction` →
>   `cuLaunchKernel`). Fatbin→PTX extraction lives in one shared header `dd-gpu/cuda/fatbin.h`
>   (walks `__fatBinC_Wrapper_t` `0x466243b1` → container `0xBA55ED50` → picks the first
>   **uncompressed kind-1 (PTX)** entry); the driver's `cuModuleLoadData` now calls it, so the former
>   blanket fatbin rejection is gone and a driver-API app can pass a fatbin directly too.
> - **Verified (`test_cudart.c`, synthetic — we own the blob):** a hand-built uncompressed fatbin
>   wrapping the vecadd PTX registered through the glue and driven end-to-end
>   `cudaMalloc → cudaMemcpy(H2D) → __cudaPushCallConfiguration → host stub → cudaLaunchKernel →
>   cudaMemcpy(D2H)` asserts `c[i]==a[i]+b[i]` for all N; `cudaGetDeviceCount==1` and
>   `cudaGetDeviceProperties` sane; a **SASS-only** fatbin (kind-2) surfaces
>   `cudaErrorInvalidKernelImage` (no crash), an unregistered host fn → `cudaErrorInvalidDeviceFunction`.
> - **Out of scope (firewall-gated, unchanged):** the NVIDIA LZ4-variant **compressed**-fatbin
>   decompressor (Tier 1.5) and real-torch validation (no torch/nvcc image cached, egress blocked) —
>   both are honest NULL→`INVALID_IMAGE` today. nvrtc shim (Tier 2) and cuBLAS/cuDNN→MPS (Tier 3) as below.

This doc
scopes exactly what a `libcudart.so` shim needs, decides the one make-or-break question
(**can we extract embedded PTX out of PyTorch's fatbins and feed the existing PTX executor?**), and
gives a ranked plan. It is the runtime-API companion to
[`CUDA_ON_METAL.md`](CUDA_ON_METAL.md) (the driver-API / where-to-intercept doc) and
[`NVIDIA_DEVICE_EMULATION.md`](NVIDIA_DEVICE_EMULATION.md).

Everything below is layered on what already exists in `dd-gpu/`:

- **`dd-gpu/cuda/` — `libcuda.so.1`** the Driver-API C shim. ~134 real `cu*` entry points
  (`REAL_LIST`), 247 honest `CUDA_ERROR_NOT_SUPPORTED` stubs (`STUB_LIST`), plus `_v2/_v3` and
  `_ptds/_ptsz` ABI aliases and a `cuGetProcAddress` dispatch table — the ~435-symbol surface. It
  ingests **PTX text**, parses `.entry` kernels, and runs a modeled SIMT subset on the embedded CPU
  oracle. **It rejects cubin (ELF `\x7fELF`) and fatbin (`0xBA55ED50`) as `CUDA_ERROR_INVALID_IMAGE`**
  (`image_is_binary()` in `cuda_shim.c` line ~937) — dd executes PTX only.
- **`dd-gpu/src/{cuda.rs, ptx.rs, ir.rs, software.rs}`** — the PTX→dd-GPU-IR front-end + CPU
  interpreter + software backend. `CudaContext` turns driver calls into `Cmd` IR; `SoftwareBackend`
  replays it and actually executes the compiled PTX per-thread over the grid.
- **`dd-gpu/nvml/` — `libnvidia-ml.so`** the NVML shim (`nvidia-smi` / pynvml work).

`libcudart` is the missing upper layer. This doc is the map.

---

## 0. TL;DR verdict

1. **cudart is thin over the existing driver shim** for the *stateful* API (malloc/memcpy/streams/
   events/device mgmt) — a few days of re-expression. The non-trivial part is the **compiler-emitted
   registration glue** (`__cudaRegisterFatBinary/Function/...`) plus the `<<<>>>` call-config stack,
   because that is where the host program hands cudart its kernels — as **fatbins**, not PTX.

2. **The crux — fatbin→PTX extraction — is FEASIBLE and is the correct path.** A nvcc fatbin is a
   documented container (`0xBA55ED50` header → typed entries; **kind 1 = PTX text**, kind 2 = SASS
   ELF). We can walk the entries, pick the PTX one, (decompress if flagged), and hand the PTX text to
   the **existing** `cuModuleLoadData` → PTX front-end — **entirely avoiding SASS**. Two real gates:
   (a) the fatbin must actually *contain* a PTX entry (per-build, per-kernel — not guaranteed for
   PyTorch's prebuilt native kernels; many are shipped SASS-only), and (b) nvcc usually **compresses**
   the payload (NVIDIA's LZ4 variant → a bounded decompressor we must write). SASS-only kernels are
   **out of scope** (would need a SASS interpreter — a second GPU-ISA emulator, huge).

3. **The cuBLAS/cuDNN wall is real and independent.** PyTorch's matmul/conv/attention live in closed
   `libcublas`/`libcudnn` `.so`s that launch their **own SASS** kernels and never touch
   `cuModuleLoadData`. The PTX path gets **none** of it. Running PyTorch's *own* kernels buys you
   **elementwise/pointwise tensor programs**, not model inference. Full inference needs redirecting
   cuBLAS/cuDNN to Apple MPS/MPSGraph/MLX (`CUDA_ON_METAL.md` tier 3) — a separate multi-quarter
   program, orthogonal to cudart.

4. **Environment blocker (confirmed):** `~/.dd/images` is **empty**; no `torch`, `libcudart`,
   `libcublas`, or even `python3` exists in any cached container rootfs under `~/.dd/containers`; the
   host firewall blocks `pip install torch`. **Real-torch fatbins cannot be produced or validated in
   this environment.** Tier-1 can still be validated with in-repo synthetic fatbin blobs (we own the
   format); real-torch validation must defer to a networked env or a pre-provisioned torch image.

---

## 1. The cudart ↔ driver boundary

`libcudart.so` is a **separate library** from `libcuda.so.1`. Apps link cudart (directly or
statically); cudart calls the driver under the hood. PyTorch links **both** — libtorch_cuda uses the
runtime API for most host-side management and the `<<<>>>` launch glue, and dips into the driver API
(`cuModuleLoadData`, `cuLaunchKernel`, nvrtc products) for JIT'd kernels. Our job: implement the
cudart surface PyTorch touches, mapping each call onto the driver shim we already ship.

### 1a. Public runtime entry points → driver mapping

| cudart entry point | maps to driver call(s) | effort |
|---|---|---|
| `cudaMalloc` | `cuMemAlloc_v2` | trivial |
| `cudaFree` | `cuMemFree_v2` | trivial |
| `cudaMallocManaged` | `cuMemAllocManaged` | trivial |
| `cudaMallocHost` / `cudaHostAlloc` | `cuMemAllocHost_v2` / `cuMemHostAlloc` | trivial |
| `cudaFreeHost` | `cuMemFreeHost` | trivial |
| `cudaMemcpy` (kind-dispatched) | `cuMemcpyHtoD_v2` / `cuMemcpyDtoH_v2` / `cuMemcpyDtoD_v2` | trivial (switch on `cudaMemcpyKind`) |
| `cudaMemcpyAsync` | `cuMemcpy*Async_v2` (executor is synchronous → same as sync) | trivial |
| `cudaMemset` / `cudaMemsetAsync` | `cuMemsetD8_v2` / `cuMemsetD32_v2` (+Async) | trivial |
| `cudaMemGetInfo` | `cuMemGetInfo_v2` | trivial |
| `cudaGetDeviceCount` | `cuDeviceGetCount` | trivial |
| `cudaSetDevice` / `cudaGetDevice` | `cuDevicePrimaryCtxRetain` + `cuCtxSetCurrent`; track current-device TLS | small |
| `cudaDeviceSynchronize` | `cuCtxSynchronize` | trivial |
| `cudaDeviceReset` | `cuDevicePrimaryCtxReset_v2` | trivial |
| `cudaGetDeviceProperties(_v2)` | fill `cudaDeviceProp` from `cuDeviceGetAttribute` + `CudaDeviceDesc` | **medium** (big struct) |
| `cudaDeviceGetAttribute` | `cuDeviceGetAttribute` (enum remap runtime↔driver) | small |
| `cudaDriverGetVersion` | `cuDriverGetVersion` | trivial |
| `cudaRuntimeGetVersion` | constant (e.g. `12040`) | trivial |
| `cudaGetLastError` / `cudaPeekAtLastError` | **cudart-side thread-local error slot** (no driver call) | small |
| `cudaGetErrorString` / `cudaGetErrorName` | local table (mirror driver's) | trivial |
| `cudaStreamCreate(WithFlags/Priority)` | `cuStreamCreate(WithPriority)` | trivial |
| `cudaStreamDestroy` | `cuStreamDestroy_v2` | trivial |
| `cudaStreamSynchronize` / `Query` | `cuStreamSynchronize` / `cuStreamQuery` | trivial |
| `cudaStreamWaitEvent` | `cuStreamWaitEvent` | trivial |
| `cudaEventCreate(WithFlags)` | `cuEventCreate` | trivial |
| `cudaEventRecord` / `Query` / `Synchronize` / `Destroy` / `ElapsedTime` | `cuEvent*` | trivial |
| `cudaPointerGetAttributes` | `cuPointerGetAttributes` | small (struct shape differs) |
| `cudaFuncGetAttributes` | `cuFuncGetAttribute` (loop the attrs into `cudaFuncAttributes`) | small |
| `cudaDeviceGetStreamPriorityRange` | `cuCtxGetStreamPriorityRange` | trivial |
| `cudaSetDeviceFlags` / `cudaGetDeviceFlags` | context flag bookkeeping | trivial |
| `cudaLaunchKernel` | resolve registered func → `cuLaunchKernel` (see §1c) | **medium** |

**Error translation:** cudart returns `cudaError_t` (a *different* enum from `CUresult`). The shim
needs a `CUresult → cudaError_t` map and a **thread-local "last error"** cell that
`cudaGetLastError` drains — this is cudart-owned state the driver shim does not have.

Net: the whole stateful surface is a mechanical re-expression onto entry points the driver shim
**already exports**. Nothing here needs *new driver support* except possibly richer
`cudaGetDeviceProperties` fields (all derivable from `cuDeviceGetAttribute`, which is already real).

### 1b. Hidden registration symbols (the compiler-emitted glue) — the real work

`nvcc` compiles a `.cu` file into host code that, at load time (via a `.init_array` constructor),
registers its device code with cudart. These symbols are **not** in any public header; they are the
private cudart ABI the compiler targets. PyTorch's extensions and libtorch_cuda emit exactly these:

| symbol | signature (abridged) | what it does | our impl |
|---|---|---|---|
| `__cudaRegisterFatBinary` | `void** __cudaRegisterFatBinary(void* fatCubin)` | `fatCubin` → `__fatBinC_Wrapper_t{ int magic=0x466243b1; int version; void* data; void* filename; }`; `data` → the fatbin container (`0xBA55ED50`). Returns an opaque **handle**. | **Parse wrapper → extract PTX (§2) → `cuModuleLoadData` → store handle→CUmodule** |
| `__cudaRegisterFatBinaryEnd` | `void __cudaRegisterFatBinaryEnd(void** handle)` | finalize marker | no-op |
| `__cudaRegisterFunction` | `void __cudaRegisterFunction(void** handle, const char* hostFun, char* deviceFun, const char* deviceName, int tl, uint3*, uint3*, dim3*, dim3*, int*)` | maps a **host stub pointer** (`hostFun`) → **device kernel name** (`deviceName`). This is the name→kernel table. | **record `hostFun → (handle, deviceName)`**; lazily `cuModuleGetFunction` on first launch |
| `__cudaRegisterVar` | `void __cudaRegisterVar(void** handle, char* hostVar, char* deviceAddr, const char* deviceName, int ext, size_t size, int constant, int global)` | registers a `__device__/__constant__` global | record; back with `cuModuleGetGlobal` (driver currently returns NOT_FOUND — dd doesn't parse `.global` yet; acceptable for kernels that don't use module globals) |
| `__cudaUnregisterFatBinary` | `void __cudaUnregisterFatBinary(void** handle)` | teardown | `cuModuleUnload` + drop handle |
| `__cudaPushCallConfiguration` | `unsigned __cudaPushCallConfiguration(dim3 grid, dim3 block, size_t sharedMem, void* stream)` | pushes `<<<grid,block,shmem,stream>>>` onto a **thread-local stack** | push onto TLS stack; return 0 |
| `__cudaPopCallConfiguration` | `cudaError_t __cudaPopCallConfiguration(dim3* grid, dim3* block, size_t* shmem, void* stream)` | pops it back inside the generated stub | pop from TLS stack |

### 1c. How a `kernel<<<grid,block>>>(args)` launch actually flows

nvcc lowers `kernel<<<g,b>>>(a,b,c)` at the call site to:

```
__cudaPushCallConfiguration(g, b, shmem, stream);   // stash launch config in TLS
kernel(a, b, c);                                     // call the HOST STUB named `kernel`
```

and emits the host stub body:

```
void kernel(A a, B b, C c) {
    void* args[] = { &a, &b, &c };
    dim3 g, b_; size_t sh; cudaStream_t st;
    __cudaPopCallConfiguration(&g, &b_, &sh, &st);   // recover it
    cudaLaunchKernel((void*)kernel, g, b_, args, sh, st);  // hostFun == &kernel
}
```

So `cudaLaunchKernel`'s first argument is the **host stub pointer**, which our shim looks up in the
`__cudaRegisterFunction` table → `(handle, deviceName)` → `cuModuleGetFunction(module, deviceName)` →
build the `KernelArg[]` from `args[]` → `cuLaunchKernel`. The existing `CudaContext::launch`
(cuda.rs) already turns that into the compute IR the backend replays.

**Conclusion for §1:** the stateful API is trivial; the *registration machinery + call-config stack +
error TLS + `cudaLaunchKernel` argument marshalling* are the ~medium new work, and they are the piece
that requires **§2 (fatbin→PTX)** to produce anything runnable.

---

## 2. THE fatbin/SASS problem — can we extract the embedded PTX? (the crux)

**Verdict: YES, architecturally — extracting embedded PTX from a fatbin and feeding it to the existing
PTX front-end is sound and is the only sane path (it sidesteps SASS entirely). But it is gated by two
real conditions: (a) the fatbin must contain a PTX entry, and (b) we must decompress NVIDIA's payload
format. For hand-written and nvrtc-JIT'd kernels PTX is essentially always available; for PyTorch's
prebuilt native kernels it is per-build and often absent → those are out of scope.**

### 2a. The container format (what `__cudaRegisterFatBinary` hands us)

Two nested layers:

1. **`__fatBinC_Wrapper_t`** (what the symbol receives):
   ```
   struct { int magic;          // 0x466243b1
            int version;         // 1
            const unsigned long long* data;   // -> fatbin container (below)
            void* filename_or_fatbins; };
   ```

2. **The fatbin container** (`data` points here) — a 16-byte header then a run of typed entries:
   ```
   struct fatBinaryHeader {      // 16 bytes
       unsigned int  magic;      // 0xBA55ED50  (bytes 50 ed 55 ba — the value image_is_binary() rejects)
       unsigned short version;   // 1
       unsigned short headerSize;// 16
       unsigned long long fatSize; };   // total bytes of all entries that follow
   ```
   then, repeated until `fatSize` is consumed, an **entry header** + payload:
   ```
   struct fatBinEntry {
       unsigned short kind;      // 1 = PTX (text),  2 = ELF/CUBIN (SASS machine code)
       unsigned short version;
       unsigned int  headerSize;
       unsigned long long paddedPayloadSize;
       unsigned int  unused0;
       unsigned int  compressedSize;      // if FLAG_COMPRESS set
       unsigned int  unused1;
       unsigned long long uncompressedSize;
       unsigned int  flags;      // bit 0x0001 = 64-bit; bit 0x2000 = FATBIN_FLAG_COMPRESS
       ...
       unsigned int  smVersion;  // e.g. 86 (sm_86) or 90; for PTX = the compute_XX virtual arch
       unsigned int  ptxVersion;
       ...   // name offset/size etc.
   };  // payload (PTX text or ELF) follows at +headerSize
   ```

### 2b. The extraction algorithm (all reusing what exists)

```
extract_ptx(fatbin_container):
    verify header magic == 0xBA55ED50
    for each entry in [header .. header+fatSize]:
        if entry.kind == 1 (PTX):
            candidate = pick best sm ≤ our reported CC (8.6), else the newest PTX entry
            payload = bytes at entry+headerSize, length = compressedSize or paddedPayloadSize
            if entry.flags & 0x2000: payload = nvidia_lz4_decompress(payload, uncompressedSize)
            return payload as NUL-terminated PTX text     # -> cuModuleLoadData -> existing front-end
    return NONE   # SASS-only fatbin: out of scope
```

Everything downstream of "return payload" **already works**: `cuModuleLoadData` accepts PTX text,
`PtxModule::parse` finds the `.entry` names, `__cudaRegisterFunction`'s `deviceName` selects the entry,
`ptx::compile` + `SoftwareBackend` execute it. **No SASS is ever touched.** The only genuinely new code
is (1) the ~40-line container walker and (2) the decompressor.

### 2c. The two honest gates

- **Compression.** nvcc defaults to `-compress-all`, so real fatbin PTX payloads are usually
  compressed with NVIDIA's proprietary **LZ4-variant** (a custom framing over an LZ4-style
  literal/match stream; the same one `cuobjdump`/community `fatbin` parsers decode). This is a
  **bounded but real decompressor to implement from scratch** (the algorithm is known; no download
  needed, but it is not zero work). Uncompressed fatbins (`-Xfatbin=-compress-all` off, or nvrtc
  output) skip this entirely — so tier-1 can be validated with uncompressed synthetic blobs first.
- **PTX presence is per-build.** A fatbin only carries PTX if the kernel was built with a **virtual**
  arch target (`-gencode arch=compute_XX,code=compute_XX`, i.e. keeping the `compute_XX` output). Many
  production builds ship **only** `code=sm_XX` (SASS) to shrink binaries and drop the PTX. So PTX
  availability is a property of the specific fatbin, not a guarantee.

### 2d. Does PyTorch carry PTX?

Mixed, and mostly **no** for the heavy path:

- PyTorch official wheels are built with a gencode list that **usually includes one virtual arch**
  (e.g. `compute_XX,compute_XX` for the newest supported arch) for forward-compat JIT — so *some*
  of libtorch_cuda's own kernels embed PTX for the newest compute capability. But a large fraction
  of its native `TensorIterator`/reduction kernels are emitted **SASS-only** per real arch, and the
  set that keeps PTX varies by release. **You cannot rely on PTX for every torch native kernel.**
- **cuBLAS/cuDNN/cuFFT kernels carry NO PTX** — they are hand-tuned SASS shipped inside closed `.so`s
  (see §3), and they don't go through cudart's fatbin registration at all.
- **nvrtc-JIT'd kernels** (torch's fused pointwise / JITerator, many custom extensions) are the happy
  case: nvrtc emits **PTX directly**, and the framework calls `cuModuleLoadData(PTX)` on the driver —
  **bypassing the fatbin entirely.** This is the *most* tractable route to running real framework
  kernels and argues for prioritizing an **nvrtc shim** (§4, tier 2).

### 2e. SASS-only → out of scope

Any kernel that resolves only to `kind==2` (ELF/SASS) is **out of scope**. Executing it would require
a **SASS interpreter** — a full second GPU-ISA emulator (SASS is undocumented, per-arch, and enormous;
this is exactly the wall ZLUDA hit). The shim must, for a SASS-only fatbin, return a clean error at
`cuModuleGetFunction`/launch (surfaced as `cudaErrorInvalidDeviceFunction` / the existing
`CUDA_ERROR_INVALID_IMAGE`) — never a crash, never a fake success.

---

## 3. The cuBLAS / cuDNN wall (honest assessment)

PyTorch's compute time is dominated by **matmul (`torch.mm`/`nn.Linear`), convolution, and attention**,
which dispatch to **`libcublas` / `libcublasLt` / `libcudnn`** — closed, hand-tuned NVIDIA libraries.
These are **separate `.so`s** with their own public API (`cublasSgemm`, `cudnnConvolutionForward`, …)
that internally launch their **own SASS** kernels. **They never call `cuModuleLoadData`, never present
PTX, and are invisible to our fatbin/PTX path.** So:

- **What running PyTorch's own kernels via extracted PTX buys you:** the **elementwise / pointwise**
  ops PyTorch implements as its own kernels (TensorIterator-generated: `add`, `mul`, `relu`, `sigmoid`,
  casts, fills, copies), plus simple per-thread reductions/indexing — **iff those specific kernels
  ship PTX** (§2d). Enough for: `torch.tensor(...).cuda()`, `.cpu()`, and **small elementwise tensor
  programs**, numerically verified on the software backend. dd's PTX subset (elementwise arithmetic,
  `fma`, `cvt`, predicated branches, `ld/st.global`; **no** shared memory, atomics, warp intrinsics,
  f64, or `printf`) matches exactly this class.
- **What it does NOT buy you:** any **matmul / conv / attention** → therefore **no `nn.Linear`, no
  transformer, no CNN — i.e. no real model inference.** That is entirely behind cuBLAS/cuDNN.

**To get real inference** you must **redirect** the cuBLAS/cuDNN API surface to **Apple
MPS/MPSGraph/MLX** (reimplement `cublasSgemm` etc. on the host GPU) — `CUDA_ON_METAL.md` **tier 3**, a
large, bounded, multi-quarter program that is **orthogonal to cudart** (it's a different set of shim
`.so`s and a host-side math backend, not a PTX problem). cudart + fatbin-PTX gets you a *correct tiny
tensor engine*, not a *fast model runtime*.

---

## 4. Ranked implementation plan

Each tier is independently shippable and testable **headless on the software backend**.

**Tier 0 — cudart thin over driver; `is_available()` + tensor round-trip.** Implement the stateful
surface (§1a): malloc/free/memcpy(kind)/memset/device count/set/get/props/synchronize/getLastError +
runtime version + streams + events + the `CUresult→cudaError_t` map + last-error TLS. **Target:**
`torch.cuda.is_available() == True`, `torch.cuda.get_device_properties(0)` sane, and
`torch.tensor([...]).cuda().cpu()` round-trips (alloc + H2D + D2H — **no kernel needed**, already works
through the driver). Smallest real win; almost pure re-expression.

**Tier 1 — registration glue + one elementwise op via extracted (uncompressed) PTX.** Implement
`__cudaRegisterFatBinary/End/Function/Var/Unregister` + `__cudaPush/PopCallConfiguration` +
`cudaLaunchKernel` marshalling (§1b/§1c), and the **fatbin container walker** for **kind==1,
uncompressed** (§2b). **Target (the smallest end-to-end ML-shaped goal):** a single elementwise
`c = a + b` on a small CUDA tensor, kernel delivered as a synthetic uncompressed fatbin, verified
`c[i]==a[i]+b[i]` on `SoftwareBackend`. Validate with **in-repo synthetic fatbin blobs** (we own the
format) since real torch is unavailable here (§0).

**Tier 1.5 — the NVIDIA fatbin decompressor.** Implement the LZ4-variant decode (§2c) so real
`-compress-all` fatbins parse. Unlocks actually-shipped fatbins (given they carry PTX at all).

**Tier 2 — `libnvrtc.so` shim (the highest-leverage move for real frameworks).** nvrtc-JIT'd kernels
emit **PTX directly** and call `cuModuleLoadData`, bypassing fatbins (§2d). A shim that runs the
supported PTX subset makes torch's **fused pointwise / JITerator** kernels run **without any fatbin or
SASS at all** — often more of real torch than fatbin extraction does. (For CUDA-C++→PTX we'd need a
compiler; near-term, support the case where the framework hands us PTX or a subset we can pattern-map.)

**Tier 3 — cuBLAS/cuDNN → MPS/MLX redirect (separate program).** The only route to matmul/conv →
**real model inference** (§3). Orthogonal to cudart; multi-quarter; host-GPU-side.

**Broadening the PTX subset** (parallel, incremental): more int/float `cvt` types, `rsqrt`/`ex2`/`lg2`
approximations, `min/max`, more `setp` predicates, `selp`, `mad.wide` — each unlocks more pointwise
kernels. Shared memory / atomics / warp shuffles are a bigger lift and gate tiled reductions.

### Environment blocker (flag to user)

- `~/.dd/images` is **empty**; **no `torch`, `libcudart`, `libcublas`, or `python3`** in any cached
  container rootfs under `~/.dd/containers`; host firewall blocks `pip install torch` / toolkit
  downloads. ⇒ **Real-torch fatbins cannot be produced or validated in this environment**, and nvcc is
  not present to generate test fatbins. Tiers 0–1 are fully validatable **here** with hand-written PTX
  + **synthetic in-repo fatbin blobs**; real-torch end-to-end validation must run in a **networked env
  or against a pre-provisioned torch image** (recommend caching a CUDA-enabled torch image into
  `~/.dd/images` out-of-band as the first real-world test fixture).

---

## 5. Metal-backend convergence

cudart adds **no new backend surface** — it is purely an upper edge that funnels into the existing
driver → `CudaContext` → dd-GPU `Cmd` IR → `GpuBackend` stack. The **same IR** is produced regardless
of backend:

- **Software backend (today):** `SoftwareBackend` compiles the forwarded PTX (`KernelDescriptor` →
  `ptx::compile` → dd-GPU kernel IR) and executes it per-thread on the CPU — the standing correctness
  oracle. Everything in tiers 0–2 is validated here with **no GPU**.
- **Metal backend (the eventual real path, same one `dd-display` uses):** a `GpuBackend` swap behind
  the identical trait. It takes the forwarded PTX/kernel descriptor and goes **PTX → SPIR-V → MSL →
  AIR** (SPIRV-Cross/naga for the middle hop, Apple's Metal compiler for the last), executing the same
  `Dispatch` with `MTLBuffer`s. On Apple-silicon **unified memory**, `cudaMemcpy` H2D/D2H collapse
  toward **zero-copy** (`CUDA_ON_METAL.md` §5.2). The cudart shim, the fatbin extractor, and the
  registration tables are **backend-agnostic** — they emit PTX + IR and never know which backend runs.
- **The one exception is the cuBLAS/cuDNN redirect (tier 3):** it **bypasses** the PTX/IR path and
  calls **MPSGraph/MLX** directly on the host side. That is the only place ML math leaves the
  PTX→IR→backend pipeline — reinforcing that it is a distinct program from the cudart/PTX work.

---

## Appendix — key source references

- `dd-gpu/cuda/cuda_shim.c` — driver shim: `image_is_binary()` (~L937, the fatbin/cubin rejection to
  *replace* with §2 extraction), `cuModuleLoadData` (~L943), `cuModuleGetFunction` (~L964),
  `launch_impl`/`cuLaunchKernel` (~L1108), `REAL_LIST`/`STUB_LIST`/`ALIAS_LIST` + `cuGetProcAddress`
  (~L1396–1482).
- `dd-gpu/src/cuda.rs` — `CudaContext` (driver→IR), `PtxModule::parse`, `launch()` (kernel-arg ABI).
- `dd-gpu/src/ptx.rs` — modeled PTX subset (`compile`, `execute`); the coverage ceiling in §3.
- `dd-gpu/src/software.rs` — `SoftwareBackend` (`run_kernel`/`run_dispatch`), the correctness oracle.
- `docs/ideas/CUDA_ON_METAL.md` — driver-API interception surface, §5.2 GpuBackend seam, tier-3
  cuBLAS/cuDNN→MPS plan. This doc is its runtime-API companion.
