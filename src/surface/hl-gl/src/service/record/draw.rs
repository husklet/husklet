use super::*;
use crate::result::GL_OUT_OF_MEMORY;

// ---- draw + clear recording ----------------------------------------------------------------------

/// `glClear(mask)` — record a full-surface clear rect at the current clear color, carrying the app's
/// buffer mask. The mask decides WHICH planes the lowering may touch: a `GL_DEPTH_BUFFER_BIT`-only clear
/// must not repaint the color buffer, which is what dropping the mask here used to do.
impl GlContext {
    /// Record `glClear(GL_COLOR_BUFFER_BIT)` (the historical mask-less entry point).
    pub fn record_clear(&mut self) {
        self.record_clear_buffers(GL_COLOR_BUFFER_BIT);
    }

    pub fn record_clear_buffers(&mut self, mask: u32) {
        // GL: any bit outside the three buffer bits is GL_INVALID_VALUE and the call records nothing.
        const BITS: u32 = GL_COLOR_BUFFER_BIT | GL_DEPTH_BUFFER_BIT | GL_STENCIL_BUFFER_BIT;
        if mask & !BITS != 0 {
            self.set_gl_error(GL_INVALID_VALUE);
            return;
        }
        // ES 3.0 rasterizer discard suppresses both primitive rasterization and framebuffer clear
        // operations. Keep validation above observable, but record no framebuffer work while enabled.
        if self.local.pipeline.rasterizer_discard {
            return;
        }
        let (w, h) = self.target_wh();
        let mut d = DrawCall {
            is_clear: true,
            clear_mask: mask,
            ..self.snapshot(false)
        };
        // Scissoring a clear to a box that covers the complete draw target changes no observable pixel.
        // Normalize that state here, where the bound target's dimensions are known, so lowering retains
        // attachment load ops instead of manufacturing an internal rect-clear draw. Boxes may extend past
        // either edge; GL intersects them with the framebuffer.
        if d.scissor_enabled {
            let [x, y, sw, sh] = d.scissor;
            if x <= 0 && y <= 0 && x.saturating_add(sw) >= w && y.saturating_add(sh) >= h {
                d.scissor_enabled = false;
            }
        }
        d.clear_rect = [0, 0, w, h];
        // Nothing left to write: every named plane is masked off entirely. Recording it would still be
        // harmless, but dropping it keeps a masked-off clear from becoming a pass boundary. A PARTIALLY
        // masked clear does write something and is kept — the frame builder paints it with the rect clear
        // (see `service::frame`), which carries `glColorMask` and `glStencilMask` on the pipeline.
        if !d.writes_any_plane() {
            return;
        }
        self.record_draw(d);
    }
}

pub const ENABLE_VERTEX_ATTRIB: fn(&mut GlContext, usize) = GlContext::enable_vertex_attrib;
pub const DISABLE_VERTEX_ATTRIB: fn(&mut GlContext, usize) = GlContext::disable_vertex_attrib;
pub const CLEAR_COLOR: fn(&mut GlContext, [f32; 4]) = GlContext::set_clear_color;
pub const CLEAR_DEPTH: fn(&mut GlContext, f32) = GlContext::set_clear_depth;
pub const BLEND_COLOR: fn(&mut GlContext, [f32; 4]) = GlContext::set_blend_color;
pub const DEPTH_FUNC: fn(&mut GlContext, u32) = GlContext::set_depth_func;
pub const DEPTH_RANGE: fn(&mut GlContext, f32, f32) = GlContext::set_depth_range;
pub const DEPTH_MASK: fn(&mut GlContext, bool) = GlContext::set_depth_mask;
pub const CLEAR_STENCIL: fn(&mut GlContext, i32) = GlContext::set_clear_stencil;
pub const STENCIL_MASK: fn(&mut GlContext, u32) = GlContext::set_stencil_mask;
pub const CULL_FACE: fn(&mut GlContext, u32) = GlContext::set_cull_face;
pub const FRONT_FACE: fn(&mut GlContext, u32) = GlContext::set_front_face;
pub const VIEWPORT: fn(&mut GlContext, [i32; 4]) = GlContext::set_viewport;
pub const SCISSOR: fn(&mut GlContext, [i32; 4]) = GlContext::set_scissor;
pub const ENABLE: fn(&mut GlContext, u32) = GlContext::enable;
pub const DISABLE: fn(&mut GlContext, u32) = GlContext::disable;
pub const CLEAR: fn(&mut GlContext) = GlContext::record_clear;
pub const CLEAR_BUFFERS: fn(&mut GlContext, u32) = GlContext::record_clear_buffers;
pub use BLEND_COLOR as blend_color;
pub use CLEAR as clear;
pub use CLEAR_BUFFERS as clear_buffers;
pub use CLEAR_COLOR as clear_color;
pub use CLEAR_DEPTH as clear_depth;
pub use CLEAR_STENCIL as clear_stencil;
pub use CULL_FACE as cull_face;
pub use DEPTH_FUNC as depth_func;
pub use DEPTH_MASK as depth_mask;
pub use DEPTH_RANGE as depth_range;
pub use DISABLE as disable;
pub use DISABLE_VERTEX_ATTRIB as disable_vertex_attrib;
pub use ENABLE as enable;
pub use ENABLE_VERTEX_ATTRIB as enable_vertex_attrib;
pub use FRONT_FACE as front_face;
pub use SCISSOR as scissor;
pub use STENCIL_MASK as stencil_mask;
pub use VIEWPORT as viewport;

