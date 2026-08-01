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
        hl_log::hl_error!(
            hl_log::tag::CUDA,
            "memcpy dangling ptr={:#x} at={}",
            p.0,
            what
        );
        GpuError::Invalid(what)
    })
}

/// Resolve `p` to its backing `(buffer, byte offset)` AND bound `len` against the containing allocation:
/// the copy/fill must stay inside `[p, alloc_end)`. A dangling pointer is `Invalid`; a range that runs
/// past the allocation end is `OutOfBounds` (the `CUDA_ERROR_INVALID_VALUE` analogue) — never a silent
/// out-of-bounds `WriteBuffer`/readback and never an `offset + len` add-overflow (all arithmetic is
/// saturating/checked). This is the single guard every copy/memset path funnels through so an
/// attacker-controlled `len`/`offset` can neither corrupt a neighbouring allocation nor drive an
/// unbounded host readback.
fn resolve_range(
    ctx: &CudaContext,
    p: DevicePtr,
    len: u64,
    what: &'static str,
) -> Result<(BufferId, u64)> {
    let (buf, off) = resolve(ctx, p, what)?;
    // `containing` returns the allocation base+size; `off` = p - base, so `size - off` (the bytes from `p`
    // to the allocation end) never underflows — `resolve` already proved `off < size.max(1)`.
    let (_base, size) = ctx
        .mem
        .containing(p)
        .expect("a resolved device pointer always has a containing allocation");
    let avail = size.saturating_sub(off);
    if len > avail {
        hl_log::hl_error!(
            hl_log::tag::CUDA,
            "copy/memset out of bounds ptr={:#x} len={} avail={} at={}",
            p.0,
            len,
            avail,
            what
        );
        return Err(GpuError::OutOfBounds);
    }
    Ok((buf, off))
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
    let (buf, off) = resolve_range(
        ctx,
        dst,
        src.len() as u64,
        "cuMemcpyHtoD: dangling destination pointer",
    )?;
    sink.submit(&[Cmd::WriteBuffer {
        id: buf.0,
        offset: off,
        data: src.to_vec(),
    }])?;
    Ok(())
}

/// The buffer-copy alignment the on-device copy command requires. Single-sourced from the protocol so
/// this lowering and the host validator cannot drift apart.
const COPY_ALIGNMENT: u64 = hl_gpu::Limits::DEFAULT_COPY_ALIGNMENT;

/// `cuMemcpyDtoD(dst, src, n)` → an on-device buffer-to-buffer copy.
///
/// `cuMemcpyDtoD` is byte-granular: CUDA places no alignment requirement on either pointer or on `n`.
/// [`Enc::CopyBufferToBuffer`] does — `wgpu`'s `copy_buffer_to_buffer` requires both offsets and the size
/// to be multiples of [`COPY_ALIGNMENT`], and the host validator rejects the whole transfer otherwise.
/// Passing the raw extents straight through therefore refused every copy whose size or either offset was
/// not a multiple of four, which measured as 3488 of 3488 unaligned cases refused against 112 of 112
/// aligned cases accepted, on both the Metal and the reference executor.
///
/// The host requirement is real, so the fix is to handle the remainder rather than to relax the check.
/// The copy is split into an aligned middle, which goes on-device as one `CopyBufferToBuffer`, and up to
/// two unaligned edges, which go through the byte-granular readback + [`Cmd::WriteBuffer`] pair. When the
/// two pointers have DIFFERENT alignments modulo [`COPY_ALIGNMENT`] no aligned middle exists at all — a
/// device copy cannot shift bytes within a word — so the whole range takes the edge path.
///
/// Overlapping source and destination ranges are undefined in CUDA, and this does not define them. But
/// every byte the edge path needs is read BEFORE anything is written, so an overlap degrades to the same
/// answer a single memmove-free copy would give rather than to a partly-clobbered source.
pub fn memcpy_dtod(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    dst: DevicePtr,
    src: DevicePtr,
    n: u64,
) -> Result<()> {
    let _s = hl_log::hl_span!(hl_log::tag::CUDA, "memcpy_dtod");
    hl_log::hl_add!(hl_log::tag::CUDA, "d2d_bytes", n);
    let (sbuf, soff) = resolve_range(ctx, src, n, "cuMemcpyDtoD: dangling source pointer")?;
    let (dbuf, doff) = resolve_range(ctx, dst, n, "cuMemcpyDtoD: dangling destination pointer")?;
    if n == 0 {
        return Ok(());
    }

    let a = COPY_ALIGNMENT;
    // A device copy moves whole words at matching positions within them, so an aligned middle exists
    // only when both pointers sit at the same offset inside their word.
    let shifted = a > 1 && soff % a != doff % a;
    let head = if a <= 1 || shifted {
        0
    } else {
        // Bytes before the first position where BOTH offsets are word-aligned, clamped to the copy.
        (((a - soff % a) % a).min(n)) as usize
    };
    let middle = if a <= 1 {
        n
    } else if shifted {
        0
    } else {
        (n - head as u64) / a * a
    };
    let tail = (n - head as u64 - middle) as usize;

    // Read both edges before writing anything, so an overlapping copy cannot read bytes the middle
    // already overwrote.
    let head_bytes = if head > 0 {
        sink.read_buffer(sbuf, soff, head)?
    } else {
        Vec::new()
    };
    let tail_bytes = if tail > 0 {
        sink.read_buffer(sbuf, soff + head as u64 + middle, tail)?
    } else {
        Vec::new()
    };

    if middle > 0 {
        sink.submit(&[Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToBuffer {
                src: sbuf.0,
                src_offset: soff + head as u64,
                dst: dbuf.0,
                dst_offset: doff + head as u64,
                size: middle,
            }],
            signal: None,
        })])?;
    }
    if !head_bytes.is_empty() {
        sink.submit(&[Cmd::WriteBuffer {
            id: dbuf.0,
            offset: doff,
            data: head_bytes,
        }])?;
    }
    if !tail_bytes.is_empty() {
        sink.submit(&[Cmd::WriteBuffer {
            id: dbuf.0,
            offset: doff + head as u64 + middle,
            data: tail_bytes,
        }])?;
    }
    Ok(())
}

