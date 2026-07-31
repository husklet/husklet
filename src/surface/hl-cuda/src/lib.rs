//! hl-cuda — the self-contained CUDA guest driver crate.
//!
//! It does exactly ONE thing (goal.md, OVERVIEW-v2 §4/§5): **lower** an intercepted CUDA operation into
//! the neutral hl-GPU IR and submit it through a [`hl_gpu::CommandSink`]. The host GPU computes; this
//! crate never touches Metal/CUDA-runtime types. It carries all three guest libs' logic — `libcuda`
//! (driver API), `libcudart` (runtime API), and `libnvidia-ml` (NVML) — but every path funnels through
//! the same lowering seam here.
//!
//! ## Layering (v2 doctrine — mirrored across the cuda/vulkan/gl drivers)
//! * [`model`] — the CUDA object model + its invariants: the device descriptor, the per-context handle
//!   tables (allocations, modules, streams), the pipeline cache, and the id counters. Owned values; no
//!   `Cmd` construction, no transport.
//! * [`service`] — one CUDA operation per file (`allocate`, `transfer`, `load_module`, `launch`,
//!   `synchronize`). Each takes `&mut CudaContext` + `&mut dyn CommandSink`, mutates the model, and
//!   submits the protocol `Cmd`s that operation lowers to. This is the tested lowering surface.
//! * [`adapter`] — external, tech-named mechanisms: [`adapter::ptx`] (PTX text → neutral kernel-IR
//!   [`hl_gpu::protocol::model::kernel::KernelProgram`]) and [`adapter::fatbin`] (nvcc fatbin container
//!   walk → embedded PTX).
//! * [`result`] — the CUDA driver/runtime result-code contract + the `GpuError` → `CUresult` map.
//!
//! ## Scope
//! The compute path is FULLY lowered: `cuMemAlloc`/`cuMemFree`, `cuMemcpyHtoD`/`DtoH`/`DtoD`,
//! `cuModuleLoadData` (+ fatbin/PTX extract), `cuModuleGetFunction`, `cuLaunchKernel`. Around that
//! lowering core, the packaging + injection is now wired: the three guest shim cdylibs (`shim/cuda`,
//! `shim/cudart`, `shim/nvml`) marshal the C ABI and call these services through a process-global
//! [`hl_gpu::RemoteCommandSink`]; `build.rs` cross-compiles + stages them for both guest arches.

pub mod adapter;
pub mod logging;
pub mod model;
pub mod result;
pub mod service;

// Ergonomic re-exports: downstream (and the shims) read `hl_cuda::{CudaContext, DevicePtr, …}`.
pub use model::context::CudaContext;
pub use model::device::{CudaDeviceDesc, DevicePtr};
pub use model::module::{Function, KernelArg, PtxModule};