/// Read one enabled CLIENT-side vertex attribute's bytes into a tightly-packed, de-interleaved buffer
/// spanning logical vertices `[0, vert_end)`, honoring `stride` (`0` = tightly packed by
/// `size * component_size`). The transient buffer is indexed by the draw's own `first_vertex` / index
/// values, so it must span from vertex `0` (not `first`) up to the last vertex the draw fetches.
///
/// UNSAFE: dereferences the client pointer the app passed to `glVertexAttribPointer` (an address in GUEST
/// memory). This is valid ONLY here, at draw-record time, in the guest process where that pointer lives —
/// the deferred model cannot defer the read to swap (the client memory may be reused by then).
unsafe fn read_client_attr(
    base: usize,
    size: i32,
    kind: u32,
    stride: i32,
    vert_end: usize,
) -> Vec<u8> {
    let elem = crate::model::glconst::GlType(kind).vertex_element_size(size);
    let st = if stride > 0 { stride as usize } else { elem };
    let mut out = Vec::with_capacity(vert_end.saturating_mul(elem));
    for v in 0..vert_end {
        let src = (base + v * st) as *const u8;
        out.extend_from_slice(std::slice::from_raw_parts(src, elem));
    }
    out
}

/// Capture every enabled attribute drawn with NO vertex buffer bound (`buffer == 0`, a client pointer) into
/// the draw's `client_vbufs`, each spanning `[0, vert_end)`. A null client pointer (`offset == 0`) is
/// skipped (nothing to read). An all-VBO draw records nothing here, so it lowers unchanged.
impl DrawCall {
    /// `vert_end` is `None` when the draw's vertex span overflowed. Returns `false` when the draw names a
    /// client-array span this driver refuses to read; an all-VBO draw captures nothing and always succeeds,
    /// however wide its span, because no guest memory is dereferenced for it.
    pub(super) fn capture_client_vbufs(&mut self, vert_end: Option<usize>) -> bool {
        let attrs = self.attrs;
        if !attrs
            .iter()
            .any(|a| a.enabled && a.buffer == 0 && a.offset != 0)
        {
            return true;
        }
        let Some(vert_end) = vert_end.filter(|end| *end <= MAX_CLIENT_VERTS) else {
            return false;
        };
        if vert_end == 0 {
            return true;
        }
        for (i, a) in attrs.iter().enumerate() {
            if !a.enabled || a.buffer != 0 || a.offset == 0 {
                continue;
            }
            let data = unsafe { read_client_attr(a.offset, a.size, a.kind, a.stride, vert_end) };
            self.client_vbufs.push(crate::model::program::ClientArray {
                location: i,
                data,
                size: a.size,
                kind: a.kind,
                normalized: a.normalized,
                integer: a.integer,
                divisor: a.divisor,
            });
        }
        true
    }
}

