# Emulating the NVIDIA device at the `/dev/nvidia*` ioctl seam (backed by Metal)

Status: **research + feasibility analysis. No production code.** Companion to
[`CUDA_ON_METAL.md`](CUDA_ON_METAL.md) (the userspace `libcuda`/NVML seam dd is already building in
`dd-gpu` / `dd-nvml`). This doc examines the **lower, riskier seam** the user asked about: implementing
the NVIDIA *kernel-driver* UAPI so the **entirely unmodified** NVIDIA userspace stack (real
`nvidia-smi`, `libcuda`, `libcudart`, cuBLAS/cuDNN, PyTorch) talks to a dd-emulated virtual GPU.

Everything GPU-real is **unverifiable on this host** (Apple-silicon Mac, no NVIDIA GPU, no crates.io, no
Metal). Claims that would need a real NVIDIA box to confirm are flagged **[NEEDS-HW]**. The analysis is
grounded in the open kernel source, gVisor's `nvproxy`, Nouveau/NVK, and dd's own syscall layer, all of
which *are* inspectable.

---

## TL;DR verdict

1. **How hard is device-level emulation?** Split it cleanly:
   - **Control plane (make `nvidia-smi` / `cuInit` believe a GPU exists):** *knowable and moderate.* The
     UAPI is now largely **documented by open source** — NVIDIA's `open-gpu-kernel-modules` (2022,
     MIT/GPL) ships the SDK headers, and gVisor's `pkg/abi/nvgpu` has already re-typed **~150+ ioctls /
     control commands and 250+ structs** in a clean, readable form. You could stand up enough
     `/dev/nvidiactl` + `/dev/nvidia0` responses to pass presence probes. Effort: **weeks-to-months**,
     mostly mechanical.
   - **Execution plane (actually run a kernel):** *this is the killer, and it is worse than the userspace
     seam.* At the device boundary a kernel launch is **already-compiled SASS in an opaque pushbuffer**,
     not PTX. SASS is NVIDIA's undocumented, per-architecture GPU machine ISA. You would be reverse-
     engineering both the command-stream (pushbuffer/GPFIFO method) encoding *and* the SASS ISA, then
     retargeting them to Metal. This is **research-grade bordering on infeasible** for a Metal backend.
   - **Full framework (PyTorch fast):** dominated by closed cuBLAS/cuDNN kernels shipped as SASS —
     compounds the execution-plane problem. Not a device-emulation deliverable at all.

2. **Can we trace it and understand it?** **Yes, the protocol is traceable and substantially already
   traced** — `strace -f -e trace=ioctl`, gVisor's ready-made **`ioctl_sniffer` / `libioctl_hook.so`**
   (LD_PRELOAD, decodes `NV_ESC_*` + control cmd + alloc class), and Nouveau's `mmiotrace` / envytools /
   `nv_push_dump`. But **every one of these needs a real NVIDIA GPU**, which dd's dev host does not have.
   So we can *understand* the control protocol from published source + others' traces, but we **cannot
   generate or validate traces here** — and, decisively, tracing the control plane does *not* make the
   **execution** plane (SASS) tractable, because the pushbuffer contents flow through even the tracers
   opaquely, straight to silicon.

3. **What should dd do?** **Keep the userspace `libcuda`/NVML seam** (`CUDA_ON_METAL.md`, in progress).
   It catches kernels as **PTX** (a documented virtual ISA) *before* SASS compilation — the single most
   important reason not to drop to the device. Device emulation buys **universality** (unmodified stack)
   at the cost of a **strictly worse execution target** (SASS). The only device-emulation that pays off
   is a **thin presence shim** — and even that is *unnecessary for dd*, because dd controls the guest's
   dynamic linker and can inject `libcuda.so` directly, so it never has to satisfy the real driver's
   ioctls at all. **Recommendation: do not emulate the device for execution; at most synthesize device
   *nodes* for presence, and intercept at PTX.** Ranking in §8.

The pre-existing `CUDA_ON_METAL.md` already states "Do NOT emulate the NVIDIA kernel driver." This doc is
the rigorous backing for that one line, plus the SASS-vs-PTX crux and the tracing answer.

---

## 1. The NVIDIA UAPI surface — what actually crosses `/dev/nvidia*`

The NVIDIA Linux stack presents four character devices. A CUDA process opens some subset of them and
drives everything through `ioctl` + `mmap`:

