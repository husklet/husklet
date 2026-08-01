use super::*;
use crate::model::context::VertexBinding;

// ---- fixed-function state ------------------------------------------------------------------------

/// Validate the `index`/`size`/`stride` triple every `glVertexAttrib{Pointer,IPointer,Format}` shares
/// (ES 3.0 §2.8): the index must name an attribute array, `size` must be 1..=4, and `stride` must not be
/// negative. Raises `GL_INVALID_VALUE` and returns `false` when the call must record nothing — silently
/// accepting these left an application believing a 5-component or negatively-strided array was in force.
fn vertex_attrib_arguments(ctx: &mut GlContext, location: usize, size: i32, stride: i32) -> bool {
    if location >= ctx.local.attr.len() || !(1..=4).contains(&size) || stride < 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return false;
    }
    true
}

/// `glVertexAttribPointer` + implicit `glEnableVertexAttribArray` is separate.
#[allow(clippy::too_many_arguments)]
pub fn vertex_attrib_pointer(
    ctx: &mut GlContext,
    location: usize,
    size: i32,
    kind: u32,
    normalized: bool,
    stride: i32,
    offset: usize,
) {
    vertex_attrib_array(ctx, location, size, kind, normalized, false, stride, offset);
}

/// `glVertexAttribIPointer` — an array that delivers INTEGERS to the shader (ES 3.0 §2.8), never
/// normalized. The distinction from [`vertex_attrib_pointer`] is the whole point of the entry point: GL
/// converts an integer component type to float for `glVertexAttribPointer` and does not for this one, so
/// a `uvec2` input must be fed a `Uint32x2` vertex format rather than the `Float32x2` the conversion path
/// produces.
pub fn vertex_attrib_ipointer(
    ctx: &mut GlContext,
    location: usize,
    size: i32,
    kind: u32,
    stride: i32,
    offset: usize,
) {
    vertex_attrib_array(ctx, location, size, kind, false, true, stride, offset);
}

/// The single writer of an array attribute's descriptor, so `integer` has ONE source of truth rather
/// than one per entry point. It was two before, and the classic pointer path hard-coded `false` for both
/// of its callers — which discarded `glVertexAttribIPointer`'s integer-ness at the door and made every
/// integer-attribute pipeline unbuildable.
#[allow(clippy::too_many_arguments)]
fn vertex_attrib_array(
    ctx: &mut GlContext,
    location: usize,
    size: i32,
    kind: u32,
    normalized: bool,
    integer: bool,
    stride: i32,
    offset: usize,
) {
    if vertex_attrib_arguments(ctx, location, size, stride) {
        let a = &mut ctx.local.attr[location];
        a.size = size;
        a.kind = kind;
        a.normalized = normalized;
        a.integer = integer;
        a.stride = stride;
        a.offset = offset;
        a.buffer = ctx.local.array_buffer;
        a.binding = None;
    }
}

/// Describe one separate-format attribute. Its buffer, base offset, stride, and divisor come from the
/// binding selected by [`vertex_attrib_binding`] when the draw is snapshotted.
#[allow(clippy::too_many_arguments)]
pub fn vertex_attrib_format(
    ctx: &mut GlContext,
    location: usize,
    size: i32,
    kind: u32,
    normalized: bool,
    integer: bool,
    relative_offset: usize,
) {
    // `glVertexAttribFormat` carries no stride of its own (the binding supplies it), so 0 always passes.
    if vertex_attrib_arguments(ctx, location, size, 0) {
        let attr = &mut ctx.local.attr[location];
        attr.size = size;
        attr.kind = kind;
        attr.normalized = normalized;
        attr.integer = integer;
        attr.offset = relative_offset;
        attr.binding.get_or_insert(location as u32);
    }
}