/// Read `count` indices of type `index_type` from `bytes` (little-endian) at index position `k`.
pub(super) fn index_at(bytes: &[u8], index_type: u32, k: usize) -> usize {
    match index_type {
        GL_UNSIGNED_BYTE => bytes.get(k).copied().unwrap_or(0) as usize,
        GL_UNSIGNED_SHORT => {
            let o = k * 2;
            u16::from_le_bytes([
                bytes.get(o).copied().unwrap_or(0),
                bytes.get(o + 1).copied().unwrap_or(0),
            ]) as usize
        }
        _ => {
            let o = k * 4;
            u32::from_le_bytes([
                bytes.get(o).copied().unwrap_or(0),
                bytes.get(o + 1).copied().unwrap_or(0),
                bytes.get(o + 2).copied().unwrap_or(0),
                bytes.get(o + 3).copied().unwrap_or(0),
            ]) as usize
        }
    }
}

/// The widest client-array vertex span this driver will read out of guest memory for one draw.
///
/// A client array carries no length: the application promises the memory behind the pointer it gave
/// `glVertexAttribPointer`, and the specification leaves reading past it undefined (Mesa faults too). A
/// guest must not be able to kill the driver regardless, so a draw whose vertex span exceeds this bound is
/// refused with `GL_INVALID_OPERATION` and records nothing. The bound is the advertised
/// `GL_MAX_ELEMENTS_VERTICES`, the largest vertex count the driver tells applications it wants per draw.
const MAX_CLIENT_VERTS: usize = crate::service::query::MAX_ELEMENTS_VERTICES as usize;

/// The indexed-draw capture: handle client-side vertex arrays and/or a client-side index array for a
/// `glDrawElements*`. Covers all four combinations (pure-VBO returns immediately, leaving the draw
/// unchanged):
/// * client index array (no element buffer bound) → read the client index bytes, promote `u8`→`u16`
///   (the index IR has no `u8` format), store them in `client_indices`, and rewrite `index_type`.
/// * client vertex arrays → scan the index range for the max index and capture each client array over
///   `[0, maxIndex + baseVertex + 1)` (the exact vertex span the fetch touches).
///
/// The index source is the client pointer when no element-array-buffer is bound, else the bound buffer's
/// bytes at `offset` (so a client-vertex + VBO-index mix still finds the right vertex span).
pub(super) fn capture_indexed(
    ctx: &GlContext,
    d: &mut DrawCall,
    count: i32,
    index_type: u32,
    offset: usize,
    base_vertex: i32,
) -> bool {
    let has_client_vbuf = d
        .attrs
        .iter()
        .any(|a| a.enabled && a.buffer == 0 && a.offset != 0);
    let client_index = d.elem_buf == 0;
    if !has_client_vbuf && !client_index {
        return true; // pure-VBO indexed draw — the existing VBO path handles everything.
    }
    let isz = crate::model::glconst::GlType(index_type).component_size();
    let need = count.max(0) as usize * isz;
    // The raw index bytes: from the client pointer, or from the bound element-array-buffer at `offset`.
    let idx_bytes: Vec<u8> = if client_index {
        if offset == 0 || count <= 0 {
            return true;
        }
        unsafe { std::slice::from_raw_parts(offset as *const u8, need).to_vec() }
    } else {
        match ctx
            .buffers
            .get(d.elem_buf)
            .and_then(|b| b.data.get(offset..(offset + need).min(b.data.len())))
        {
            Some(s) => s.to_vec(),
            None => return true,
        }
    };
    // A client index array is promoted (u8→u16) into the final index-buffer encoding + type rewrite.
    if client_index {
        if index_type == GL_UNSIGNED_BYTE {
            let mut promoted = Vec::with_capacity(count as usize * 2);
            for k in 0..count as usize {
                promoted
                    .extend_from_slice(&(index_at(&idx_bytes, index_type, k) as u16).to_le_bytes());
            }
            d.client_indices = promoted;
            d.index_type = GL_UNSIGNED_SHORT;
        } else {
            d.client_indices = idx_bytes.clone();
        }
    }
    // Client vertex arrays span [0, maxIndex + baseVertex + 1) — the widest vertex the fetch reaches.
    if has_client_vbuf {
        let mut max_idx = 0usize;
        for k in 0..count.max(0) as usize {
            let i = index_at(&idx_bytes, index_type, k);
            if i > max_idx {
                max_idx = i;
            }
        }
        // The widest vertex the fetch reaches. A u32 index can name a span far past any array the
        // application actually provided, so the capture bound applies here too.
        let vert_end = max_idx
            .saturating_add(base_vertex.max(0) as usize)
            .saturating_add(1);
        return d.capture_client_vbufs(Some(vert_end));
    }
    true
}

