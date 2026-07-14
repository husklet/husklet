//! `cuModuleLoadData` / `cuModuleGetFunction` — module registration + function resolution.
//!
//! These are guest-side bookkeeping ops: they emit NO IR (a module's shader is created lazily at the
//! first [`crate::service::launch`], keyed by entry+block). `cuModuleLoadData` accepts either a raw PTX
//! text image or an nvcc fatbin container — the fatbin case is walked by [`crate::adapter::fatbin`] to
//! recover the embedded PTX. Ported from `hl-gpu/src/cuda.rs` (`module_load`/`module_get_function`) +
//! the fatbin extract path from `hl-shim-cudart`.

use crate::adapter::fatbin;
use crate::model::context::CudaContext;
use crate::model::module::Function;
use hl_gpu::{GpuError, Result};

/// `cuModuleLoadData(image)` where `image` is raw PTX text (as the CUDA driver API receives it). Returns
/// the module id.
pub fn module_load_ptx(ctx: &mut CudaContext, ptx: &str) -> u32 {
    ctx.modules.load(ptx)
}

/// `cuModuleLoadData(image)` on an arbitrary image: if it is an nvcc fatbin container, walk it and load
/// the embedded uncompressed PTX; otherwise interpret the bytes as raw PTX text. Errors
/// (`CUDA_ERROR_INVALID_PTX`/`INVALID_IMAGE` analogue) if a fatbin carries no usable PTX or the bytes
/// are not valid UTF-8 PTX text.
pub fn module_load_data(ctx: &mut CudaContext, image: &[u8]) -> Result<u32> {
    let ptx_bytes = if fatbin::is_fatbin(image) {
        fatbin::extract_ptx(image)
            .ok_or(GpuError::Invalid("cuModuleLoadData: fatbin carries no uncompressed PTX"))?
    } else {
        image.to_vec()
    };
    let ptx = std::str::from_utf8(&ptx_bytes)
        .map_err(|_| GpuError::Invalid("cuModuleLoadData: image is not valid PTX text"))?;
    Ok(ctx.modules.load(ptx))
}

/// `cuModuleGetFunction(module, name)` → the function handle, or `CUDA_ERROR_NOT_FOUND` analogue.
pub fn module_get_function(ctx: &CudaContext, module: u32, name: &str) -> Result<Function> {
    ctx.modules
        .get_function(module, name)
        .ok_or(GpuError::Invalid("cuModuleGetFunction: no such entry in module"))
}