| Node | Role | What crosses it |
|---|---|---|
| **`/dev/nvidiactl`** | Resource Manager (RM) **control** device, GPU-independent | Client/root object creation, version check, GPU enumeration/attach, `NV_ESC_RM_ALLOC` of the object tree, `NV_ESC_RM_CONTROL` system commands |
| **`/dev/nvidia0..N`** | Per-GPU device | Per-GPU `NV_ESC_RM_*` allocations (memory, VA space, channels), `mmap` of BAR/regs/GPFIFO doorbell, per-GPU `NV2080_CTRL_*` queries |
| **`/dev/nvidia-uvm`** | **Unified Virtual Memory** driver (separate `nvidia-uvm.ko`) | Its own ioctl family (`UVM_INITIALIZE`, `UVM_REGISTER_GPU`, `UVM_MAP_EXTERNAL_ALLOCATION`, `UVM_MIGRATE`, …). This is what backs `cudaMallocManaged` and modern CUDA's address-space model |
| **`/dev/nvidia-modeset`** | Display/KMS | Irrelevant to compute; a headless CUDA box may not even open it |

### 1.1 The `NV_ESC_*` frontend ioctls

Base `NV_IOCTL_BASE = 200`. The set is small (~15–20) and stable-ish. The important ones
(numbers from `kernel-open/common/inc/nv-ioctl-numbers.h`, cross-checked against gVisor `nvgpu`):

- `NV_ESC_CHECK_VERSION_STR` (200+10) — **the gate**: userspace and kernel driver versions must match
  exactly (NVIDIA does *not* promise kernel ABI stability).
- `NV_ESC_CARD_INFO`, `NV_ESC_NUMA_INFO`, `NV_ESC_SYS_PARAMS`, `NV_ESC_REGISTER_FD`,
  `NV_ESC_ATTACH_GPUS_TO_FD`, `NV_ESC_ALLOC_OS_EVENT`.
- The five that carry the real weight, all multiplexed through a single ioctl each:
  - **`NV_ESC_RM_ALLOC`** — allocate an object of some **class** (`hClass`) under a parent handle. This is
    how the *entire* RM object tree is built: root client → device → subdevice → VA space → memory →
    channel group → channel → compute/DMA-copy engine objects.
  - **`NV_ESC_RM_CONTROL`** — invoke a **control command** (`cmd`) on an object. This is the giant
    surface: hundreds of `NVxxxx_CTRL_CMD_*` opcodes.
  - **`NV_ESC_RM_ALLOC_MEMORY`**, **`NV_ESC_RM_MAP_MEMORY`**, **`NV_ESC_RM_FREE`**,
    `NV_ESC_RM_DUP_OBJECT`, `NV_ESC_RM_SHARE`.

The parameter blocks are the **`NVOS*` structs** — `NVOS21_PARAMETERS` (alloc), `NVOS54_PARAMETERS`
(control), `NVOS32/33` (vidmem/mapping), `NVOS64` (alloc-with-access), etc. Each has a status field the
kernel fills. gVisor enumerates **30+** of these.

### 1.2 The control-command explosion

`NV_ESC_RM_CONTROL`'s `cmd` is a 32-bit opcode namespaced by object class:

- `NV0000_CTRL_CMD_*` — system/client (e.g. `SYSTEM_GET_BUILD_VERSION = 0x101`,
  `GPU_GET_ATTACHED_IDS = 0x201`).
- `NV2080_CTRL_CMD_*` — per-subdevice (e.g. `GPU_GET_INFO_V2 = 0x20800102`, `FB_GET_INFO`, `GR_GET_INFO`,
  `BUS_GET_PCI_INFO`) — **this is most of what `nvidia-smi` reads.**
- `NVC36F_CTRL_CMD_GPFIFO_GET_WORK_SUBMIT_TOKEN`, `NV503C_CTRL_CMD_REGISTER_VA_SPACE`, … — channel/VA
  plumbing for actually running work.

gVisor's `nvgpu` models **100+** control commands and **250+** structs total. That number is the honest
scale of "the surface," and it is a *subset* filtered to what CUDA actually calls.

### 1.3 The open-source leverage — how much is now knowable?

**This is the single biggest change since the Nouveau era.** In May 2022 NVIDIA open-sourced
`open-gpu-kernel-modules` (MIT/GPL dual license). Critically, it ships **`src/common/sdk/nvidia/inc/`** —
the *same SDK headers* the closed userspace driver is built against: `nvos.h`, the `nv-ioctl*.h`
frontend numbers, and the entire `ctrl/` and `class/` control-command + class-ID trees. So:

- **The kernel↔userspace *boundary* (structs, ioctl numbers, control opcodes, class IDs, GPFIFO method
  layouts) is essentially fully documented** by that source. You do not have to reverse-engineer struct
  layouts anymore; you read them. gVisor's `pkg/abi/nvgpu` is proof-by-existence — a clean re-typing of
  the boundary in Go, derived from these headers.
- **What remains closed / opaque:**
  1. **The behavioral semantics inside RM** — the object model's invariants, ordering, and error
     conditions are only *implied* by the (now open) kernel implementation; emulating them faithfully
     means reading and re-implementing a large slice of `src/nvidia/`.
  2. **GSP-RM firmware.** On Turing+ most of RM runs on the GPU's own microcontroller (GSP) as a signed
     firmware blob. The open kernel module is increasingly a thin shim that ships commands to GSP. The
     firmware itself is **not** source-available — but for a *device emulator* that is fine (you *are*
     the "firmware"); it matters only that behavior is under-documented.
  3. **The pushbuffer command payloads and SASS** — see §3. The open source documents the *methods* that
     start a launch, but the compute kernel body is opaque SASS.

**Bottom line for part 1:** the control-plane *structs and opcodes* are ~90% knowable from open source
today (a night-and-day improvement over Nouveau's clean-room struggle). The control-plane *behavior* is
knowable-but-large. The execution plane is not meaningfully more knowable than before.

---

## 2. Can we trace it and understand it? — yes, but not here

### 2.1 The tools (all mature)

- **`strace -f -e trace=ioctl`** — sees every `ioctl(fd, request, argp)` and the fd's `/dev/nvidia*`
  path. Raw, but with the open headers you can decode `request` → `NV_ESC_*` and dump the `NVOS*` struct.
- **gVisor `ioctl_sniffer` / `libioctl_hook.so`** — the purpose-built tool. **LD_PRELOAD** a shared lib
  that hooks `ioctl(2)`, filters to NVIDIA device files, and **decodes the request, the `NV_ESC_RM_CONTROL`
  control `cmd`, and the `NV_ESC_RM_ALLOC` `hClass`**, printing e.g.
  `Control ioctl: request=0xc020462a [nr=NV_ESC_RM_CONTROL, cmd=0x2080014b]`. It exists to answer exactly
  "what does this workload demand of the driver," which is precisely the map a device emulator needs.
- **Nouveau lineage** — `mmiotrace` (kernel MMIO capture of the blob driver), **envytools** (`rnndb`
  register/method DB, decoders) and Mesa's **`nv_push_dump`** disassemble *pushbuffer* method streams;
  the modern **envyhooks** dumps command sequences from the proprietary driver. NVK (Mesa) and the Rust
  **`nova`** driver are the current RE frontier.

So the answer to "can we trace the calls and understand how it works" is an unambiguous **yes for the
control plane**: the interface is observable, and much of it is already published (gVisor's allowlist +
the open headers *are* a distilled trace).

### 2.2 The hard caveat — **no NVIDIA GPU on dd's dev host**

Every tool above needs a real NVIDIA GPU driving a real driver. dd's development host is an
Apple-silicon Mac with a Metal GPU and no NVIDIA hardware, behind an egress firewall, with no crates.io.
Concretely that means:

- We **cannot capture our own traces** of `nvidia-smi`, PyTorch, etc. **[NEEDS-HW]**
- We **cannot validate** any emulated response against ground truth — dd's terminal/fidelity work has a
  hard "byte-exact vs the oracle" rule (see MEMORY), and here **there is no runnable oracle**.
- We would be reduced to: the open kernel source + gVisor's `nvgpu`/allowlist + others' published traces.
  That is enough to *understand and prototype* the control plane, but **not** enough to reach the
  fidelity dd holds itself to without periodic access to a separate real-NVIDIA box.

And crucially: **tracing does not crack the execution plane.** Even the sniffers pass the pushbuffer
payload through opaquely — the SASS never gets "decoded" by them; it goes to hardware. Tracing tells you
*which* ioctls run a kernel; it does not tell you *what the kernel computes* in a form you can run on
Metal.

---

## 3. The execution-plane killer: SASS vs PTX (the crux)

This is the decisive argument, so state it precisely.

### 3.1 Where compilation happens

```
CUDA C++  --nvcc/nvrtc-->  PTX  --ptxas (embedded in libcuda, HOST-side)-->  SASS  -->  pushbuffer  -->  GPU
          ^ documented           ^ documented virtual ISA    ^ UNDOCUMENTED, per-arch machine ISA
          |                      |                            |
          |                      +-- the libcuda/ZLUDA seam catches HERE (PTX)
          |                                                   +-- the DEVICE seam catches HERE (SASS)
```

The userspace CUDA driver (`libcuda.so`) contains **`ptxas`** and compiles **PTX → SASS on the host CPU**
*before* submission. What crosses `/dev/nvidia*` for a launch is: a channel/GPFIFO set up via
`NV_ESC_RM_*`, GPU memory holding the **finished SASS cubin**, and a **pushbuffer** whose GPFIFO methods
point the compute engine at that SASS with a grid/block launch descriptor. The device driver **never
sees PTX**.

### 3.2 Why this makes the device seam strictly worse for a Metal backend

| | Userspace `libcuda` seam (`CUDA_ON_METAL.md`) | Device `/dev/nvidia*` seam (this doc) |
|---|---|---|
| Kernel arrives as | **PTX** — documented (PTX ISA spec), stable virtual ISA, texty, translatable | **SASS** — undocumented, per-architecture (Volta≠Ampere≠Hopper≠Blackwell), binary machine code |
| Translation path to Metal | PTX → SPIR-V → MSL → AIR (ZLUDA/chipStar-style; hard but *done by others*) | SASS → ? → Metal. No public SASS→anything recompiler exists for running elsewhere |
| Command submission | You *are* `cuLaunchKernel`; you know grid/block/args directly | You must parse the **pushbuffer/GPFIFO method stream** and the launch descriptor to even find the args |
| Prior art that works | ZLUDA, chipStar (run real apps app-by-app) | **None** run unmodified apps by translating SASS |
| ZLUDA's own limit | "if an application embeds SASS instead of PTX, ZLUDA will break" | **the device seam is *always* in that broken case** |

The last row is the whole argument. ZLUDA — the reference implementation of "CUDA on a foreign GPU" —
**breaks exactly when it gets SASS**. The device seam receives SASS **by construction, every time**. So
dropping to the device seam *permanently* puts you in ZLUDA's failure mode, plus adds the pushbuffer/
GPFIFO decode on top. You would need a **SASS disassembler + a SASS→(SPIR-V/MSL) recompiler** for every
NVIDIA architecture generation — something even the academic simulators only do as *interpretation*, not
retargeting (§4). For a Metal execution target this is **research-grade at best**.

### 3.3 The hybrid escape hatch

The only sane way the device seam helps: **emulate just enough device control/presence for the unmodified
stack to believe a GPU exists, but never let a real launch reach the device** — intercept execution one
layer up (at PTX in a `libcuda` shim, or by redirecting cuBLAS/cuDNN to Apple MPS/MLX). But note: **if you
can inject a `libcuda` shim, you have already solved presence there too** and never needed the device
emulation. The hybrid collapses back into "just do the userspace seam," which is what `CUDA_ON_METAL.md`
prescribes. Device-node *synthesis* (a `stat`-able `/dev/nvidia0`) is the only genuinely useful crumb,
and it is ~50 lines (§5).

---

## 4. Prior art — and why each doesn't fit dd

| Approach | What it is | Why it does / doesn't fit dd (no VM, Metal target, no NVIDIA HW) |
|---|---|---|
| **gVisor `nvproxy`** | Userspace-kernel (Sentry) that **intercepts `/dev/nvidia*` ioctls and re-issues them to a *real host NVIDIA driver***, copying structs across the sandbox boundary. | **The definitive datapoint.** Google mapped the *entire* UAPI (`pkg/abi/nvgpu`) and *still chose to proxy to real hardware, not emulate.* Their proposal says emulation "would require reverse-engineering thousands of commands and maintaining that emulation across versions," and that "GPU command submission is opaque … cannot be safely intercepted or validated." dd has **no real driver to forward to** — so dd is in the strictly harder position gVisor deliberately avoided. |
| **NVIDIA vGPU / VFIO mediated devices (mdev)** | Host driver carves one physical GPU into virtual ones for VMs. | Requires **real NVIDIA hardware + hypervisor**. dd has neither a GPU nor a VM. N/A. |
| **GPGPU-Sim / Accel-Sim** | Academic cycle-level simulators that **execute PTX or SASS** functionally + timing. | Prove SASS *can* be interpreted — but they **interpret for research metrics at ~10⁴–10⁶× slowdown**, they do not **retarget** SASS to another GPU, and they model specific arches. Useless as a Metal execution backend; useful only as evidence SASS semantics are partially known for older arches. |
| **rCUDA / gVirtuS / cricket** | **API remoting** — marshal CUDA API calls over a transport to a remote machine that has a real GPU. | This is the **`libcuda`/runtime seam**, not the device. Confirms the right cut-point is the API, not the ioctls — but they still need a real GPU on the far end. dd's twist is the far end is *Metal*, not a GPU. |
| **QEMU GPU emulation** | Emulates simple/virtio GPUs, or does VFIO passthrough. | Emulated models are toy 2D/basic-3D devices; it does **not** emulate a real NVIDIA compute device. Passthrough needs the physical card. N/A. |
| **`libnvidia-container` / nvidia-container-toolkit** | For containers, **bind-mounts the host's `/dev/nvidia0`, `/dev/nvidiactl`, `/dev/nvidia-uvm`** + the driver `.so`s into the container. | **Confirms the device nodes are the seam** — the whole toolkit is "make those three nodes + libs appear, backed by the *real* driver." It never emulates; it forwards to hardware. Same conclusion: the nodes are real presence, the driver behind them is not something anyone reimplements. |

**Synthesis of prior art:** *Everyone who reaches the device level forwards to real silicon
(nvproxy, container-toolkit, vGPU).* *Everyone who wants portability cuts at the API/PTX
(ZLUDA, chipStar, rCUDA).* *The only things that "run" SASS are slow academic interpreters.* Nobody
emulates the NVIDIA device to run an unmodified stack on a foreign GPU, because the execution payload is
SASS. dd should not be the first to try it as a product path.

---

## 5. dd-specific integration — where it *would* slot in

dd's engine already virtualizes the exact syscalls this needs, so the *plumbing* is cheap; the *content*
is the cost.

- **Device-node presence** — dd synthesizes `/proc`, `/sys`, and `/dev` entries in the filesystem
  service. `openat` is handled at `dd-jit-darwin/src/runtime/os/linux/syscall/dispatch.c:552` (case 56)
  and the synthesis logic lives in `.../syscall/fs.c` (see the `/proc`·`/sys`·`/dev` synth around
  fs.c:1510+, and the `synth_str_fd` anonymous-fd trick in `dispatch.c:397`). Adding a `stat`-able
  `/dev/nvidia0`, `/dev/nvidiactl`, `/dev/nvidia-uvm` (char devices, major **195** for `nvidiactl/nvidia0`,
  a dynamic major for `nvidia-uvm`) plus a minimal `/proc/driver/nvidia/version` is **small and
  well-precedented** — this is the same class of work as the existing `/dev/full`, `/dev/pts`,
  `/proc/*` synthesis. **This crumb is worth doing regardless**, purely so presence probes and
  `nvidia-container-toolkit`-style checks don't `ENOENT`.
- **ioctl dispatch** — `ioctl` is owned by the FS service; see the handler in
  `.../syscall/fs.c` (the `case 0x5404/0x5414/…` termios/winsize block near fs.c:424, `net_ioctl`
  offload at fs.c:665, and the note in `.../syscall/io.c:952` that `svc_fs` owns ioctl). A
  `/dev/nvidia*` fd would be tagged at `openat` time (like the `/dev/full` flag at vfs.c:102) and routed
  to a new `nvidia_ioctl(fd, req, argp)` that:
  1. truncates/decodes `req` → `NV_ESC_*` (dd already truncates ioctl requests to 32 bits, fs.c:427);
  2. copies the `NVOS*` param struct in from guest memory (dd already does struct marshalling across the
     guest boundary throughout the syscall layer);
  3. dispatches `NV_ESC_RM_ALLOC` by `hClass`, `NV_ESC_RM_CONTROL` by `cmd`.
- **Backing allocations** — `NV_ESC_RM_ALLOC_MEMORY` + `NV_ESC_RM_MAP_MEMORY` + guest `mmap` would map
  host memory (or an `IOSurface`/`MTLBuffer` on the mac side) into the guest's address space. dd already
  virtualizes `mmap`; on Apple's **unified memory** the "VRAM" is host RAM, so `cudaMemcpy` H2D/D2H
  collapse toward zero-copy — the one genuine architectural gift here (already noted in
  `CUDA_ON_METAL.md`).
- **Submission** — `dd-gpu`'s command IR + `GpuBackend` trait (`dd-gpu/src/backend.rs`, `ir.rs`,
  `ring.rs`) is the forwarding channel to a host **`MetalBackend`**. But this is where it dies: what the
  emulated channel receives is a **pushbuffer of GPFIFO methods pointing at SASS**, and `dd-gpu` has no
  SASS front-end (its `cuda.rs`/`PtxModule` intercepts **PTX**, by design).

