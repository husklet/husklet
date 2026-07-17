//! PBO-style buffer mapping (`glMapBufferRange` / `glUnmapBuffer` / `glFlushMappedBufferRange`).
//!
//! Backed by the GL buffer's own host storage: `glMapBufferRange` grows the bound buffer's bytes to cover
//! the requested range and hands back a pointer INTO that storage (the C shim computes the raw pointer
//! from the returned `(name, offset)`); the app writes through it; `glUnmapBuffer` flushes the mapped
//! range to the device as a `WriteBuffer` (and the draw path already re-uploads the mutated bytes at
//! swap, so a mapped-then-drawn buffer is coherent). `glFlushMappedBufferRange` flushes a sub-range
//! explicitly while still mapped. This mirrors the reference shim's functional map, expressed as IR.

use crate::model::context::GlContext;
use crate::model::glconst::*;
use hl_gpu::protocol::model::descriptor::BufferDesc;
use hl_gpu::protocol::model::enums::buffer_usage;
use hl_gpu::{Cmd, CommandSink, Result};

/// `glMapBufferRange(target, offset, length, access)` — map `[offset, offset+length)` of the buffer bound
/// to `target`. Returns `(buffer_name, data_offset)` for the C shim to turn into a raw pointer into the
/// buffer's storage, or `None` (with the GL error set) on a bad range / no bound buffer / unknown buffer.
pub fn map_buffer_range(
    ctx: &mut GlContext,
    target: u32,
    offset: isize,
    length: isize,
    _access: u32,
) -> Option<(u32, usize)> {
    if offset < 0 || length < 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return None;
    }
    let name = ctx.buffer_for_target(target);
    if name == 0 {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return None;
    }
    // An unknown (e.g. deleted) buffer name — the binding named a buffer with no object.
    let Some(size) = ctx.buffers.get(name).map(|b| b.data.len()) else {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return None;
    };
    // The mapped range must lie within the buffer's current size (real GL: GL_INVALID_VALUE if
    // `offset + length > GL_BUFFER_SIZE`). This bounds the mapping so a hostile `offset`/`length` can never
    // grow the buffer's `Vec` to an unbounded (or overflowing) size and OOM/panic.
    let (off, len) = (offset as usize, length as usize);
    match off.checked_add(len) {
        Some(end) if end <= size => {}
        _ => {
            ctx.set_gl_error(GL_INVALID_VALUE);
            return None;
        }
    }
    match ctx.buffers.map_range(name, off, len) {
        Some(off) => Some((name, off)),
        None => {
            ctx.set_gl_error(GL_INVALID_OPERATION);
            None
        }
    }
}

/// `glUnmapBuffer(target)` — flush the mapped range of the buffer bound to `target` to the device as a
/// `WriteBuffer` and clear the mapping. Returns `GL_TRUE` (`1`) on success; `GL_FALSE` (`0`) with the GL
/// error set if nothing was mapped / no buffer is bound.
pub fn unmap_buffer(ctx: &mut GlContext, sink: &mut dyn CommandSink, target: u32) -> Result<u8> {
    let name = ctx.buffer_for_target(target);
    if name == 0 {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return Ok(GL_FALSE as u8);
    }
    let Some((offset, bytes)) = ctx.buffers.take_map(name) else {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return Ok(GL_FALSE as u8);
    };
    flush_bytes(ctx, sink, name, offset, bytes)?;
    Ok(GL_TRUE as u8)
}

/// `glFlushMappedBufferRange(target, offset, length)` — flush a sub-range (relative to the START of the
/// current mapping, per GL) of a still-mapped buffer to the device as a `WriteBuffer`. A negative range,
/// no bound buffer, or an unmapped buffer raises `GL_INVALID_OPERATION`/`GL_INVALID_VALUE`.
pub fn flush_mapped_range(
    ctx: &mut GlContext,
    sink: &mut dyn CommandSink,
    target: u32,
    offset: isize,
    length: isize,
) -> Result<()> {
    if offset < 0 || length < 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return Ok(());
    }
    let name = ctx.buffer_for_target(target);
    let map_off = match ctx.buffers.get(name).and_then(|b| b.mapped) {
        Some((o, _)) if name != 0 => o,
        _ => {
            ctx.set_gl_error(GL_INVALID_OPERATION);
            return Ok(());
        }
    };
    let abs = map_off + offset as usize;
    let bytes = ctx
        .buffers
        .range_bytes(name, abs, length as usize)
        .unwrap_or_default();
    if !bytes.is_empty() {
        ctx.buffers.mark_changed(name);
    }
    flush_bytes(ctx, sink, name, abs, bytes)
}

/// Emit + submit the flush of `bytes` at `data_offset` of GL buffer `name`: a fresh IR buffer sized to the
/// whole GL buffer, plus a `WriteBuffer` of the flushed range. A fresh IR id per flush keeps each upload
/// an independent, duplicate-id-free command (the draw path mints its own ids at swap).
fn flush_bytes(
    ctx: &mut GlContext,
    sink: &mut dyn CommandSink,
    name: u32,
    data_offset: usize,
    bytes: Vec<u8>,
) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let total = ctx
        .buffers
        .get(name)
        .map(|b| b.data.len())
        .unwrap_or(data_offset + bytes.len()) as u64;
    let ir = ctx.alloc_buffer_ir();
    let cmds = vec![
        Cmd::CreateBuffer(
            ir,
            BufferDesc {
                size: total,
                usage: buffer_usage::STORAGE | buffer_usage::COPY_DST,
                label: String::new(),
            },
        ),
        Cmd::WriteBuffer {
            id: ir,
            offset: data_offset as u64,
            data: bytes,
        },
    ];
    sink.submit(&cmds)
}
