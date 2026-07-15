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

/// The usage flags every CUDA device allocation needs: storage + both copy directions + host-mappable.
pub(crate) fn cuda_buffer_usage() -> u32 {
    buffer_usage::STORAGE | buffer_usage::COPY_SRC | buffer_usage::COPY_DST | buffer_usage::MAP
}

/// Build the `CreateBuffer` command for a CUDA allocation of `size` bytes backed by `buffer`.
pub(crate) fn create_buffer_cmd(buffer: u32, size: u64) -> Cmd {
    Cmd::CreateBuffer(buffer, BufferDesc { size, usage: cuda_buffer_usage(), label: String::new() })
}

/// Reject an allocation of `size` bytes that would push total live device memory past the modeled
/// device's VRAM budget (`CudaDeviceDesc::total_mem`), returning the `CUDA_ERROR_OUT_OF_MEMORY` /
/// `cudaErrorMemoryAllocation` analogue ([`GpuError::ResourceLimit`]).
///
/// A real driver fails `cuMemAlloc`/`cudaMalloc` with an out-of-memory status once the request exceeds
/// what the device can back; without this check the model would MINT a device pointer for an
/// impossible allocation (a fake success), then hand the guest a buffer the host could never populate.
/// The check runs BEFORE any id is minted or `Cmd` submitted, so an over-budget request touches no state.
fn check_budget(ctx: &CudaContext, size: u64) -> Result<()> {
    let used = ctx.mem.total_bytes();
    let budget = ctx.device.total_mem;
    if used.checked_add(size).map(|total| total > budget).unwrap_or(true) {
        hl_log::hl_warn!(
            hl_log::tag::CUDA,
            "mem_alloc OOM: size={} used={} budget={}",
            size,
            used,
            budget
        );
        return Err(GpuError::ResourceLimit("cuMemAlloc: allocation exceeds device memory budget"));
    }
    Ok(())
}

/// `cuMemAlloc(size)` → a device pointer, submitting the backing [`Cmd::CreateBuffer`].
pub fn mem_alloc(ctx: &mut CudaContext, sink: &mut dyn CommandSink, size: u64) -> Result<DevicePtr> {
    check_budget(ctx, size)?;
    let buffer = ctx.alloc_buffer();
    let ptr = ctx.mem.record(buffer, size);
    sink.submit(&[create_buffer_cmd(buffer, size)])?;
    hl_log::hl_debug!(hl_log::tag::CUDA, "mem_alloc size={} buf={} ptr={:#x}", size, buffer, ptr.0);
    hl_log::hl_count!(hl_log::tag::CUDA, "allocs");
    hl_log::hl_add!(hl_log::tag::CUDA, "alloc_bytes", size);
    Ok(ptr)
}

/// `cuMemAllocManaged(size)` → a device pointer into *managed* (unified, host-addressable) memory. The
/// backing IR is identical to [`mem_alloc`] — a single [`Cmd::CreateBuffer`] — but the allocation is
/// flagged managed in the model so `cuPointerGetAttribute(IS_MANAGED)` answers truthfully.
pub fn mem_alloc_managed(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    size: u64,
) -> Result<DevicePtr> {
    check_budget(ctx, size)?;
    let buffer = ctx.alloc_buffer();
    let ptr = ctx.mem.record_managed(buffer, size);
    sink.submit(&[create_buffer_cmd(buffer, size)])?;
    Ok(ptr)
}

/// `cuMemAllocPitch(widthBytes, height, elementSize)` → a 2D device allocation. The row *pitch* is
/// `widthBytes` rounded up to a 512-byte boundary (matching a real allocator's texture alignment), and the
/// backing buffer is `pitch * height` bytes. Returns the base device pointer and the computed pitch (the
/// value the driver hands back through its `*pPitch` out-param). The IR is a single [`Cmd::CreateBuffer`].
pub fn mem_alloc_pitch(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    width_bytes: u64,
    height: u64,
    _element_size: u32,
) -> Result<(DevicePtr, u64)> {
    if width_bytes == 0 || height == 0 {
        return Err(GpuError::Invalid("cuMemAllocPitch: zero width or height"));
    }
    // 512-byte aligned rows, like a real allocator's CU_DEVICE_ATTRIBUTE_TEXTURE_ALIGNMENT. The
    // round-up `width_bytes + 511` is a CHECKED add: a `width_bytes` near u64::MAX must surface a typed
    // error, never an add-overflow (debug panic) / wrap to a tiny pitch.
    let pitch = width_bytes
        .checked_add(511)
        .map(|w| w & !511u64)
        .ok_or(GpuError::Invalid("cuMemAllocPitch: width alignment overflow"))?;
    let size = pitch
        .checked_mul(height)
        .ok_or(GpuError::Invalid("cuMemAllocPitch: pitch*height overflow"))?;
    check_budget(ctx, size)?;
    let buffer = ctx.alloc_buffer();
    let ptr = ctx.mem.record(buffer, size);
    sink.submit(&[create_buffer_cmd(buffer, size)])?;
    Ok((ptr, pitch))
}