### 5.1 Honest size estimate

| Tier | What works | ioctls / control cmds to model | Effort | Confidence |
|---|---|---|---|---|
| **T0 — device nodes only** | `stat`/`open` of `/dev/nvidia*`, `/proc/driver/nvidia/version` present | 0 ioctls (just VFS synth) | **~days** | High |
| **T1 — `nvidia-smi` / presence** | version check, GPU enumerate, `GPU_GET_INFO_V2`, `FB_GET_INFO`, `BUS_GET_PCI_INFO`, ECC/util queries answered with plausible static data | `NV_ESC_CHECK_VERSION_STR`, `CARD_INFO`, a handful of `NV_ESC_RM_ALLOC` classes (client/device/subdevice), **~20–40** `NV2080/NV0000_CTRL_*` | **weeks–2 months** | Med **[NEEDS-HW to validate]** |
| **T2 — `cuInit`+`cudaMalloc`+copy, no launch** | full client object tree, VA space, UVM init, memory alloc/map/free | **+ ~30–50** more control cmds, most of `UVM_*`, `NVOS32/33/64` mapping structs, doorbell/GPFIFO `mmap` | **several months** | Low **[NEEDS-HW]** |
| **T3 — actually launch a kernel** | channel/GPFIFO create + pushbuffer submit + **interpret SASS on Metal** | + channel/copy-engine classes, GPFIFO method decode, **+ a SASS→Metal recompiler per arch** | **research program / infeasible** | Very low |
| **T4 — PyTorch fast** | + closed cuBLAS/cuDNN kernels (also SASS) redirected | everything in T3 × the framework's kernel zoo | **not a device-emulation project** | — |

