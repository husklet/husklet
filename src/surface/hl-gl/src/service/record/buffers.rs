use super::*;

pub fn bind_buffer(ctx: &mut GlContext, target: u32, name: u32) -> bool {
    if !matches!(
        target,
        GL_ARRAY_BUFFER
            | GL_ELEMENT_ARRAY_BUFFER
            | GL_PIXEL_PACK_BUFFER
            | GL_PIXEL_UNPACK_BUFFER
            | GL_UNIFORM_BUFFER
            | GL_SHADER_STORAGE_BUFFER
            | GL_ATOMIC_COUNTER_BUFFER
            | GL_TRANSFORM_FEEDBACK_BUFFER
            | GL_DISPATCH_INDIRECT_BUFFER
            | GL_COPY_READ_BUFFER
            | GL_COPY_WRITE_BUFFER
            | GL_DRAW_INDIRECT_BUFFER
    ) {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return false;
    }
    if ctx.buffer_is_deleted(name) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return false;
    }
    ctx.buffers.ensure(name);
    ctx.mark_debug_object_materialized(GL_BUFFER_OBJECT, name);
    match target {
        GL_ARRAY_BUFFER => ctx.local.array_buffer = name,
        GL_ELEMENT_ARRAY_BUFFER => ctx.local.element_buffer = name,
        t => {
            if name == 0 {
                ctx.local.general_buffers.remove(&t);
            } else {
                ctx.local.general_buffers.insert(t, name);
            }
        }
    }
    true
}

/// `glBufferData(target, data, usage)` — fills the buffer currently bound to `target`.
pub fn buffer_data(ctx: &mut GlContext, target: u32, data: &[u8], usage: u32) {
    let name = ctx.buffer_for_target(target);
    if (1..=9).contains(&name) {
        hl_log::hl_debug!(
            hl_log::tag::GL,
            "[UBO_DUMP] glBufferData target={target:#x} name={name} len={} usage={usage:#x}",
            data.len()
        );
    }
    if ctx.buffers.is_mapped(name) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
    } else if name != 0 {
        ctx.buffers.set_data(name, target, data, usage);
    } else {
        // ES 3.0 §2.9.2: with NO buffer bound to `target`, `glBufferData` is GL_INVALID_OPERATION. This
        // was a silent no-op, so an application whose binding had reverted — most often because the buffer
        // was deleted, which correctly unbinds it — uploaded into nothing and read GL_NO_ERROR back.
        ctx.set_gl_error(GL_INVALID_OPERATION);
    }
}

/// `glBufferSubData(target, offset, data)`. A range that overflows or reaches beyond the bound buffer's
/// current size is `GL_INVALID_VALUE` (real GL) — this also bounds the write so a hostile `offset` can
/// never grow the buffer's storage to an unbounded (or overflowing) size and panic/OOM.
pub fn buffer_sub_data(ctx: &mut GlContext, target: u32, offset: usize, data: &[u8]) {
    let name = ctx.buffer_for_target(target);
    if (1..=9).contains(&name) {
        hl_log::hl_debug!(
            hl_log::tag::GL,
            "[UBO_DUMP] glBufferSubData target={target:#x} name={name} off={offset} len={}",
            data.len()
        );
    }
    if ctx.buffers.is_mapped(name) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
    } else if name != 0 {
        let size = ctx.buffers.get(name).map(|b| b.data.len()).unwrap_or(0);
        match offset.checked_add(data.len()) {
            Some(end) if end <= size => ctx.buffers.set_sub_data(name, offset, data),
            _ => ctx.set_gl_error(GL_INVALID_VALUE),
        }
    }
}

/// `glDeleteBuffers` (one name).
impl GlContext {
    pub fn delete_buffer(&mut self, name: u32) -> bool {
        self.clear_object_label(GL_BUFFER_OBJECT, name);
        if self.local.array_buffer == name {
            self.local.array_buffer = 0;
        }
        if self.local.element_buffer == name {
            self.local.element_buffer = 0;
        }
        self.local
            .general_buffers
            .retain(|_, buffer| *buffer != name);
        self.local
            .indexed_buffers
            .retain(|_, binding| binding.buffer != name);
        self.local
            .transform_feedbacks
            .remove_buffer_from_bound(name);
        for attribute in &mut self.local.attr {
            if attribute.buffer == name {
                attribute.buffer = 0;
            }
        }
        for binding in &mut self.local.vertex_bindings {
            if binding.buffer == name {
                binding.buffer = 0;
            }
        }
        // Retire the buffer's resident IR ids (queued Destroy for the next frame) so its residency is reclaimed.
        self.retire_buffer(name);
        self.buffers.delete(name)
    }
}

