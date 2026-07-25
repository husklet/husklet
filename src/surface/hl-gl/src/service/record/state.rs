use super::*;

// ---- fixed-function state ------------------------------------------------------------------------

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
    if location < ctx.attr.len() {
        let a = &mut ctx.attr[location];
        a.size = size;
        a.kind = kind;
        a.normalized = normalized;
        a.integer = false;
        a.stride = stride;
        a.offset = offset;
        a.buffer = ctx.array_buffer;
    }
}

/// `glVertexAttribDivisor(index, divisor)` — set the instance-step divisor for attribute `index`
/// (`0` = per-vertex, `>0` = per-instance). Recorded per attribute; the frame builder marks a
/// vertex-buffer slot instance-stepped when its attributes carry a non-zero divisor.
pub fn vertex_attrib_divisor(ctx: &mut GlContext, index: usize, divisor: u32) {
    if index < ctx.attr.len() {
        ctx.attr[index].divisor = divisor;
    }
}

/// `glEnableVertexAttribArray(location)`.
impl GlContext {
    pub fn enable_vertex_attrib(&mut self, location: usize) {
        if location < self.attr.len() {
            self.attr[location].enabled = true;
        }
    }

    /// `glDisableVertexAttribArray(location)`.
    pub fn disable_vertex_attrib(&mut self, location: usize) {
        if location < self.attr.len() {
            self.attr[location].enabled = false;
        }
    }

    /// `glClearColor(r, g, b, a)`.
    pub fn set_clear_color(&mut self, rgba: [f32; 4]) {
        self.clear_color = rgba;
    }

    /// `glClearDepthf(d)` — recorded for completeness (no depth attachment is modeled, so it is not lowered).
    pub fn set_clear_depth(&mut self, d: f32) {
        self.clear_depth = d;
    }
}

/// `glBlendFunc(src, dst)` — set the same factor pair for RGB and alpha.
pub fn blend_func(ctx: &mut GlContext, src: u32, dst: u32) {
    ctx.blend_src_rgb = src;
    ctx.blend_dst_rgb = dst;
    ctx.blend_src_alpha = src;
    ctx.blend_dst_alpha = dst;
}

/// `glBlendFuncSeparate(srcRGB, dstRGB, srcAlpha, dstAlpha)`.
pub fn blend_func_separate(
    ctx: &mut GlContext,
    src_rgb: u32,
    dst_rgb: u32,
    src_a: u32,
    dst_a: u32,
) {
    ctx.blend_src_rgb = src_rgb;
    ctx.blend_dst_rgb = dst_rgb;
    ctx.blend_src_alpha = src_a;
    ctx.blend_dst_alpha = dst_a;
}

/// `glBlendColor` — set the constant color used by the CONSTANT blend factors. GLES clamps each channel.
impl GlContext {
    pub fn set_blend_color(&mut self, color: [f32; 4]) {
        self.blend_color = color.map(|value| value.clamp(0.0, 1.0));
    }

    /// `glDepthFunc(func)` — set the depth-compare function.
    pub fn set_depth_func(&mut self, func: u32) {
        self.depth_func = func;
    }

    /// `glDepthMask(flag)` — enable/disable depth writes.
    pub fn set_depth_mask(&mut self, write: bool) {
        self.depth_write = write;
    }

    /// `glClearStencil(s)` — set the stencil-buffer clear value, lowered to `DepthAttachment.clear_stencil`.
    pub fn set_clear_stencil(&mut self, s: i32) {
        self.clear_stencil = s;
    }
}

/// `glStencilFunc(func, ref, mask)` — set the compare func + reference + value read mask for BOTH faces.
pub fn stencil_func(ctx: &mut GlContext, func: u32, reference: i32, mask: u32) {
    ctx.stencil_func_front = func;
    ctx.stencil_func_back = func;
    ctx.stencil_ref = reference;
    ctx.stencil_read_mask = mask;
}

/// `glStencilFuncSeparate(face, func, ref, mask)` — set the compare func + reference + value read mask for
/// the selected face(s) (`GL_FRONT` / `GL_BACK` / `GL_FRONT_AND_BACK`). The reference/read-mask are single
/// per-pass values on the wire, so setting them for either face updates the lowered value.
pub fn stencil_func_separate(ctx: &mut GlContext, face: u32, func: u32, reference: i32, mask: u32) {
    if face == GL_FRONT || face == GL_FRONT_AND_BACK {
        ctx.stencil_func_front = func;
    }
    if face == GL_BACK || face == GL_FRONT_AND_BACK {
        ctx.stencil_func_back = func;
    }
    ctx.stencil_ref = reference;
    ctx.stencil_read_mask = mask;
}

/// `glStencilOp(sfail, dpfail, dppass)` — set the stencil-fail / depth-fail / depth-pass ops for BOTH faces.
pub fn stencil_op(ctx: &mut GlContext, sfail: u32, dpfail: u32, dppass: u32) {
    ctx.stencil_fail_front = sfail;
    ctx.stencil_zfail_front = dpfail;
    ctx.stencil_zpass_front = dppass;
    ctx.stencil_fail_back = sfail;
    ctx.stencil_zfail_back = dpfail;
    ctx.stencil_zpass_back = dppass;
}

