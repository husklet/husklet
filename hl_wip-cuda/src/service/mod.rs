//! One CUDA operation per file (OVERVIEW-v2 §2 `service/`).
//!
//! Every function here takes `&mut CudaContext` (the model it mutates) + `&mut dyn CommandSink` (the
//! boundary it submits through), lowers the CUDA operation into protocol [`hl_gpu::Cmd`]s, and submits
//! them. This is the tested lowering surface: a driver test drives these against a
//! [`hl_gpu::RecordingSink`] and asserts the exact recorded command sequence.
//!
//! Fully lowered this pass: [`allocate`] (`cuMemAlloc`/`cuMemFree`), [`transfer`]
//! (`cuMemcpyHtoD`/`DtoH`/`DtoD`), [`load_module`] (`cuModuleLoadData`/`cuModuleGetFunction`),
//! [`launch`] (`cuLaunchKernel`), [`synchronize`] (`cuCtxSynchronize`/`cuStreamSynchronize`).

pub mod allocate;
pub mod launch;
pub mod load_module;
pub mod synchronize;
pub mod transfer;