/// `glCopyBufferSubData(readTarget, writeTarget, readOffset, writeOffset, size)` — copy `size` bytes
/// between the buffers bound to the two targets (`gl_shim.c` parity, a CPU-side byte copy). A negative
/// offset/size → `GL_INVALID_VALUE`; an out-of-range range is an honest no-op (nothing is copied).
pub fn copy_buffer_sub_data(
    ctx: &mut GlContext,
    read_target: u32,
    write_target: u32,
    read_off: isize,
    write_off: isize,
    size: isize,
) {
    if read_off < 0 || write_off < 0 || size < 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    let rb = ctx.buffer_for_target(read_target);
    let wb = ctx.buffer_for_target(write_target);
    hl_log::hl_debug!(hl_log::tag::GL, "[UBO_DUMP] glCopyBufferSubData rt={read_target:#x} wt={write_target:#x} rb={rb} wb={wb} ro={read_off} wo={write_off} size={size}");
    let (ro, wo, n) = (read_off as usize, write_off as usize, size as usize);
    if rb == 0 || wb == 0 {
        return;
    }
    if ctx.buffers.is_mapped(rb) || ctx.buffers.is_mapped(wb) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    let src = match ctx.buffers.range_bytes(rb, ro, n) {
        Some(s) if s.len() == n => s,
        _ => return, // out-of-range read source: no-op
    };
    // Grow the write buffer to cover the destination range, then overwrite it.
    if ctx
        .buffers
        .get(wb)
        .map(|b| b.data.len() < wo + n)
        .unwrap_or(true)
    {
        return; // destination out of range: no-op (matches gl_shim.c's bounds guard)
    }
    ctx.buffers.set_sub_data(wb, wo, &src);
}

// ---- indexed buffer bindings (glBindBufferBase / glBindBufferRange) -------------------------------

use crate::model::context::IndexedBinding;

/// The per-index binding cap for an indexed-buffer `target`, or `None` if `target` is not a valid indexed
/// target (`glBindBufferBase`/`glBindBufferRange` raise `GL_INVALID_ENUM`).
impl IndexedBinding {
    pub(super) fn target_cap(target: u32) -> Option<u32> {
        match target {
            GL_UNIFORM_BUFFER => Some(MAX_UNIFORM_BUFFER_BINDINGS),
            GL_SHADER_STORAGE_BUFFER => Some(MAX_SHADER_STORAGE_BUFFER_BINDINGS),
            GL_ATOMIC_COUNTER_BUFFER => Some(MAX_ATOMIC_COUNTER_BUFFER_BINDINGS),
            GL_TRANSFORM_FEEDBACK_BUFFER => Some(MAX_TRANSFORM_FEEDBACK_BUFFERS),
            _ => None,
        }
    }
}

/// `glBindBufferBase(target, index, buffer)` — bind the whole `buffer` to indexed slot `index` of `target`
/// (and the generic target binding). A UBO/SSBO binding feeds a `glDispatchCompute` bind group.
pub fn bind_buffer_base(ctx: &mut GlContext, target: u32, index: u32, buffer: u32) {
    bind_indexed_buffer(ctx, target, index, buffer, 0, 0, true);
}

/// `glBindBufferRange(target, index, buffer, offset, size)` — bind `[offset, offset+size)` of `buffer` to
/// indexed slot `index` (`size == 0` from `glBindBufferBase` = the whole buffer). Honest GL errors: a
/// non-indexed `target` → `GL_INVALID_ENUM`; `index >= cap` or a non-zero `buffer` with a non-positive
/// size / negative offset → `GL_INVALID_VALUE` (first-error-wins).
pub fn bind_buffer_range(
    ctx: &mut GlContext,
    target: u32,
    index: u32,
    buffer: u32,
    offset: isize,
    size: isize,
) {
    bind_indexed_buffer(ctx, target, index, buffer, offset, size, false);
}