/// The scissor-clipped sample footprint of one draw — its viewport rectangle intersected with the scissor
/// box (when `GL_SCISSOR_TEST` is enabled), times the instance count. Used to feed an open occlusion query
/// so `GL_ANY_SAMPLES_PASSED` reflects reality: a visible draw contributes a positive area, a draw whose
/// scissor excludes its viewport (or a zero-area scissor) contributes `0`. This is an UPPER BOUND on the
/// true rasterized sample count (it does not clip to the primitive itself), which is exactly what the
/// boolean `GL_ANY_SAMPLES_PASSED` needs — nonzero iff the draw could rasterize any sample.
impl DrawCall {
    pub(super) fn coverage(&self) -> u64 {
        if self.rasterizer_discard {
            return 0;
        }
        let [vx, vy, vw, vh] = self.viewport;
        let (mut x0, mut y0, mut x1, mut y1) =
            (vx, vy, vx.saturating_add(vw), vy.saturating_add(vh));
        if self.scissor_enabled {
            let [sx, sy, sw, sh] = self.scissor;
            x0 = x0.max(sx);
            y0 = y0.max(sy);
            x1 = x1.min(sx.saturating_add(sw));
            y1 = y1.min(sy.saturating_add(sh));
        }
        let w = (x1 - x0).max(0) as u64;
        let h = (y1 - y0).max(0) as u64;
        w * h * self.instance_count.max(1) as u64
    }
}

/// Feed `d`'s coverage into an open occlusion query (no-op when none is armed). Called for every recorded
/// geometry draw (never for a `glClear` — clears do not affect occlusion queries).
impl GlContext {
    pub(super) fn accumulate_occlusion(&mut self, d: &DrawCall) {
        self.local.queries.accumulate(d.coverage());
    }
}

fn primitive_capture(mode: u32, count: u32) -> Option<(u32, u32, u32)> {
    let (class, vertices, primitives) = match mode {
        GL_POINTS => (GL_POINTS, count, count),
        GL_LINES => (GL_LINES, count / 2 * 2, count / 2),
        GL_LINE_STRIP => {
            let complete = count >= 2;
            (GL_LINES, if complete { count } else { 0 }, count.saturating_sub(1))
        }
        0x0002 /* GL_LINE_LOOP */ => {
            let complete = count >= 2;
            (GL_LINES, if complete { count } else { 0 }, if complete { count } else { 0 })
        }
        GL_TRIANGLES => (GL_TRIANGLES, count / 3 * 3, count / 3),
        GL_TRIANGLE_STRIP | 0x0006 /* GL_TRIANGLE_FAN */ => {
            let complete = count >= 3;
            (
                GL_TRIANGLES,
                if complete { count } else { 0 },
                count.saturating_sub(2),
            )
        }
        _ => return None,
    };
    Some((class, vertices, primitives))
}

