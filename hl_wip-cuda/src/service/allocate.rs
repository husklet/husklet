//! `cuMemAlloc` / `cuMemFree` — device-memory allocation → buffer lifecycle IR.
//!
//! Ported from `hl-gpu/src/cuda.rs` (`mem_alloc`/`mem_free`): an allocation mints a buffer id, records
//! the device-pointer→buffer mapping, and lowers to a single [`Cmd::CreateBuffer`] with STORAGE + the
//! copy/map usage flags a CUDA allocation needs; a free lowers to [`Cmd::DestroyBuffer`].

use crate::model::context::CudaContext;
use crate::model::device::DevicePtr;
use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::{Cmd, CommandSink, GpuError, Result};

/// `cuMemAlloc(size)` → a device pointer, submitting the backing [`Cmd::CreateBuffer`].
pub fn mem_alloc(ctx: &mut CudaContext, sink: &mut dyn CommandSink, size: u64) -> Result<DevicePtr> {
    let buffer = ctx.alloc_buffer();
    let ptr = ctx.mem.record(buffer, size);
    let cmd = Cmd::CreateBuffer(
        buffer,
        BufferDesc {
            size,
            usage: buffer_usage::STORAGE
                | buffer_usage::COPY_SRC
                | buffer_usage::COPY_DST
                | buffer_usage::MAP,
            label: String::new(),
        },
    );
    sink.submit(&[cmd])?;
    Ok(ptr)
}

/// `cuMemFree(ptr)` → destroy the backing buffer. Errors (`CUDA_ERROR_INVALID_VALUE` analogue) if `ptr`
/// is not a live allocation base.
pub fn mem_free(ctx: &mut CudaContext, sink: &mut dyn CommandSink, ptr: DevicePtr) -> Result<()> {
    let buffer = ctx
        .mem
        .free(ptr)
        .ok_or(GpuError::Invalid("cuMemFree: pointer is not a live allocation base"))?;
    sink.submit(&[Cmd::DestroyBuffer(buffer)])?;
    Ok(())
}
