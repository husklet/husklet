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
//! ## Scope of this staging pass
//! The compute path is FULLY lowered: `cuMemAlloc`/`cuMemFree`, `cuMemcpyHtoD`/`DtoH`/`DtoD`,
//! `cuModuleLoadData` (+ fatbin/PTX extract), `cuModuleGetFunction`, `cuLaunchKernel`. Deferred to later
//! passes (called out in the module docs): the injectable shim cdylibs (`shim/`), the `build.rs`
//! dual-arch cross-compile, and the `hl_jit::Driver` plug (`Cuda::new`/`inject`). Those are wiring, not
//! lowering, and are intentionally NOT built here to keep this crate a light, standalone workspace.

pub mod adapter;
pub mod model;
pub mod result;
pub mod service;

// Ergonomic re-exports: downstream (and the shims, later) read `hl_cuda::{CudaContext, DevicePtr, …}`.
pub use model::context::CudaContext;
pub use model::device::{CudaDeviceDesc, DevicePtr};
pub use model::module::{Function, KernelArg, PtxModule};