pub fn vertex_attrib_binding(ctx: &mut GlContext, location: usize, binding: u32) {
    if location < ctx.local.attr.len() && (binding as usize) < ctx.local.vertex_bindings.len() {
        ctx.local.attr[location].binding = Some(binding);
    } else {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
}

pub fn bind_vertex_buffer(
    ctx: &mut GlContext,
    binding: usize,
    buffer: u32,
    offset: isize,
    stride: i32,
) {
    if binding >= ctx.local.vertex_bindings.len() || offset < 0 || stride < 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if ctx.buffer_is_deleted(buffer) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    ctx.buffers.ensure(buffer);
    ctx.local.vertex_bindings[binding] = VertexBinding {
        buffer,
        offset: offset as usize,
        stride,
        divisor: ctx.local.vertex_bindings[binding].divisor,
    };
}

pub fn vertex_binding_divisor(ctx: &mut GlContext, binding: usize, divisor: u32) {
    if let Some(slot) = ctx.local.vertex_bindings.get_mut(binding) {
        slot.divisor = divisor;
    } else {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
}

pub fn vertex_attrib(ctx: &mut GlContext, location: usize, value: [f32; 4]) {
    if let Some(current) = ctx.local.current_attr.get_mut(location) {
        *current = value;
        ctx.local.current_attr_kind[location] = 0;
    } else {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
}

pub fn vertex_attrib_i(ctx: &mut GlContext, location: usize, value: [i32; 4]) {
    vertex_attrib(
        ctx,
        location,
        value.map(|component| f32::from_bits(component as u32)),
    );
    if location < ctx.local.current_attr_kind.len() {
        ctx.local.current_attr_kind[location] = 1;
    }
}

pub fn vertex_attrib_ui(ctx: &mut GlContext, location: usize, value: [u32; 4]) {
    vertex_attrib(ctx, location, value.map(f32::from_bits));
    if location < ctx.local.current_attr_kind.len() {
        ctx.local.current_attr_kind[location] = 2;
    }
}

/// `glVertexAttribDivisor(index, divisor)` — set the instance-step divisor for attribute `index`
/// (`0` = per-vertex, `>0` = per-instance). Recorded per attribute; the frame builder marks a
/// vertex-buffer slot instance-stepped when its attributes carry a non-zero divisor.
pub fn vertex_attrib_divisor(ctx: &mut GlContext, index: usize, divisor: u32) {
    if index < ctx.local.attr.len() {
        ctx.local.attr[index].divisor = divisor;
    }
}

/// `glEnableVertexAttribArray(location)`.
impl GlContext {
    pub fn enable_vertex_attrib(&mut self, location: usize) {
        match self.local.attr.get_mut(location) {
            Some(attr) => attr.enabled = true,
            // ES 3.0 §2.8: an index at or past GL_MAX_VERTEX_ATTRIBS is GL_INVALID_VALUE.
            None => self.set_gl_error(GL_INVALID_VALUE),
        }
    }

    /// `glDisableVertexAttribArray(location)`.
    pub fn disable_vertex_attrib(&mut self, location: usize) {
        match self.local.attr.get_mut(location) {
            Some(attr) => attr.enabled = false,
            None => self.set_gl_error(GL_INVALID_VALUE),
        }
    }

    /// `glClearColor(r, g, b, a)`.
    pub fn set_clear_color(&mut self, rgba: [f32; 4]) {
        self.local.pipeline.clear_color = rgba;
    }

    /// `glClearDepthf(d)` — recorded for completeness (no depth attachment is modeled, so it is not lowered).
    pub fn set_clear_depth(&mut self, d: f32) {
        self.local.pipeline.clear_depth = d;
    }
}

/// `glBlendFunc(src, dst)` — set the same factor pair for RGB and alpha.
pub fn blend_func(ctx: &mut GlContext, src: u32, dst: u32) {
    ctx.local.pipeline.blend_src_rgb = src;
    ctx.local.pipeline.blend_dst_rgb = dst;
    ctx.local.pipeline.blend_src_alpha = src;
    ctx.local.pipeline.blend_dst_alpha = dst;
}

/// `glBlendFuncSeparate(srcRGB, dstRGB, srcAlpha, dstAlpha)`.
pub fn blend_func_separate(
    ctx: &mut GlContext,
    src_rgb: u32,
    dst_rgb: u32,
    src_a: u32,
    dst_a: u32,
) {
    ctx.local.pipeline.blend_src_rgb = src_rgb;
    ctx.local.pipeline.blend_dst_rgb = dst_rgb;
    ctx.local.pipeline.blend_src_alpha = src_a;
    ctx.local.pipeline.blend_dst_alpha = dst_a;
}

/// `glBlendColor` — set the constant color used by the CONSTANT blend factors. GLES clamps each channel.
impl GlContext {
    pub fn set_blend_color(&mut self, color: [f32; 4]) {
        self.local.pipeline.blend_color = color.map(|value| value.clamp(0.0, 1.0));
    }

    /// `glDepthFunc(func)` — set the depth-compare function.
    pub fn set_depth_func(&mut self, func: u32) {
        self.local.pipeline.depth_func = func;
    }

    /// `glDepthRangef(n, f)` — the window-depth range the viewport transform maps device depth onto.
    ///
    /// This was discarded at the ABI boundary and the viewport was lowered with a hard-coded `[0, 1]`, so
    /// every fragment landed at its default-range depth however the application set the range. Both
    /// components clamp to `[0, 1]` (ES 3.0 §2.12.1); `n > f` is legal and reverses the mapping, which the
    /// host accepts — it validates each component's range, not their order.
    pub fn set_depth_range(&mut self, near: f32, far: f32) {
        self.local.pipeline.depth_range = [near.clamp(0.0, 1.0), far.clamp(0.0, 1.0)];
    }

    /// `glDepthMask(flag)` — enable/disable depth writes.
    pub fn set_depth_mask(&mut self, write: bool) {
        self.local.pipeline.depth_write = write;
    }

    /// `glClearStencil(s)` — set the stencil-buffer clear value, lowered to `DepthAttachment.clear_stencil`.
    pub fn set_clear_stencil(&mut self, s: i32) {
        self.local.pipeline.clear_stencil = s;
    }
}

/// `glStencilFunc(func, ref, mask)` — set the compare func + reference + value read mask for BOTH faces.
pub fn stencil_func(ctx: &mut GlContext, func: u32, reference: i32, mask: u32) {
    ctx.local.pipeline.stencil_func_front = func;
    ctx.local.pipeline.stencil_func_back = func;
    ctx.local.pipeline.stencil_ref = reference;
    ctx.local.pipeline.stencil_read_mask = mask;
}

/// `glStencilFuncSeparate(face, func, ref, mask)` — set the compare func + reference + value read mask for
/// the selected face(s) (`GL_FRONT` / `GL_BACK` / `GL_FRONT_AND_BACK`). The reference/read-mask are single
/// per-pass values on the wire, so setting them for either face updates the lowered value.
pub fn stencil_func_separate(ctx: &mut GlContext, face: u32, func: u32, reference: i32, mask: u32) {
    if face == GL_FRONT || face == GL_FRONT_AND_BACK {
        ctx.local.pipeline.stencil_func_front = func;
    }
    if face == GL_BACK || face == GL_FRONT_AND_BACK {
        ctx.local.pipeline.stencil_func_back = func;
    }
    ctx.local.pipeline.stencil_ref = reference;
    ctx.local.pipeline.stencil_read_mask = mask;
}

/// `glStencilOp(sfail, dpfail, dppass)` — set the stencil-fail / depth-fail / depth-pass ops for BOTH faces.
pub fn stencil_op(ctx: &mut GlContext, sfail: u32, dpfail: u32, dppass: u32) {
    ctx.local.pipeline.stencil_fail_front = sfail;
    ctx.local.pipeline.stencil_zfail_front = dpfail;
    ctx.local.pipeline.stencil_zpass_front = dppass;
    ctx.local.pipeline.stencil_fail_back = sfail;
    ctx.local.pipeline.stencil_zfail_back = dpfail;
    ctx.local.pipeline.stencil_zpass_back = dppass;
}

/// `glStencilOpSeparate(face, sfail, dpfail, dppass)` — set the three stencil ops for the selected face(s).
pub fn stencil_op_separate(ctx: &mut GlContext, face: u32, sfail: u32, dpfail: u32, dppass: u32) {
    if face == GL_FRONT || face == GL_FRONT_AND_BACK {
        ctx.local.pipeline.stencil_fail_front = sfail;
        ctx.local.pipeline.stencil_zfail_front = dpfail;
        ctx.local.pipeline.stencil_zpass_front = dppass;
    }
    if face == GL_BACK || face == GL_FRONT_AND_BACK {
        ctx.local.pipeline.stencil_fail_back = sfail;
        ctx.local.pipeline.stencil_zfail_back = dpfail;
        ctx.local.pipeline.stencil_zpass_back = dppass;
    }
}

/// `glStencilMask(mask)` — set the stencil write mask for BOTH faces.
impl GlContext {
    pub fn set_stencil_mask(&mut self, mask: u32) {
        self.local.pipeline.stencil_write_mask = mask;
    }
}

/// `glStencilMaskSeparate(face, mask)` — set the stencil write mask for the selected face(s). The wire
/// carries a single write mask for both faces, so setting either face updates the lowered value.
pub fn stencil_mask_separate(ctx: &mut GlContext, _face: u32, mask: u32) {
    ctx.local.pipeline.stencil_write_mask = mask;
}

/// `glCullFace(mode)` — select the culled face (`GL_FRONT` / `GL_BACK` / `GL_FRONT_AND_BACK`).
impl GlContext {
    pub fn set_cull_face(&mut self, mode: u32) {
        self.local.pipeline.cull_face = mode;
    }

    /// `glFrontFace(mode)` — select the front-face winding (`GL_CW` / `GL_CCW`).
    pub fn set_front_face(&mut self, mode: u32) {
        self.local.pipeline.front_face = mode;
    }
}

/// `glColorMask(r, g, b, a)` — set the per-channel framebuffer write mask. Packs the four booleans into
/// the low 4 bits (`R<<0 | G<<1 | B<<2 | A<<3`), the exact `ColorTargetState::write_mask` encoding, so a
/// masked channel is dropped by the lowered pipeline instead of being silently written anyway.
pub fn color_mask(ctx: &mut GlContext, r: bool, g: bool, b: bool, a: bool) {
    ctx.local.pipeline.color_mask =
        (r as u32) | ((g as u32) << 1) | ((b as u32) << 2) | ((a as u32) << 3);
}

/// `glViewport(x, y, w, h)` — a negative width or height is `GL_INVALID_VALUE` and leaves the viewport
/// unchanged (ES 3.0 §2.12.1). Accepting one silently let an application draw through a viewport it
/// believed it had rejected.
impl GlContext {
    pub fn set_viewport(&mut self, vp: [i32; 4]) {
        if vp[2] < 0 || vp[3] < 0 {
            self.set_gl_error(GL_INVALID_VALUE);
            return;
        }
        self.local.pipeline.viewport = vp;
    }
}

/// `glPixelStorei(pname, value)` — record a pack/unpack pixel-store parameter (affecting texture upload /
/// readback packing). Alignments accept only `{1,2,4,8}`; row-length/skip parameters must be non-negative.
/// An out-of-range value raises `GL_INVALID_VALUE` (first-error-wins) and leaves the parameter unchanged;
/// an unrecognized `pname` is ignored (the long tail of pack/unpack params this model does not track).
pub fn pixel_store(ctx: &mut GlContext, pname: u32, value: i32) {
    let ps = &mut ctx.local.pixel_store;
    let ok = match pname {
        GL_UNPACK_ALIGNMENT if matches!(value, 1 | 2 | 4 | 8) => {
            ps.unpack_alignment = value;
            true
        }
        GL_PACK_ALIGNMENT if matches!(value, 1 | 2 | 4 | 8) => {
            ps.pack_alignment = value;
            true
        }
        GL_UNPACK_ROW_LENGTH if value >= 0 => {
            ps.unpack_row_length = value;
            true
        }
        GL_UNPACK_SKIP_ROWS if value >= 0 => {
            ps.unpack_skip_rows = value;
            true
        }
        GL_UNPACK_SKIP_PIXELS if value >= 0 => {
            ps.unpack_skip_pixels = value;
            true
        }
        GL_PACK_ROW_LENGTH if value >= 0 => {
            ps.pack_row_length = value;
            true
        }
        GL_PACK_SKIP_ROWS if value >= 0 => {
            ps.pack_skip_rows = value;
            true
        }
        GL_PACK_SKIP_PIXELS if value >= 0 => {
            ps.pack_skip_pixels = value;
            true
        }
        // A recognized parameter with an out-of-range value is GL_INVALID_VALUE.
        GL_UNPACK_ALIGNMENT
        | GL_PACK_ALIGNMENT
        | GL_UNPACK_ROW_LENGTH
        | GL_UNPACK_SKIP_ROWS
        | GL_UNPACK_SKIP_PIXELS
        | GL_PACK_ROW_LENGTH
        | GL_PACK_SKIP_ROWS
        | GL_PACK_SKIP_PIXELS => {
            ctx.set_gl_error(GL_INVALID_VALUE);
            return;
        }
        // Unrecognized pname: an untracked pack/unpack parameter — leave state unchanged.
        _ => true,
    };
    let _ = ok;
}

/// `glScissor(x, y, w, h)` — a negative width or height is `GL_INVALID_VALUE` (ES 3.0 §4.1.2), like
/// [`GlContext::set_viewport`].
impl GlContext {
    pub fn set_scissor(&mut self, sc: [i32; 4]) {
        if sc[2] < 0 || sc[3] < 0 {
            self.set_gl_error(GL_INVALID_VALUE);
            return;
        }
        self.local.pipeline.scissor = sc;
    }

    /// `glEnable(cap)`.
    pub fn enable(&mut self, cap: u32) {
        set_cap(self, cap, true);
    }

    /// `glDisable(cap)`.
    pub fn disable(&mut self, cap: u32) {
        set_cap(self, cap, false);
    }
}

pub(super) fn set_cap(ctx: &mut GlContext, cap: u32, on: bool) {
    match cap {
        GL_BLEND => ctx.local.pipeline.blend = on,
        GL_DEPTH_TEST => ctx.local.pipeline.depth = on,
        GL_STENCIL_TEST => ctx.local.pipeline.stencil = on,
        GL_SCISSOR_TEST => ctx.local.pipeline.scissor_enabled = on,
        GL_CULL_FACE => ctx.local.pipeline.cull_enabled = on,
        _ => {}
    }
}

// ---- clear-buffer (glClearBuffer*) ---------------------------------------------------------------

/// `glClearBuffer*(GL_COLOR, drawbuffer, value)` — clear color attachment `drawbuffer` to `rgba`.
///
/// Recorded as a full-surface clear SCOPED to that attachment, so a multiple-render-target frame clears
/// the attachment it was given rather than whichever one happens to be first. Unlike `glClear` this does
/// not consult — and must not disturb — `GL_COLOR_CLEAR_VALUE` (ES 3.0 §4.2.3): the value travels with the
/// recorded clear.
impl GlContext {
    pub fn clear_buffer_color(&mut self, drawbuffer: u32, rgba: [f32; 4]) {
        let (w, h) = self.target_wh();
        let mut d = DrawCall {
            is_clear: true,
            clear_mask: GL_COLOR_BUFFER_BIT,
            clear_draw_buffer: Some(drawbuffer),
            ..self.snapshot(false)
        };
        d.clear = rgba;
        d.clear_rect = [0, 0, w, h];
        // A clear that writes no channel at all is a no-op, exactly as for `glClear`; a channel-masked one
        // writes its enabled channels through the rect draw (see `record_clear_buffers`).
        if !d.clears_color() && !d.color_clear_is_partial() {
            return;
        }
        self.record_draw(d);
    }

    // ---- blend equation (glBlendEquation*) -----------------------------------------------------------

    /// `glBlendEquation(mode)` — set the same blend equation for RGB and alpha.
    pub fn set_blend_equation(&mut self, mode: u32) {
        self.local.pipeline.blend_eq_rgb = mode;
        self.local.pipeline.blend_eq_alpha = mode;
    }
}

pub const CLEAR_BUFFER_COLOR: fn(&mut GlContext, u32, [f32; 4]) = GlContext::clear_buffer_color;
pub const BLEND_EQUATION: fn(&mut GlContext, u32) = GlContext::set_blend_equation;
pub use BLEND_EQUATION as blend_equation;
pub use CLEAR_BUFFER_COLOR as clear_buffer_color;

/// `glBlendEquationSeparate(modeRGB, modeAlpha)`.
pub fn blend_equation_separate(ctx: &mut GlContext, rgb: u32, alpha: u32) {
    ctx.local.pipeline.blend_eq_rgb = rgb;
    ctx.local.pipeline.blend_eq_alpha = alpha;
}