T0/T1 are real and bounded. T2 is a slog with no runnable oracle here. **T3 is the wall** — and it is the
same wall `CUDA_ON_METAL.md` avoids by intercepting PTX at `libcuda`.

### 5.2 Illustrative skeleton (NOT production; dep-light, never compiled here)

Purely to make the dispatch shape concrete. This is the *only* code in this doc and is illustrative.

```c
/* ILLUSTRATIVE ONLY — not wired into dd, not compiled, not validated (no NVIDIA HW).
 * Shows the shape of a /dev/nvidia* ioctl dispatcher if dd ever did T1 presence.
 * Numbers/structs would come from open-gpu-kernel-modules src/common/sdk/nvidia/inc + gVisor nvgpu. */
#define NV_IOCTL_BASE            200
#define NV_ESC_CHECK_VERSION_STR (NV_IOCTL_BASE + 10)
#define NV_ESC_RM_ALLOC          0x2b     /* + NV_IOCTL_BASE-relative in real headers */
#define NV_ESC_RM_CONTROL        0x2a
#define NV2080_CTRL_CMD_GPU_GET_INFO_V2 0x20800102

static long nvidia_ioctl(dd_guest *g, int fd, uint32_t req, uint64_t argp) {
    switch (req & 0xff) {                       /* NV_ESC_* lives in the low byte of the RM cmds */
    case NV_ESC_CHECK_VERSION_STR:              /* THE GATE: must echo the driver version we claim */
        return emit_version_ok(g, argp, "570.00.00");   /* [NEEDS-HW: real string per driver build] */
    case NV_ESC_RM_ALLOC: {                     /* build our fake RM object tree, keyed by hClass */
        nvos21 p; guest_copy_in(g, argp, &p, sizeof p);
        p.hObjectNew = fake_handle_alloc(g, p.hClass);   /* client/device/subdevice/... */
        p.status = NV_OK; guest_copy_out(g, argp, &p, sizeof p); return 0;
    }
    case NV_ESC_RM_CONTROL: {
        nvos54 p; guest_copy_in(g, argp, &p, sizeof p);
        switch (p.cmd) {
        case NV2080_CTRL_CMD_GPU_GET_INFO_V2:   /* answer nvidia-smi from a static CudaDeviceDesc */
            return answer_gpu_info(g, &p);      /* name/CC/SM count/mem — mirrors dd_gpu::CudaDeviceDesc */
        /* ... ~dozens more for T1 presence ... */
        default: p.status = NV_ERR_NOT_SUPPORTED; guest_copy_out(g,argp,&p,sizeof p); return 0;
        }
    }
    /* A real launch would arrive as NV_ESC_RM_ALLOC(channel) + mmap(GPFIFO doorbell) + a pushbuffer
     * of SASS.  There is NO Metal-runnable path from here — this is the wall (see §3). */
    default: return -EINVAL;
    }
}
```