/// Attach the active transform-feedback destination to a completed draw snapshot. Cursor arithmetic and
/// every bound range are validated before any cursor advances, so an overflowing/refused draw is atomic.
fn capture_transform_feedback(ctx: &mut GlContext, draw: &mut DrawCall) -> bool {
    let object = ctx.local.transform_feedbacks.bound_obj();
    if !object.active || object.paused {
        return true;
    }
    let Some(layout) = ctx
        .programs
        .program(draw.prog)
        .and_then(|program| program.transform_feedback_layout.clone())
    else {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return false;
    };
    let Some((class, vertices, primitives)) =
        primitive_capture(draw.mode, draw.count.max(0) as u32)
    else {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return false;
    };
    if class != object.primitive_mode {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return false;
    }
    let Some(vertices) = vertices.checked_mul(draw.instance_count) else {
        ctx.set_gl_error(GL_OUT_OF_MEMORY);
        return false;
    };
    let Some(primitives) = primitives.checked_mul(draw.instance_count) else {
        ctx.set_gl_error(GL_OUT_OF_MEMORY);
        return false;
    };
    let mut lengths = [0u64; 4];
    let mut absolute = [0u64; 4];
    for index in 0..layout.buffers as usize {
        let Some(binding) = object.bindings[index] else {
            ctx.set_gl_error(GL_INVALID_OPERATION);
            return false;
        };
        let Some(buffer_len) = ctx
            .buffers
            .get(binding.buffer)
            .map(|buffer| buffer.data.len() as u64)
        else {
            ctx.set_gl_error(GL_INVALID_OPERATION);
            return false;
        };
        let Ok(binding_offset) = u64::try_from(binding.offset) else {
            ctx.set_gl_error(GL_INVALID_OPERATION);
            return false;
        };
        let binding_len = if binding.size == 0 {
            buffer_len.saturating_sub(binding_offset)
        } else {
            binding.size as u64
        };
        let Some(length) = u64::from(layout.strides[index]).checked_mul(u64::from(vertices)) else {
            ctx.set_gl_error(GL_OUT_OF_MEMORY);
            return false;
        };
        let Some(end) = object.cursors[index].checked_add(length) else {
            ctx.set_gl_error(GL_OUT_OF_MEMORY);
            return false;
        };
        if end > binding_len {
            ctx.set_gl_error(GL_INVALID_OPERATION);
            return false;
        }
        let Some(offset) = binding_offset.checked_add(object.cursors[index]) else {
            ctx.set_gl_error(GL_OUT_OF_MEMORY);
            return false;
        };
        absolute[index] = offset;
        lengths[index] = length;
    }
    let Some(previous) = ctx.local.transform_feedbacks.advance(lengths) else {
        ctx.set_gl_error(GL_OUT_OF_MEMORY);
        return false;
    };
    debug_assert!(previous
        .iter()
        .enumerate()
        .all(|(index, cursor)| absolute[index]
            == object.bindings[index].map_or(0, |binding| binding.offset as u64 + cursor)));
    ctx.local.queries.accumulate_transform_feedback(primitives);
    draw.transform_feedback = Some(crate::model::program::TransformFeedbackCapture {
        layout,
        bindings: object.bindings,
        byte_offsets: absolute,
        byte_lengths: lengths,
        vertices,
        primitives,
    });
    true
}

/// `glDrawArrays(mode, first, count)` — snapshot the bound state and append the draw (one instance).
pub fn draw_arrays(ctx: &mut GlContext, mode: u32, first: i32, count: i32) {
    draw_arrays_instanced(ctx, mode, first, count, 1);
}