/// `glStencilOpSeparate(face, sfail, dpfail, dppass)` — set the three stencil ops for the selected face(s).
pub fn stencil_op_separate(ctx: &mut GlContext, face: u32, sfail: u32, dpfail: u32, dppass: u32) {
    if face == GL_FRONT || face == GL_FRONT_AND_BACK {
        ctx.stencil_fail_front = sfail;
        ctx.stencil_zfail_front = dpfail;
        ctx.stencil_zpass_front = dppass;
    }
    if face == GL_BACK || face == GL_FRONT_AND_BACK {
        ctx.stencil_fail_back = sfail;
        ctx.stencil_zfail_back = dpfail;
        ctx.stencil_zpass_back = dppass;
    }
}

/// `glStencilMask(mask)` — set the stencil write mask for BOTH faces.
impl GlContext {
    pub fn set_stencil_mask(&mut self, mask: u32) {
        self.stencil_write_mask = mask;
    }
}

/// `glStencilMaskSeparate(face, mask)` — set the stencil write mask for the selected face(s). The wire
/// carries a single write mask for both faces, so setting either face updates the lowered value.
pub fn stencil_mask_separate(ctx: &mut GlContext, _face: u32, mask: u32) {
    ctx.stencil_write_mask = mask;
}

/// `glCullFace(mode)` — select the culled face (`GL_FRONT` / `GL_BACK` / `GL_FRONT_AND_BACK`).
impl GlContext {
    pub fn set_cull_face(&mut self, mode: u32) {
        self.cull_face = mode;
    }

    /// `glFrontFace(mode)` — select the front-face winding (`GL_CW` / `GL_CCW`).
    pub fn set_front_face(&mut self, mode: u32) {
        self.front_face = mode;
    }
}

/// `glColorMask(r, g, b, a)` — set the per-channel framebuffer write mask. Packs the four booleans into
/// the low 4 bits (`R<<0 | G<<1 | B<<2 | A<<3`), the exact `ColorTargetState::write_mask` encoding, so a
/// masked channel is dropped by the lowered pipeline instead of being silently written anyway.
pub fn color_mask(ctx: &mut GlContext, r: bool, g: bool, b: bool, a: bool) {
    ctx.color_mask = (r as u32) | ((g as u32) << 1) | ((b as u32) << 2) | ((a as u32) << 3);
}

/// `glViewport(x, y, w, h)`.
impl GlContext {
    pub fn set_viewport(&mut self, vp: [i32; 4]) {
        self.viewport = vp;
    }
}

/// `glPixelStorei(pname, value)` — record a pack/unpack pixel-store parameter (affecting texture upload /
/// readback packing). Alignments accept only `{1,2,4,8}`; row-length/skip parameters must be non-negative.
/// An out-of-range value raises `GL_INVALID_VALUE` (first-error-wins) and leaves the parameter unchanged;
/// an unrecognized `pname` is ignored (the long tail of pack/unpack params this model does not track).
pub fn pixel_store(ctx: &mut GlContext, pname: u32, value: i32) {
    let ps = &mut ctx.pixel_store;
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

/// `glScissor(x, y, w, h)`.
impl GlContext {
    pub fn set_scissor(&mut self, sc: [i32; 4]) {
        self.scissor = sc;
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
        GL_BLEND => ctx.blend = on,
        GL_DEPTH_TEST => ctx.depth = on,
        GL_STENCIL_TEST => ctx.stencil = on,
        GL_SCISSOR_TEST => ctx.scissor_enabled = on,
        GL_CULL_FACE => ctx.cull_enabled = on,
        _ => {}
    }
}

// ---- clear-buffer (glClearBuffer*) ---------------------------------------------------------------

/// `glClearBufferfv(GL_COLOR, drawbuffer, value)` — clear the current render target to `rgba`. Recorded as
/// a full-surface clear at the given color (the same deferred clear `glClear` records), so the frame
/// builder lowers a pass clear-load with this color.
impl GlContext {
    pub fn clear_buffer_color(&mut self, rgba: [f32; 4]) {
        self.clear_color = rgba;
        self.record_clear();
    }

    // ---- blend equation (glBlendEquation*) -----------------------------------------------------------

    /// `glBlendEquation(mode)` — set the same blend equation for RGB and alpha.
    pub fn set_blend_equation(&mut self, mode: u32) {
        self.blend_eq_rgb = mode;
        self.blend_eq_alpha = mode;
    }
}

pub const CLEAR_BUFFER_COLOR: fn(&mut GlContext, [f32; 4]) = GlContext::clear_buffer_color;
pub const BLEND_EQUATION: fn(&mut GlContext, u32) = GlContext::set_blend_equation;
pub use BLEND_EQUATION as blend_equation;
pub use CLEAR_BUFFER_COLOR as clear_buffer_color;

/// `glBlendEquationSeparate(modeRGB, modeAlpha)`.
pub fn blend_equation_separate(ctx: &mut GlContext, rgb: u32, alpha: u32) {
    ctx.blend_eq_rgb = rgb;
    ctx.blend_eq_alpha = alpha;
}