/// `cuMemFree(ptr)` → destroy the backing buffer. Errors (`CUDA_ERROR_INVALID_VALUE` analogue) if `ptr`
/// is not a live allocation base.
pub fn mem_free(ctx: &mut CudaContext, sink: &mut dyn CommandSink, ptr: DevicePtr) -> Result<()> {
    let buffer = ctx.mem.free(ptr).ok_or_else(|| {
        hl_log::hl_warn!(hl_log::tag::CUDA, "mem_free bad ptr={:#x}", ptr.0);
        GpuError::Invalid("cuMemFree: pointer is not a live allocation base")
    })?;
    sink.submit(&[Cmd::DestroyBuffer(buffer)])?;
    hl_log::hl_debug!(hl_log::tag::CUDA, "mem_free buf={} ptr={:#x}", buffer, ptr.0);
    Ok(())
}

// --------------------------------------------------------------------------------------------------
// host (pinned / registered) memory — `cuMemAllocHost` / `cuMemHostAlloc` / `cuMemFreeHost` /
// `cuMemHostRegister` / `cuMemHostUnregister` / `cuMemHostGetDevicePointer`.
// --------------------------------------------------------------------------------------------------

/// `cuMemAllocHost(size)` / `cuMemHostAlloc(size, flags)` → the base address of a fresh page-locked host
/// buffer the model owns. The returned address is real, stable host memory the caller can read/write and
/// pass directly to `cuMemcpy*` as a host source/destination.
///
/// `size` is attacker-controlled, so it is BOUNDED against the modeled memory budget before the backing
/// `vec![0u8; size]` is ever created: an over-budget request returns `None` (the
/// `CUDA_ERROR_OUT_OF_MEMORY` / `cudaErrorMemoryAllocation` analogue a real driver returns) rather than
/// attempting a multi-GiB host allocation that would abort the process on OOM. A real `cuMemAllocHost`
/// likewise fails once the request exceeds what the host can back.
pub fn host_alloc(ctx: &mut CudaContext, size: usize) -> Option<u64> {
    if size as u64 > ctx.device.total_mem {
        hl_log::hl_warn!(
            hl_log::tag::CUDA,
            "host_alloc OOM: size={} budget={}",
            size,
            ctx.device.total_mem
        );
        return None;
    }
    Some(ctx.host.alloc_pinned(size))
}

/// `cuMemFreeHost(p)` → free a pinned allocation. Errors if `base` is not a live pinned allocation.
pub fn host_free(ctx: &mut CudaContext, base: u64) -> Result<()> {
    if ctx.host.free_pinned(base) {
        Ok(())
    } else {
        Err(GpuError::Invalid("cuMemFreeHost: pointer is not a live pinned allocation"))
    }
}

/// `cuMemHostRegister(p, size, flags)` → page-lock an existing guest host range. Errors if it is already
/// a live host allocation (the `CUDA_ERROR_HOST_MEMORY_ALREADY_REGISTERED` analogue).
pub fn host_register(ctx: &mut CudaContext, base: u64, size: u64) -> Result<()> {
    if ctx.host.register(base, size) {
        Ok(())
    } else {
        Err(GpuError::Invalid("cuMemHostRegister: host range is already registered"))
    }
}

/// `cuMemHostUnregister(p)` → unlock a previously registered host range. Errors if `base` was not
/// registered (the `CUDA_ERROR_HOST_MEMORY_NOT_REGISTERED` analogue).
pub fn host_unregister(ctx: &mut CudaContext, base: u64) -> Result<()> {
    if ctx.host.unregister(base) {
        Ok(())
    } else {
        Err(GpuError::Invalid("cuMemHostUnregister: host range is not registered"))
    }
}

/// `cuMemHostGetDevicePointer(p, flags)` → the device pointer that maps the host allocation based at
/// `base`. The first call lazily creates a backing device buffer (sized to the host allocation) with a
/// single [`Cmd::CreateBuffer`] and records the device allocation; repeat calls return the same device
/// pointer. Errors if `base` is not a live host allocation.
pub fn host_get_device_pointer(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    base: u64,
) -> Result<DevicePtr> {
    if let Some((_, ptr)) = ctx.host.device_mapping(base) {
        return Ok(DevicePtr(ptr));
    }
    let size = ctx
        .host
        .size_of(base)
        .ok_or(GpuError::Invalid("cuMemHostGetDevicePointer: not a live host allocation"))?;
    let buffer = ctx.alloc_buffer();
    let ptr = ctx.mem.record(buffer, size);
    ctx.host.set_device_mapping(base, buffer, ptr.0);
    sink.submit(&[create_buffer_cmd(buffer, size)])?;
    Ok(ptr)
}
