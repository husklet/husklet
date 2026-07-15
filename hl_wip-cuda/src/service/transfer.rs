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
use crate::model::stream::Stream;
use hl_gpu::protocol::model::command::Enc;
use hl_gpu::{BufferId, Cmd, CommandBuffer, CommandSink, GpuError, Result};

fn resolve(ctx: &CudaContext, p: DevicePtr, what: &'static str) -> Result<(BufferId, u64)> {
    ctx.resolve(p).ok_or_else(|| {
        hl_log::hl_warn!(hl_log::tag::CUDA, "memcpy dangling ptr={:#x} at={}", p.0, what);
        GpuError::Invalid(what)
    })
}

/// Validate a stream handle for a stream-ordered (`*Async`) op. The lowering is synchronous, so an async
/// op is its sync counterpart guarded by this handle check — a bogus stream is a hard error, never a
/// silent success.
fn check_stream(ctx: &CudaContext, stream: Stream, what: &'static str) -> Result<()> {
    if ctx.streams.is_valid(stream) {
        Ok(())
    } else {
        Err(GpuError::Invalid(what))
    }
}

/// `cuMemcpyHtoD(dst, src, n)` → write `src` into the backing buffer at the resolved offset.
pub fn memcpy_htod(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    dst: DevicePtr,
    src: &[u8],
) -> Result<()> {
    let _s = hl_log::hl_span!(hl_log::tag::CUDA, "memcpy_htod");
    hl_log::hl_add!(hl_log::tag::CUDA, "h2d_bytes", src.len() as u64);
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
    let _s = hl_log::hl_span!(hl_log::tag::CUDA, "memcpy_dtod");
    hl_log::hl_add!(hl_log::tag::CUDA, "d2d_bytes", n);
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
    let _s = hl_log::hl_span!(hl_log::tag::CUDA, "memcpy_dtoh");
    hl_log::hl_add!(hl_log::tag::CUDA, "d2h_bytes", n as u64);
    let (buf, off) = resolve(ctx, src, "cuMemcpyDtoH: dangling source pointer")?;
    sink.read_buffer(buf, off, n)
}

/// `cuMemsetD8/D16/D32(dst, value, N)` → write the already-expanded byte `pattern` into the backing buffer
/// at the resolved offset. The hl-GPU IR has no dedicated fill op, so a memset lowers to the same
/// [`Cmd::WriteBuffer`] as an HtoD copy — the caller expands the element pattern (`value` repeated `N`
/// times) to bytes first. A dangling destination is a hard error.
pub fn memset(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    dst: DevicePtr,
    pattern: &[u8],
) -> Result<()> {
    let (buf, off) = resolve(ctx, dst, "cuMemset: dangling destination pointer")?;
    sink.submit(&[Cmd::WriteBuffer { id: buf.0, offset: off, data: pattern.to_vec() }])?;
    Ok(())
}

// --------------------------------------------------------------------------------------------------
// stream-ordered (`*Async`) copies + memset. The executor is synchronous, so each is its synchronous
// counterpart guarded by a `CUstream` handle validation (`record on the given stream`).
// --------------------------------------------------------------------------------------------------

/// `cuMemcpyHtoDAsync(dst, src, n, stream)` — the stream-ordered HtoD copy. Records the SAME
/// [`Cmd::WriteBuffer`] as [`memcpy_htod`] after validating `stream`.
pub fn memcpy_htod_async(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    stream: Stream,
    dst: DevicePtr,
    src: &[u8],
) -> Result<()> {
    check_stream(ctx, stream, "cuMemcpyHtoDAsync: invalid stream handle")?;
    memcpy_htod(ctx, sink, dst, src)
}

/// `cuMemcpyDtoDAsync(dst, src, n, stream)` — the stream-ordered on-device copy. Records the SAME
/// [`Enc::CopyBufferToBuffer`] as [`memcpy_dtod`] after validating `stream`.
pub fn memcpy_dtod_async(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    stream: Stream,
    dst: DevicePtr,
    src: DevicePtr,
    n: u64,
) -> Result<()> {
    check_stream(ctx, stream, "cuMemcpyDtoDAsync: invalid stream handle")?;
    memcpy_dtod(ctx, sink, dst, src, n)
}

/// `cuMemcpyDtoHAsync(host, src, n, stream)` — the stream-ordered device→host readback. Validates
/// `stream`, then reads `n` bytes back through the sink like [`read_dtoh`].
pub fn read_dtoh_async(
    ctx: &CudaContext,
    sink: &mut dyn CommandSink,
    stream: Stream,
    src: DevicePtr,
    n: usize,
) -> Result<Vec<u8>> {
    check_stream(ctx, stream, "cuMemcpyDtoHAsync: invalid stream handle")?;
    read_dtoh(ctx, sink, src, n)
}

/// `cuMemsetD*Async(dst, value, N, stream)` — the stream-ordered fill. Records the SAME
/// [`Cmd::WriteBuffer`] as [`memset`] after validating `stream`.
pub fn memset_async(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    stream: Stream,
    dst: DevicePtr,
    pattern: &[u8],
) -> Result<()> {
    check_stream(ctx, stream, "cuMemsetAsync: invalid stream handle")?;
    memset(ctx, sink, dst, pattern)
}
