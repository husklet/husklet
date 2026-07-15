//! `cuModuleLoadData` / `cuModuleGetFunction` — module registration + function resolution.
//!
//! These are guest-side bookkeeping ops: they emit NO IR (a module's shader is created lazily at the
//! first [`crate::service::launch`], keyed by entry+block). `cuModuleLoadData` accepts either a raw PTX
//! text image or an nvcc fatbin container — the fatbin case is walked by [`crate::adapter::fatbin`] to
//! recover the embedded PTX. Ported from `hl-gpu/src/cuda.rs` (`module_load`/`module_get_function`) +
//! the fatbin extract path from `hl-shim-cudart`.

use crate::adapter::fatbin;
use crate::model::context::CudaContext;
use crate::model::device::DevicePtr;
use crate::model::module::Function;
use crate::service::allocate::create_buffer_cmd;
use hl_gpu::{CommandSink, GpuError, Result};

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
    let id = ctx.modules.load(ptx);
    hl_log::hl_debug!(
        hl_log::tag::CUDA,
        "module_load id={} bytes={} fatbin={}",
        id,
        image.len(),
        fatbin::is_fatbin(image)
    );
    hl_log::hl_count!(hl_log::tag::CUDA, "modules");
    Ok(id)
}

/// `cuModuleGetFunction(module, name)` → the function handle, or `CUDA_ERROR_NOT_FOUND` analogue.
pub fn module_get_function(ctx: &CudaContext, module: u32, name: &str) -> Result<Function> {
    ctx.modules.get_function(module, name).ok_or_else(|| {
        hl_log::hl_warn!(hl_log::tag::CUDA, "get_function miss mod={} name={}", module, name);
        GpuError::Invalid("cuModuleGetFunction: no such entry in module")
    })
}

/// `cuModuleGetGlobal(module, name)` → the device pointer + byte size of the module's `.global`/`.const`
/// variable `name`. The first lookup of a global lazily creates its backing device buffer (a single
/// [`Cmd::CreateBuffer`]) sized to the global and records the device allocation; repeat lookups return
/// the SAME device pointer and emit nothing. `Ok(None)` means the module declares no such symbol (the
/// `CUDA_ERROR_NOT_FOUND` analogue) — distinct from an `Err` lowering failure so neither fakes success.
pub fn module_get_global(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    module: u32,
    name: &str,
) -> Result<Option<(DevicePtr, u64)>> {
    if let Some((ptr, size)) = ctx.global_alloc(module, name) {
        return Ok(Some((DevicePtr(ptr), size)));
    }
    let Some(size) = ctx.modules.get_global(module, name) else {
        return Ok(None); // module unknown or no such global → NOT_FOUND at the ABI seam
    };
    // A zero-length declaration (e.g. an `extern` array of unknown extent) still gets a 1-byte backing
    // buffer so the returned pointer is a live, resolvable device allocation; the reported size is honest.
    let buffer = ctx.alloc_buffer();
    let ptr = ctx.mem.record(buffer, size.max(1));
    ctx.record_global_alloc(module, name, ptr.0, size);
    sink.submit(&[create_buffer_cmd(buffer, size.max(1))])?;
    hl_log::hl_debug!(
        hl_log::tag::CUDA,
        "module_global mod={} name={} size={} ptr={:#x}",
        module,
        name,
        size,
        ptr.0
    );
    Ok(Some((ptr, size)))
}