/// `cuMemcpyDtoH(host, src, n)` → resolve the device source to its (buffer, offset) for the caller to
/// read back out-of-band. Submits no command; returns the source location. Prefer [`read_dtoh`] when you
/// want the bytes — it performs the readback through the sink.
impl CudaContext {
    pub fn device_location(&self, src: DevicePtr) -> Result<(BufferId, u64)> {
        resolve(self, src, "cuMemcpyDtoH: dangling source pointer")
    }
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
    // Bound `n` against the source allocation BEFORE the readback: an attacker-controlled `n` must never
    // drive the sink's `read_buffer` to allocate/return bytes past the allocation (an unbounded host
    // readback / OOB read), so this is checked up front, not deferred to the executor.
    let (buf, off) = resolve_range(ctx, src, n as u64, "cuMemcpyDtoH: dangling source pointer")?;
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
    let (buf, off) = resolve_range(
        ctx,
        dst,
        pattern.len() as u64,
        "cuMemset: dangling destination pointer",
    )?;
    sink.submit(&[Cmd::WriteBuffer {
        id: buf.0,
        offset: off,
        data: pattern.to_vec(),
    }])?;
    Ok(())
}

/// `cuMemsetD8/D16/D32(dst, value, count)` — the element-wise fill from the RAW `(value, width, count)`,
/// expanded HERE rather than by the caller. Doing the expansion in the service is what makes it safe: an
/// attacker-controlled `count` is bounded before a single byte is allocated.
///
/// * `width * count` is a **checked** multiply — a `usize`/`u64` product that overflows is `OutOfBounds`,
///   never a wrap that under-allocates or a debug panic.
/// * the total fill length is bounded against the destination allocation (via [`resolve_range`]) BEFORE
///   the fill `Vec` is built, so `count = usize::MAX` can never drive a multi-GiB `Vec::with_capacity` /
///   OOM abort — the pre-check caps it at the destination's size (a small, real allocation).
///
/// This is the path the `cuMemsetD*` shims lower through; the pre-expanded [`memset`] above remains for
/// callers that already hold the byte pattern.
pub fn memset_elements(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    dst: DevicePtr,
    value: u64,
    width: usize,
    count: usize,
) -> Result<()> {
    if width == 0 || width > 8 {
        return Err(GpuError::Invalid(
            "cuMemset: element width must be 1..=8 bytes",
        ));
    }
    // Total fill bytes as a checked product — an attacker-controlled `count` can never overflow it.
    let bytes = (width as u64)
        .checked_mul(count as u64)
        .ok_or(GpuError::OutOfBounds)?;
    // Bound against the destination allocation BEFORE building the fill buffer: `bytes <= avail` after
    // this, so the `Vec` below is capped by the (small, real) destination size — never unbounded.
    let (buf, off) = resolve_range(ctx, dst, bytes, "cuMemset: dangling destination pointer")?;
    let el = &value.to_le_bytes()[..width];
    let mut data = Vec::with_capacity(bytes as usize);
    for _ in 0..count {
        data.extend_from_slice(el);
    }
    sink.submit(&[Cmd::WriteBuffer {
        id: buf.0,
        offset: off,
        data,
    }])?;
    Ok(())
}

/// `cuMemsetD*Async(dst, value, count, stream)` — the stream-ordered element-wise fill. Validates
/// `stream`, then lowers the SAME bounded fill as [`memset_elements`].
pub fn memset_elements_async(
    ctx: &mut CudaContext,
    sink: &mut dyn CommandSink,
    stream: Stream,
    dst: DevicePtr,
    value: u64,
    width: usize,
    count: usize,
) -> Result<()> {
    check_stream(ctx, stream, "cuMemsetAsync: invalid stream handle")?;
    memset_elements(ctx, sink, dst, value, width, count)
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
