Linux-only guest is the case that fits the clean architecture perfectly. Since there's exactly one guest, you don't need the guest-front abstraction at all; you need one Linux front + a universal host-service interface + one backend per host. Here's how I'd structure it.

The shape

[Linux guest binary]
   -> [JIT]                        (arch axis: x86_64/arm64 -> host CPU)   ── one crate, host-agnostic
   -> [Linux syscall ABI front]    (nr, args, errno, /proc, fd/proc model) ── ONE crate, host-agnostic
        -> [HOST-SERVICE INTERFACE] (the universal trait)                  ── the seam
             -> host-linux  impl
             -> host-macos  impl
             -> host-windows impl

Because there's only one guest, the "universal layer" collapses to a single, well-defined job: "what services must a host provide so the Linux front can run any Linux program?" That interface is your whole portability contract.

Concrete crate layout

- translator — frontends (x86/arm → IR), IR, backends (IR → host CPU). No OS calls. Selected by host CPU.
- linux-abi — the syscall dispatcher (switch(nr)), argument decoding, errno encoding, /proc+/sys synthesis, the fd/open-file-description + process/pid object model. Host-agnostic. It never names a real OS; it only calls the host-service trait.
- host-services — the trait (the universal interface). This is the artifact you design most carefully.
- host-linux / host-macos / host-windows — one implementation of the trait each.
- engine — wires translator + linux-abi + the compile-time-selected host-* back together.


- write proper agents.md on code-style
- drop mac containers
- host should have support for rendering so each engine can render via wayland


What each backend must implement (fixed contract, from the IR)

Every backend, regardless of GPU, must resolve these IR concepts:
- Device/context lifecycle
- Memory (device alloc/free, host↔device copy, mapped/unified)
- Kernel launch (grid/block → the host's dispatch model)
- Streams/events (async ordering + fences)
- Kernel code: translate the IR's PTX into the host's kernel language

The only thing that changes per backend is the two hard parts: execution/memory-model mapping and PTX → host-kernel-language:

┌────────────────────┬────────────────────────────┬───────────────────┬───────────┐
│      Host GPU      │        API mapping         │   PTX lowers to   │   Cost    │
├────────────────────┼────────────────────────────┼───────────────────┼───────────┤
│ NVIDIA (Win/Linux) │ forward to real CUDA       │ PTX (passthrough) │ cheap     │
├────────────────────┼────────────────────────────┼───────────────────┼───────────┤
│ AMD (Win/Linux)    │ HIP/ROCm or Vulkan compute │ SPIR-V / GCN      │ moderate  │
├────────────────────┼────────────────────────────┼───────────────────┼───────────┤
│ Metal (mac)        │ Metal compute              │ MSL/AIR           │ expensive │
└────────────────────┴────────────────────────────┴───────────────────┴───────────┘

Can wgpu be used?

Partly — and it's an attractive shortcut, but it does NOT cover the whole job. Be clear about what wgpu is: a portable graphics+compute API (WebGPU/WGSL) that lowers to Vulkan / Metal / D3D12 / GL under the hood.


guest process
  guest CUDA app
    -> calls YOUR libcuda shim (mounted in rootfs)      [normal function call]
    -> shim serializes {API call + args + PTX} into compute-IR
    -> writes to a COMMAND RING / sends over a SOCKET or shared-mem region
        ─────────── crosses the guest↔host boundary (a syscall: write/ioctl/futex) ───────────
  host side (real GPU owner)
    -> GPU service reads the IR from the ring/socket
    -> backend lowers IR: PTX->MSL/SPIR-V, launch, copy, fence
    -> runs on real Metal / CUDA / wgpu
    -> writes results + completion back into the ring / shared buffer
    -> signals the guest (eventfd/futex wake) -> shim returns to the app



Complete the native Linux-GUI stack on dd so that real applications run natively on macOS with zero user installation and no workarounds. The GPU layer must be built as three first-class guest driver libraries — CUDA (libcuda), OpenGL ES (libEGL/libGLESv2), and Vulkan (libvulkan) — each a proper product package rather than a test fixture, all sharing one common core (dd-shim) that owns the single IR wire format, the command-ring/socket transport, and the futex/eventfd completion signaling, with the IR defined as one contract that both the guest shims and the host (dd-gpu) agree on so they cannot drift; each of the three libraries must be correct and surface-complete (full, spec-faithful API coverage, not a minimal subset), we have to use opensource libraries in order to understand all implementations and all three serialize into that same IR and are executed on the host by a single backend that lowers per API (GLSL, SPIR-V, and PTX to MSL) onto the real GPU through wgpu and MoltenVK (but everything in rust), sharing memory zero-copy via the IOSurface bridge. On top of that GPU foundation, the Smithay-native compositor must be surface-complete and become the default, presenting every guest window crystal-sharp (true HiDPI, correct input coordinates, correctly positioned popups and menus) and fully responsive, with the host injecting all needed libraries and mounting the sockets automatically so the guest environment just works. The three target application classes must all run as usual by simply launching them with no arguments and no traces or hacks left in the codebase: Chrome (the benchmark, driving GLES through ANGLE) must render and behave normally, GTK/Qt toolkit apps must render without the PIXMAN or libX11 workarounds, and Vulkan apps such as Zed must run — first via software Vulkan and then GPU-accelerated through the Vulkan library forwarding to the host. Every underlying engine bug that blocks this (the multi-process epoll heap corruption, the aarch64 NEON code-generation fault, the shim library shadowing) must be fixed at the root, the full test harness must stay green, the work should be delegated across many agents and merged continuously, and this continues until it is declared good. You are manager keep as many agents busy as possible at all times, merge their work.