fn bind_indexed_buffer(
    ctx: &mut GlContext,
    target: u32,
    index: u32,
    buffer: u32,
    offset: isize,
    size: isize,
    base: bool,
) {
    let Some(cap) = IndexedBinding::target_cap(target) else {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    };
    if index >= cap {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if buffer != 0 && !base {
        let alignment = match target {
            GL_UNIFORM_BUFFER => crate::service::query::UNIFORM_BUFFER_OFFSET_ALIGNMENT as usize,
            GL_SHADER_STORAGE_BUFFER => {
                crate::service::query::SHADER_STORAGE_BUFFER_OFFSET_ALIGNMENT as usize
            }
            _ => 4,
        };
        let valid_numbers = offset >= 0 && size > 0 && (offset as usize).is_multiple_of(alignment);
        let in_bounds = valid_numbers
            && ctx
                .buffers
                .get(buffer)
                .and_then(|object| {
                    (offset as usize)
                        .checked_add(size as usize)
                        .map(|end| end <= object.data.len())
                })
                .unwrap_or(false);
        if !in_bounds {
            ctx.set_gl_error(GL_INVALID_VALUE);
            return;
        }
    }
    if target == GL_UNIFORM_BUFFER {
        hl_log::hl_debug!(hl_log::tag::GL, "[UBO_DUMP] glBindBufferRange target={target:#x} index={index} buffer={buffer} offset={offset} size={size}");
    }
    // Bind the generic target too (GL binds both), so a later glBufferData(target, …) fills this buffer.
    if !bind_buffer(ctx, target, buffer) {
        return;
    }
    if buffer == 0 {
        ctx.local.indexed_buffers.remove(&(target, index));
        if target == GL_TRANSFORM_FEEDBACK_BUFFER {
            ctx.local.transform_feedbacks.set_binding(index, None);
        }
    } else {
        let binding = IndexedBinding {
            buffer,
            offset,
            size,
        };
        ctx.local.indexed_buffers.insert((target, index), binding);
        if target == GL_TRANSFORM_FEEDBACK_BUFFER {
            ctx.local
                .transform_feedbacks
                .set_binding(index, Some(binding));
        }
    }
}

/// The indexed-buffer binding at `(target, index)` (`glBindBufferBase`/`glBindBufferRange`), or `None` if
/// nothing is bound there. Exposed for the lowering tests + the compute bind-group builder.
pub fn indexed_buffer_binding(ctx: &GlContext, target: u32, index: u32) -> Option<IndexedBinding> {
    ctx.local.indexed_buffers.get(&(target, index)).copied()
}

// ---- MRT draw/read buffer selection (glDrawBuffers / glReadBuffer) --------------------------------

/// `glDrawBuffers(bufs)` — record the fragment-output color-buffer list. Each entry must be `GL_NONE`,
/// `GL_BACK` (default framebuffer), or a `GL_COLOR_ATTACHMENT{i}` (FBO) — else `GL_INVALID_ENUM`. This
/// model renders a single color target, so the list round-trips faithfully but only the first attachment
/// is materialized (an honest partial).
impl GlContext {
    pub fn set_draw_buffers(&mut self, bufs: &[u32]) {
        for &b in bufs {
            let ok = b == GL_NONE
                || b == GL_BACK
                || (GL_COLOR_ATTACHMENT0..=GL_COLOR_ATTACHMENT0 + 15).contains(&b);
            if !ok {
                self.set_gl_error(GL_INVALID_ENUM);
                return;
            }
        }
        self.local.draw_buffers = bufs.to_vec();
    }

    /// `glReadBuffer(src)` — select the color buffer subsequent `glReadPixels`/blit reads from. `src` must be
    /// `GL_NONE`, `GL_BACK`, or a `GL_COLOR_ATTACHMENT{i}` (else `GL_INVALID_ENUM`).
    pub fn set_read_buffer(&mut self, src: u32) {
        let ok = src == GL_NONE
            || src == GL_BACK
            || (GL_COLOR_ATTACHMENT0..=GL_COLOR_ATTACHMENT0 + 15).contains(&src);
        if !ok {
            self.set_gl_error(GL_INVALID_ENUM);
            return;
        }
        self.local.read_buffer_src = src;
    }
}

pub const DRAW_BUFFERS: fn(&mut GlContext, &[u32]) = GlContext::set_draw_buffers;
pub const READ_BUFFER: fn(&mut GlContext, u32) = GlContext::set_read_buffer;
pub use DRAW_BUFFERS as draw_buffers;
pub use READ_BUFFER as read_buffer;
