//! `cuMemcpyHtoD` / `cuMemcpyDtoH` / `cuMemcpyDtoD` — the host↔device / device↔device copy paths.
//!
//! * **HtoD** lowers to a [`Cmd::WriteBuffer`] at the resolved (buffer, offset) — the guest bytes go
//!   straight into the backing buffer (ported from `hl-gpu/src/cuda.rs` `memcpy_htod`).
//! * **DtoD** lowers to a [`Enc::CopyBufferToBuffer`] inside a [`Cmd::Submit`] — a real on-device copy.
//! * **DtoH** has NO protocol command: a device→host read is an out-of-band readback the executor
//!   serves through [`CommandSink::read_buffer`]. [`memcpy_dtoh`] resolves the source to its
//!   (buffer, offset) for callers that only need the location; [`read_dtoh`] performs the full readback,
//!   returning the device bytes over whatever transport the sink is (socket-free or socketed).

use crate::model::context::CudaContext;
use crate::model::device::DevicePtr;
use hl_gpu::protocol::model::command::Enc;
use hl_gpu::{BufferId, Cmd, CommandBuffer, CommandSink, GpuError, Result};

fn resolve(ctx: &CudaContext, p: DevicePtr, what: &'static str) -> Result<(BufferId, u64)> {
    ctx.resolve(p).ok_or(GpuError::Invalid(what))
}

/// `cuMemcpyHtoD(dst, src, n)` → write `src` into the backing buffer at the resolved offset.
pub fn memcpy_htod(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    dst: DevicePtr,
    src: &[u8],
) -> Result<()> {
    let (buf, off) = resolve(ctx, dst, "cuMemcpyHtoD: dangling destination pointer")?;
    sink.submit(&[Cmd::WriteBuffer { id: buf.0, offset: off, data: src.to_vec() }])?;
    Ok(())
}

/// `cuMemcpyDtoD(dst, src, n)` → an on-device buffer-to-buffer copy submitted as one command buffer.
pub fn memcpy_dtod(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    dst: DevicePtr,
    src: DevicePtr,
    n: u64,
) -> Result<()> {
    let (sbuf, soff) = resolve(ctx, src, "cuMemcpyDtoD: dangling source pointer")?;
    let (dbuf, doff) = resolve(ctx, dst, "cuMemcpyDtoD: dangling destination pointer")?;
    sink.submit(&[Cmd::Submit(CommandBuffer {
        encoder: vec![Enc::CopyBufferToBuffer {
            src: sbuf.0,
            src_offset: soff,
            dst: dbuf.0,
            dst_offset: doff,
            size: n,
        }],
        signal: None,
    })])?;
    Ok(())
}

/// `cuMemcpyDtoH(host, src, n)` → resolve the device source to its (buffer, offset) for the caller to
/// read back out-of-band. Submits no command; returns the source location. Prefer [`read_dtoh`] when you
/// want the bytes — it performs the readback through the sink.
pub fn memcpy_dtoh(ctx: &CudaContext, src: DevicePtr) -> Result<(BufferId, u64)> {
    resolve(ctx, src, "cuMemcpyDtoH: dangling source pointer")
}

/// `cuMemcpyDtoH(host, src, n)`, fully served: resolve the device source and read `n` bytes back through
/// the sink's device→host readback path (the real `CommandSink::read_buffer`, in-process or over the wire).
/// Returns exactly `n` device bytes.
pub fn read_dtoh(
    ctx: &CudaContext,
    sink: &mut dyn CommandSink,
    src: DevicePtr,
    n: usize,
) -> Result<Vec<u8>> {
    let (buf, off) = resolve(ctx, src, "cuMemcpyDtoH: dangling source pointer")?;
    sink.read_buffer(buf, off, n)
}