---

## 6. What device emulation buys vs costs (honest ledger)

**Buys:**
- **Universality** — the *entirely unmodified* NVIDIA userspace runs, including statically-linked
  cudart and anything that bypasses `libcuda`'s public API. The libcuda shim can't cover a binary that
  refuses to use *your* `libcuda`. (In practice, essentially everything dynamically links the stock
  `libcuda.so.1`, so this edge is thin.)
- A clean **security/isolation** story (gVisor's actual motivation) — irrelevant to dd's "run on Metal"
  goal.

**Costs:**
- **SASS execution** — the whole §3 wall. No Metal-runnable path.
- **No runnable oracle on dd's host** — can't validate to dd's fidelity bar without a separate box.
- **Per-driver-version ABI churn** — NVIDIA pins userspace↔kernel versions and does *not* promise kernel
  ABI stability; gVisor maintains **version-specific struct sets** and files issues per new driver
  (e.g. 535.x, 595.x). A dd emulator would inherit that treadmill forever.
- **Behavioral fidelity of RM** — beyond structs, the object-model invariants must match or CUDA aborts
  during init in obscure ways.

The ledger is lopsided: the one real win (universality) is thin because dd controls the linker, and it is
paid for with the worst-possible execution target plus a maintenance treadmill plus no local oracle.

---

## 7. Direct answers to the two questions

**Q1 — How hard is it to emulate the NVIDIA device at the `/dev/nvidia*` ioctl level in dd?**
- *Presence/control plane:* **moderate and bounded** (T0–T1: days-to-2-months), and much easier than the
  Nouveau era because the open kernel headers + gVisor's `nvgpu` hand you the structs/opcodes. dd's VFS
  and ioctl plumbing already exist.
- *Execution plane:* **research-grade to infeasible** for a Metal backend, because you receive **SASS**
  (undocumented per-arch machine code) inside opaque **pushbuffers**, with no public SASS→Metal path.
  This is strictly harder than the PTX seam and is the reason to stop.

**Q2 — Can we trace the calls and understand how it works?**
- **Yes** — via `strace`, gVisor's `ioctl_sniffer`/`libioctl_hook.so` (decodes `NV_ESC_*` + control cmd +
  alloc class), and Nouveau/envytools/`nv_push_dump`. Much of the control plane is *already traced and
  published* (gVisor's allowlist + the open SDK headers). **Caveat, load-bearing:** all tracing needs a
  **real NVIDIA GPU dd's host doesn't have [NEEDS-HW]**, so dd can understand/prototype the control plane
  from published artifacts but cannot capture or validate traces locally — and **tracing does not make
  the SASS execution plane tractable**, since the kernel payload passes through every tracer opaquely.

---

## 8. Recommendation & ranking (effort vs payoff)

**Pursue the userspace `libcuda`/NVML seam (`CUDA_ON_METAL.md`, already in progress). Do not emulate the
device for execution.** Add only the T0 device-node synthesis as a cheap presence crumb.

Ranked, best payoff-per-effort first:

1. **[DO — in progress] Userspace `libcuda`/NVML shim + PTX interception → Metal.** Catches kernels as
   **PTX** (documented), reuses `dd-gpu`'s IR/backend and the `dd-nvml` shim, ships tier-by-tier
   (presence → custom PTX kernels → frameworks-via-MPS/MLX). This is the *only* seam with working prior
   art (ZLUDA/chipStar) and it sidesteps SASS entirely. **Payoff: high. Effort: high but bounded.**
2. **[DO — cheap] T0: synthesize `/dev/nvidia*` nodes + `/proc/driver/nvidia/version`.** ~days, in dd's
   existing VFS synth. Makes presence probes and container-toolkit-style checks stop `ENOENT`-ing.
   Complements #1; needs no ioctl emulation. **Payoff: modest. Effort: tiny.**
3. **[MAYBE, only with a real NVIDIA box] T1: thin `/dev/nvidiactl` ioctl presence** so a *stock*
   `nvidia-smi` (not dd's NVML shim) works unmodified. Only worth it if "unmodified `nvidia-smi`" is an
   explicit requirement AND a validation box is available. Otherwise dd's NVML shim already makes
   `nvidia-smi` work more cheaply. **Payoff: niche. Effort: weeks. [NEEDS-HW]**
4. **[DO NOT] T3 device-level kernel execution (SASS→Metal).** The wall. No Metal-runnable path, no local
   oracle, permanent ABI churn. This is a research program, not a dd feature. **Payoff: universality
   (thin). Effort: research-grade/infeasible.**

**One-line verdict:** *Emulating the NVIDIA device is now well-understood and traceable at the control
plane thanks to `open-gpu-kernel-modules` + gVisor `nvgpu` — but it makes the execution problem strictly
worse (SASS instead of PTX) with no path to Metal and no oracle on dd's host, so dd should keep
intercepting at `libcuda`/PTX and only synthesize device nodes for presence.*

---

## Sources

- NVIDIA, **open-gpu-kernel-modules** (MIT/GPL; `src/common/sdk/nvidia/inc`, `kernel-open/.../nv-ioctl*.h`,
  `escape.c`): https://github.com/NVIDIA/open-gpu-kernel-modules — and the NVIDIA blog announcing it:
  https://developer.nvidia.com/blog/nvidia-releases-open-source-gpu-kernel-modules/
- Source-code analyses of the open module (RM control ioctls, `NV_ESC_RM_ALLOC`, GPFIFO/`NV_ESC_RM_MAP_MEMORY`
  init path): https://eunomia.dev/zh/blog/posts/nvidia-open-driver-analysis/ ,
  https://deepwiki.com/NVIDIA/open-gpu-kernel-modules/3.1-nvidia.ko-core-driver
- gVisor **`pkg/abi/nvgpu`** (re-typed UAPI: ~150+ ioctls/ctrl cmds, `NVOS*`, UVM, class IDs):
  https://pkg.go.dev/gvisor.dev/gvisor/pkg/abi/nvgpu
- gVisor **`nvproxy`** design proposal (proxies to real driver, *does not emulate*; ABI-version churn):
  https://github.com/google/gvisor/blob/master/g3doc/proposals/nvidia_driver_proxy.md — package:
  https://pkg.go.dev/gvisor.dev/gvisor/pkg/sentry/devices/nvproxy
- gVisor **`ioctl_sniffer` / `libioctl_hook.so`** (LD_PRELOAD ioctl tracer, decodes `NV_ESC_*`/cmd/hClass):
  https://pkg.go.dev/gvisor.dev/gvisor/tools/ioctl_sniffer
- **Nouveau** RE + tooling (MmioTrace/REnouveau/Valgrind-MMT; GSP-RM opacity):
  https://en.wikipedia.org/wiki/Nouveau_(software) , https://nouveau.freedesktop.org/NVC0_Firmware.html
- **envytools** (rnndb register/method DB): https://github.com/envytools/envytools ; NVK external HW docs
  (`nv_push_dump`, envyhooks): https://docs.mesa3d.org/drivers/nvk/external_hardware_docs.html
- **ZLUDA** (libcuda shim; PTX→LLVM; *breaks on embedded SASS*):
  https://deepwiki.com/vosen/ZLUDA , https://zluda.org/
- PTX→SASS pipeline / driver-side JIT / pushbuffer submission (background for §3):
  https://docs.nvidia.com/cuda/parallel-thread-execution/index.html ,
  https://docs.nvidia.com/cuda/cuda-compiler-driver-nvcc/index.html
- **libnvidia-container / nvidia-container-toolkit** (bind-mounts `/dev/nvidia0|ctl|-uvm`, confirming the
  nodes are the seam): https://github.com/NVIDIA/libnvidia-container ,
  https://www.abhik.ai/concepts/linux/gpu-containers
- dd integration points (this repo): `dd-jit-darwin/src/runtime/os/linux/syscall/{dispatch.c,fs.c,io.c}`
  (openat case 56 / ioctl handler / `/dev`·`/proc` synth), `dd-jit-darwin/.../container/vfs.c`
  (`/dev/full`, devtmpfs synth), `dd-gpu/src/{cuda.rs,backend.rs,ir.rs}`, `dd-nvml/`,
  and the companion design [`CUDA_ON_METAL.md`](CUDA_ON_METAL.md).

*Unverifiable-here disclosure: no NVIDIA GPU, no Metal, no crates.io, no network egress on this host.
Every runtime/fidelity claim about real NVIDIA behavior is marked **[NEEDS-HW]** and rests on the cited
open source + third-party traces, not on local execution.*