/// `glDrawArraysInstanced(mode, first, count, instances)` — like [`draw_arrays`] with an explicit
/// instance count, recorded onto the draw so the frame builder lowers a `Draw { instance_count }`. A
/// negative first, count, or instance count raises `GL_INVALID_VALUE` (first-error-wins) and records
/// nothing; a zero count (or vertex count) is a legal no-op.
pub fn draw_arrays_instanced(
    ctx: &mut GlContext,
    mode: u32,
    first: i32,
    count: i32,
    instances: i32,
) {
    if first < 0 || instances < 0 || count < 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if !ctx.draw_preconditions(mode, None) {
        return;
    }
    if ctx.draw_uses_mapped_buffer() {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if count == 0 || instances == 0 {
        return;
    }
    if ctx.is_enabled(GL_RASTERIZER_DISCARD) && !ctx.local.transform_feedbacks.bound_obj().active {
        return;
    }
    let mut d = ctx.snapshot(true);
    d.mode = mode;
    d.first = first;
    d.count = count;
    d.instance_count = instances as u32;
    // Client-side vertex arrays span [0, first+count): the transient buffer is indexed by `first_vertex`.
    // A span that overflows, or that runs past the transient capture bound, is refused rather than read:
    // the client array is promised by the application and a guest must not be able to fault the driver.
    if !d.capture_client_vbufs(first.checked_add(count).map(|end| end as usize)) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if !capture_transform_feedback(ctx, &mut d) {
        return;
    }
    ctx.accumulate_occlusion(&d);
    ctx.record_draw(d);
}

/// `glDrawElements(mode, count, index_type, offset)` — snapshot + append an indexed draw (one instance).
pub fn draw_elements(ctx: &mut GlContext, mode: u32, count: i32, index_type: u32, offset: usize) {
    draw_elements_instanced(ctx, mode, count, index_type, offset, 1);
}

/// `glDrawElementsInstanced(mode, count, index_type, offset, instances)` — like [`draw_elements`] with
/// an explicit instance count, lowered to a `DrawIndexed { instance_count }`. A negative instance count
/// raises `GL_INVALID_VALUE` and records nothing.
pub fn draw_elements_instanced(
    ctx: &mut GlContext,
    mode: u32,
    count: i32,
    index_type: u32,
    offset: usize,
    instances: i32,
) {
    if instances < 0 || count < 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if !ctx.draw_preconditions(mode, Some(index_type)) {
        return;
    }
    if ctx.draw_uses_mapped_buffer() {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if count == 0 || instances == 0 {
        return;
    }
    if ctx.is_enabled(GL_RASTERIZER_DISCARD) && !ctx.local.transform_feedbacks.bound_obj().active {
        return;
    }
    let mut d = ctx.snapshot(true);
    d.mode = mode;
    d.count = count;
    d.indexed = true;
    d.index_type = index_type;
    d.index_offset = offset;
    d.instance_count = instances as u32;
    d.elem_buf = ctx.local.element_buffer;
    if !capture_indexed(ctx, &mut d, count, index_type, offset, 0) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if !capture_transform_feedback(ctx, &mut d) {
        return;
    }
    ctx.accumulate_occlusion(&d);
    ctx.record_draw(d);
}

/// `glDrawElementsBaseVertex(mode, count, type, offset, basevertex)` — an indexed draw whose fetched
/// index values are all offset by `basevertex` before the vertex fetch. Recorded onto the draw's
/// `base_vertex` (the frame builder folds it into the vertex fetch).
pub fn draw_elements_base_vertex(
    ctx: &mut GlContext,
    mode: u32,
    count: i32,
    index_type: u32,
    offset: usize,
    base_vertex: i32,
) {
    draw_elements_instanced_base_vertex(ctx, mode, count, index_type, offset, 1, base_vertex);
}

/// `glDrawElementsInstancedBaseVertex(...)` — the instanced + base-vertex indexed draw.
pub fn draw_elements_instanced_base_vertex(
    ctx: &mut GlContext,
    mode: u32,
    count: i32,
    index_type: u32,
    offset: usize,
    instances: i32,
    base_vertex: i32,
) {
    if instances < 0 || count < 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if !ctx.draw_preconditions(mode, Some(index_type)) {
        return;
    }
    if ctx.draw_uses_mapped_buffer() {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if count == 0 || instances == 0 {
        return;
    }
    if ctx.is_enabled(GL_RASTERIZER_DISCARD) && !ctx.local.transform_feedbacks.bound_obj().active {
        return;
    }
    let mut d = ctx.snapshot(true);
    d.mode = mode;
    d.count = count;
    d.indexed = true;
    d.index_type = index_type;
    d.index_offset = offset;
    d.instance_count = instances as u32;
    d.base_vertex = base_vertex;
    d.elem_buf = ctx.local.element_buffer;
    if !capture_indexed(ctx, &mut d, count, index_type, offset, base_vertex) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if !capture_transform_feedback(ctx, &mut d) {
        return;
    }
    ctx.accumulate_occlusion(&d);
    ctx.record_draw(d);
}

/// `glDrawRangeElements(mode, start, end, count, type, offset)` — a bounded indexed draw. `[start, end]`
/// is a driver hint about the referenced index range (used for buffer-residency optimization); it does not
/// change what is drawn, so this records the same indexed draw as `glDrawElements`. `end < start` →
/// `GL_INVALID_VALUE`.
// The arguments intentionally mirror glDrawRangeElements plus the supported base-vertex variant.
#[allow(clippy::too_many_arguments)]
pub fn draw_range_elements(
    ctx: &mut GlContext,
    mode: u32,
    start: u32,
    end: u32,
    count: i32,
    index_type: u32,
    offset: usize,
    base_vertex: i32,
) {
    if end < start {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    draw_elements_base_vertex(ctx, mode, count, index_type, offset, base_vertex);
}

/// Read a `u32` at byte `off` from the buffer bound to `target` (little-endian), or `None` out of range.
pub(super) fn read_u32_at(ctx: &GlContext, target: u32, off: usize) -> Option<u32> {
    let name = ctx.buffer_for_target(target);
    let b = ctx.buffers.get(name)?;
    let bytes = b.data.get(off..off + 4)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

/// `glDrawArraysIndirect(mode, indirect)` — an array draw whose `{count, instanceCount, first,
/// baseInstance}` are read from the buffer bound to `GL_DRAW_INDIRECT_BUFFER` at byte offset `indirect`.
/// This model reads the CPU-side indirect struct and records the equivalent instanced array draw. A
/// missing / too-short indirect buffer is an honest no-op (nothing drawn).
pub fn draw_arrays_indirect(ctx: &mut GlContext, mode: u32, indirect: usize) {
    let indirect_buffer = ctx.buffer_for_target(GL_DRAW_INDIRECT_BUFFER);
    if ctx.buffers.is_mapped(indirect_buffer) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    let count = match read_u32_at(ctx, GL_DRAW_INDIRECT_BUFFER, indirect) {
        Some(v) => v as i32,
        None => return,
    };
    let instances = read_u32_at(ctx, GL_DRAW_INDIRECT_BUFFER, indirect + 4).unwrap_or(1) as i32;
    let first = read_u32_at(ctx, GL_DRAW_INDIRECT_BUFFER, indirect + 8).unwrap_or(0) as i32;
    draw_arrays_instanced(ctx, mode, first, count, instances);
}

/// `glDrawElementsIndirect(mode, type, indirect)` — the indexed indirect draw; reads `{count,
/// instanceCount, firstIndex, baseVertex, baseInstance}` from the bound indirect buffer.
pub fn draw_elements_indirect(ctx: &mut GlContext, mode: u32, index_type: u32, indirect: usize) {
    let indirect_buffer = ctx.buffer_for_target(GL_DRAW_INDIRECT_BUFFER);
    if ctx.buffers.is_mapped(indirect_buffer) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    let count = match read_u32_at(ctx, GL_DRAW_INDIRECT_BUFFER, indirect) {
        Some(v) => v as i32,
        None => return,
    };
    let instances = read_u32_at(ctx, GL_DRAW_INDIRECT_BUFFER, indirect + 4).unwrap_or(1) as i32;
    let first_index = read_u32_at(ctx, GL_DRAW_INDIRECT_BUFFER, indirect + 8).unwrap_or(0) as usize;
    let base_vertex = read_u32_at(ctx, GL_DRAW_INDIRECT_BUFFER, indirect + 12).unwrap_or(0) as i32;
    // The element offset is a byte offset; firstIndex is in index units. Convert by the index size.
    let index_size = match index_type {
        GL_UNSIGNED_BYTE => 1,
        GL_UNSIGNED_SHORT => 2,
        _ => 4,
    };
    draw_elements_instanced_base_vertex(
        ctx,
        mode,
        count,
        index_type,
        first_index * index_size,
        instances,
        base_vertex,
    );
}

#[cfg(test)]
mod client_attribute_tests {
    use super::*;

    #[test]
    fn tightly_packed_2_10_10_10_client_vertices_advance_four_bytes() {
        let words = [0x0123_4567u32, 0x89ab_cdef, 0xfeed_face, 0xcafe_babe];
        let bytes =
            unsafe { read_client_attr(words.as_ptr() as usize, 4, GL_INT_2_10_10_10_REV, 0, 2) };

        assert_eq!(bytes.len(), 8);
        assert_eq!(&bytes[..4], &words[0].to_le_bytes());
        assert_eq!(&bytes[4..], &words[1].to_le_bytes());
    }
}
