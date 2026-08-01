use super::*;

// ---- framebuffers (offscreen render targets) -----------------------------------------------------

/// `glBindFramebuffer(target, name)` — bind `name` as the draw and/or read framebuffer (`0` = default).
/// `GL_FRAMEBUFFER` binds both; the split `GL_DRAW_FRAMEBUFFER`/`GL_READ_FRAMEBUFFER` bind one. A recorded
/// draw's render target follows the draw binding; the read binding is the `glReadPixels`/blit source.
pub fn bind_framebuffer(ctx: &mut GlContext, target: u32, name: u32) {
    ctx.local.framebuffers.ensure(name);
    match target {
        GL_FRAMEBUFFER => {
            ctx.local.bound_fbo = name;
            ctx.local.read_fbo = name;
        }
        GL_DRAW_FRAMEBUFFER => ctx.local.bound_fbo = name,
        GL_READ_FRAMEBUFFER => ctx.local.read_fbo = name,
        _ => ctx.set_gl_error(GL_INVALID_ENUM),
    }
}

/// `glFramebufferTexture2D(target, attachment, textarget, tex, level)` — attach `tex` as the bound FBO's
/// color target. Only `GL_COLOR_ATTACHMENT0` of a `GL_TEXTURE_2D` at level `0` is modeled; the default
/// framebuffer `0` has no attachable slot. Honest GL errors: bad `target` → `GL_INVALID_ENUM`; an
/// unmodeled attachment/textarget/level → `GL_INVALID_VALUE`; attaching to the default framebuffer or an
/// unknown texture → `GL_INVALID_OPERATION` (all first-error-wins).
pub fn framebuffer_texture_2d(
    ctx: &mut GlContext,
    target: u32,
    attachment: u32,
    textarget: u32,
    tex: u32,
    level: i32,
) {
    if !matches!(
        target,
        GL_FRAMEBUFFER | GL_DRAW_FRAMEBUFFER | GL_READ_FRAMEBUFFER
    ) {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    let fbo = if target == GL_READ_FRAMEBUFFER {
        ctx.local.read_fbo
    } else {
        ctx.local.bound_fbo
    };
    // GL_COLOR_ATTACHMENT0..15 are all attachable (MRT); a non-color attachment / textarget / level is
    // unmodeled. The attachment index is `attachment - GL_COLOR_ATTACHMENT0`.
    let is_color = (GL_COLOR_ATTACHMENT0..=GL_COLOR_ATTACHMENT0 + 15).contains(&attachment);
    let is_depth_stencil = matches!(
        attachment,
        GL_DEPTH_ATTACHMENT | GL_STENCIL_ATTACHMENT | GL_DEPTH_STENCIL_ATTACHMENT
    );
    if (!is_color && !is_depth_stencil) || textarget != GL_TEXTURE_2D || level != 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if fbo == 0 {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if tex != 0 && ctx.textures.get(tex).is_none() {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    // A depth/stencil TEXTURE attachment records only its presence, exactly as the renderbuffer path does
    // (see [`framebuffer_renderbuffer`]): the plane itself is minted by the frame builder.
    match attachment {
        GL_DEPTH_ATTACHMENT => ctx.local.framebuffers.attach_depth(fbo, tex),
        GL_STENCIL_ATTACHMENT => ctx.local.framebuffers.attach_stencil(fbo, tex),
        GL_DEPTH_STENCIL_ATTACHMENT => {
            ctx.local.framebuffers.attach_depth(fbo, tex);
            ctx.local.framebuffers.attach_stencil(fbo, tex);
        }
        _ => ctx
            .local
            .framebuffers
            .attach_color_index(fbo, attachment - GL_COLOR_ATTACHMENT0, tex),
    }
}

/// `glDeleteFramebuffers` (one name). Deleting the bound draw/read FBO reverts that binding to the default.
impl GlContext {
    pub fn delete_framebuffer(&mut self, name: u32) -> bool {
        if self.local.bound_fbo == name {
            self.local.bound_fbo = 0;
        }
        if self.local.read_fbo == name {
            self.local.read_fbo = 0;
        }
        self.local.framebuffers.delete(name)
    }

    /// `glIsFramebuffer(name)` — true once `name` names a generated (non-default) framebuffer object.
    pub fn has_framebuffer(&self, name: u32) -> bool {
        self.local.framebuffers.exists(name)
    }

    /// `glCheckFramebufferStatus(target)` — completeness of the bound draw/read framebuffer. Returns
    /// `GL_FRAMEBUFFER_COMPLETE` for the default framebuffer or a user FBO with a sized color attachment,
    /// `GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT` for a user FBO with no color attachment, and
    /// `GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT` when the color attachment is unsized (e.g. a renderbuffer with
    /// no `glRenderbufferStorage` yet). A bad `target` raises `GL_INVALID_ENUM` and returns `0`.
    pub fn check_framebuffer_status(&mut self, target: u32) -> u32 {
        let fbo = match target {
            GL_FRAMEBUFFER | GL_DRAW_FRAMEBUFFER => self.local.bound_fbo,
            GL_READ_FRAMEBUFFER => self.local.read_fbo,
            _ => {
                self.set_gl_error(GL_INVALID_ENUM);
                return 0;
            }
        };
        self.framebuffer_status(fbo)
    }

    /// Completeness for the color-only framebuffer subset this model renders. The default framebuffer (`0`)
    /// is managed by EGL and is complete; a user FBO needs one sized, live color-texture attachment.
    pub(super) fn framebuffer_status(&self, fbo: u32) -> u32 {
        if fbo == 0 {
            return if self.local.surf.have {
                GL_FRAMEBUFFER_COMPLETE
            } else {
                GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT
            };
        }
        if !self.local.framebuffers.exists(fbo) {
            return GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT;
        }
        let color = self.local.framebuffers.color_attachment(fbo);
        if color == 0 {
            return GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT;
        }
        match self.textures.get(color) {
            // ES 3.0 §4.4.4: a colour attachment must have a COLOUR-RENDERABLE internal format. Reporting
            // COMPLETE for a depth, snorm, shared-exponent, SRGB8 or three-component integer texture bound
            // to `GL_COLOR_ATTACHMENT0` hands the application a framebuffer that cannot work and gives it
            // no way to find out — the check it performs precisely to avoid that says yes.
            Some(t) if t.w > 0 && t.h > 0 && !colour_renderable(t.internal_format) => {
                // At error level, because this refusal is the whole of a caller's failure and it is the one
                // fact the caller cannot recover: the status names no format, so an application told
                // INCOMPLETE_ATTACHMENT has no way to learn which attachment or which format was refused.
                // A browser hit this thousands of times per minute and no reading of its own log could
                // identify the format, because the format is only known here.
                hl_log::hl_error!(
                    hl_log::tag::GL,
                    "framebuffer {fbo} incomplete: colour attachment texture {color} declares format \
                     {:#06x}, which is not colour-renderable here",
                    t.internal_format
                );
                GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT
            }
            Some(t) if t.w > 0 && t.h > 0 => GL_FRAMEBUFFER_COMPLETE,
            other => {
                match other {
                    Some(t) => hl_log::hl_error!(
                        hl_log::tag::GL,
                        "framebuffer {fbo} incomplete: colour attachment texture {color} has no storage \
                         ({}x{}), so nothing was allocated for it to render into",
                        t.w,
                        t.h
                    ),
                    None => hl_log::hl_error!(
                        hl_log::tag::GL,
                        "framebuffer {fbo} incomplete: colour attachment names texture {color}, which this \
                         context does not have"
                    ),
                }
                GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT
            }
        }
    }
}

pub const DELETE_FRAMEBUFFER: fn(&mut GlContext, u32) -> bool = GlContext::delete_framebuffer;
pub const IS_FRAMEBUFFER: fn(&GlContext, u32) -> bool = GlContext::has_framebuffer;
pub const CHECK_FRAMEBUFFER_STATUS: fn(&mut GlContext, u32) -> u32 =
    GlContext::check_framebuffer_status;
pub use CHECK_FRAMEBUFFER_STATUS as check_framebuffer_status;
pub use DELETE_FRAMEBUFFER as delete_framebuffer;
pub use IS_FRAMEBUFFER as is_framebuffer;

// ---- renderbuffers (modeled as texture-backed color attachments) ---------------------------------

/// `glGenRenderbuffers` (one name). Eagerly mints the RBO's stable backing texture (still unsized, so it
/// reads back incomplete until `glRenderbufferStorage`), so a `glFramebufferRenderbuffer` that runs before
/// the storage call still resolves to the right attachment once storage lands.
impl GlContext {
    pub fn gen_renderbuffer(&mut self) -> u32 {
        let name = self.renderbuffers.gen();
        let tex = self.textures.gen();
        self.renderbuffers.set_storage(name, tex, 0, 0);
        name
    }
}

/// `glBindRenderbuffer(GL_RENDERBUFFER, name)` — select the target of the next `glRenderbufferStorage`.
pub fn bind_renderbuffer(ctx: &mut GlContext, target: u32, name: u32) {
    if target != GL_RENDERBUFFER {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    ctx.renderbuffers.ensure(name);
    ctx.local.bound_rbo = name;
}

/// `glRenderbufferStorage(GL_RENDERBUFFER, internalformat, w, h)` — size the bound renderbuffer's backing
/// texture. The model materializes every renderbuffer as an RGBA8 color plane (its neutral render-target
/// format), so `internalformat` does not select the plane — but it IS recorded, because completeness is
/// asked about the format the application declared, not the plane the model chose.
///
/// Dropping it made this path answer COMPLETE for every format there is. The texture path has checked
/// `colour_renderable` since that rule was written and the renderbuffer path never did, which is the
/// sibling disagreement in its usual shape: two ways to reach one attachment, one of them gated. A
/// renderbuffer of `GL_RGB8_SNORM`, `GL_SRGB8`, `GL_RGB9_E5` or a three-component integer format is not
/// colour-renderable in ES 3.0 §4.4.4, and an application that asks precisely so it can fall back was
/// told yes.
///
/// Honest GL errors: bad `target` → `GL_INVALID_ENUM`; no bound renderbuffer → `GL_INVALID_OPERATION`;
/// negative extent → `GL_INVALID_VALUE`.
pub fn renderbuffer_storage(ctx: &mut GlContext, target: u32, internalformat: u32, w: i32, h: i32) {
    if target != GL_RENDERBUFFER {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    let rbo = ctx.local.bound_rbo;
    if rbo == 0 {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    // A negative extent, or one beyond the advertised GL_MAX_RENDERBUFFER_SIZE, is GL_INVALID_VALUE (real
    // GL rejects an oversized renderbuffer). This also bounds the backing-plane allocation to a sane size.
    if w < 0
        || h < 0
        || w > crate::service::query::MAX_TEXTURE_SIZE
        || h > crate::service::query::MAX_TEXTURE_SIZE
    {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    // Reuse the RBO's stable backing texture (minted at gen) so an earlier attachment stays wired.
    let tex = match ctx.renderbuffers.backing_tex(rbo) {
        0 => ctx.textures.gen(),
        t => t,
    };
    ctx.textures
        .image_2d(tex, w, h, &[], TextureFormat::Rgba8Unorm);
    // The declared format rides on the backing texture, which is what `check_framebuffer_status` reads —
    // so one table answers the question for both attachment paths instead of two rules drifting apart.
    if let Some(texture) = ctx.textures.get_mut(tex) {
        texture.internal_format = internalformat;
    }
    ctx.renderbuffers.set_storage(rbo, tex, w, h);
}

/// `glDeleteRenderbuffers` (one name). Detaches the backing texture from every FBO color slot and drops
/// the backing texture. Returns `false` for an unknown / zero name.
impl GlContext {
    pub fn delete_renderbuffer(&mut self, name: u32) -> bool {
        if self.local.bound_rbo == name {
            self.local.bound_rbo = 0;
        }
        match self.renderbuffers.delete(name) {
            Some(rb) => {
                self.local.framebuffers.detach_color_texture(rb.tex);
                // The renderbuffer's backing texture owns the offscreen render-target IR — retire it too.
                self.retire_texture(rb.tex);
                self.textures.delete(rb.tex);
                true
            }
            None => false,
        }
    }

    /// `glIsRenderbuffer(name)` — true once `name` names a generated (non-default) renderbuffer object.
    pub fn is_renderbuffer(&self, name: u32) -> bool {
        self.renderbuffers.contains(name)
    }
}

pub const GEN_RENDERBUFFER: fn(&mut GlContext) -> u32 = GlContext::gen_renderbuffer;
pub const DELETE_RENDERBUFFER: fn(&mut GlContext, u32) -> bool = GlContext::delete_renderbuffer;
pub const IS_RENDERBUFFER: fn(&GlContext, u32) -> bool = GlContext::is_renderbuffer;
pub use DELETE_RENDERBUFFER as delete_renderbuffer;
pub use GEN_RENDERBUFFER as gen_renderbuffer;
pub use IS_RENDERBUFFER as is_renderbuffer;

/// `glFramebufferRenderbuffer(target, attachment, renderbuffertarget, rbo)` — attach a renderbuffer to the
/// bound FBO. The color attachment resolves to the renderbuffer's backing texture (reusing the exact
/// texture-attachment render path); depth/stencil attachments are accepted as an honest no-op (this model
/// has no depth/stencil buffer). Honest GL errors: bad `target`/`renderbuffertarget`/attachment →
/// `GL_INVALID_ENUM`; attaching to the default framebuffer or an unknown renderbuffer → `GL_INVALID_OPERATION`.
pub fn framebuffer_renderbuffer(
    ctx: &mut GlContext,
    target: u32,
    attachment: u32,
    rbtarget: u32,
    rbo: u32,
) {
    if !matches!(
        target,
        GL_FRAMEBUFFER | GL_DRAW_FRAMEBUFFER | GL_READ_FRAMEBUFFER
    ) || rbtarget != GL_RENDERBUFFER
    {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    let fbo = if target == GL_READ_FRAMEBUFFER {
        ctx.local.read_fbo
    } else {
        ctx.local.bound_fbo
    };
    if fbo == 0 {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if rbo != 0 && !ctx.renderbuffers.contains(rbo) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    match attachment {
        GL_COLOR_ATTACHMENT0 => {
            // The renderbuffer's texture-backed storage becomes the FBO's color target (`0` detaches) —
            // but the ATTACHED OBJECT is the renderbuffer, and the attachment queries must say so.
            let tex = ctx.renderbuffers.backing_tex(rbo);
            ctx.local.framebuffers.attach_color(fbo, tex);
            ctx.local.framebuffers.set_color_source(fbo, 0, rbo, true);
        }
        // This model has no depth/stencil PLANE — the frame builder mints one for any depth/stencil-tested
        // pass — but WHETHER the framebuffer has such an attachment is load-bearing GL state: without an
        // attachment the depth (resp. stencil) test always passes and writes nothing (ES 3.0 §4.1.5/§4.1.6).
        GL_DEPTH_ATTACHMENT => ctx.local.framebuffers.attach_depth(fbo, rbo),
        GL_STENCIL_ATTACHMENT => ctx.local.framebuffers.attach_stencil(fbo, rbo),
        GL_DEPTH_STENCIL_ATTACHMENT => {
            ctx.local.framebuffers.attach_depth(fbo, rbo);
            ctx.local.framebuffers.attach_stencil(fbo, rbo);
        }
        _ => ctx.set_gl_error(GL_INVALID_ENUM),
    }
}

/// `glBlitFramebuffer(srcX0,srcY0,srcX1,srcY1, dstX0,dstY0,dstX1,dstY1, mask, filter)` — validate the
/// read+draw framebuffers and RECORD the color blit for the frame builder.
///
/// The deferred model applies the blit AFTER the frame's render passes: its source is the read FBO's
/// rendered color attachment and its destination is the draw FBO's, both materialized as render-target
/// textures. For the equal-size (non-scaling) case the frame lowers this to `Enc::CopyTextureToTexture`
/// (the executor implements exact texture→texture copy); a SCALING blit (source extent != destination
/// extent) lowers to `Enc::BlitTexture` carrying the resampling `filter`. A non-color `mask` is a no-op, and an incomplete
/// read or draw framebuffer raises `GL_INVALID_FRAMEBUFFER_OPERATION` (first-error-wins) — a conforming
/// driver never samples an incomplete attachment.
#[allow(clippy::too_many_arguments)]
pub fn blit_framebuffer(
    ctx: &mut GlContext,
    src_x0: i32,
    src_y0: i32,
    src_x1: i32,
    src_y1: i32,
    dst_x0: i32,
    dst_y0: i32,
    dst_x1: i32,
    dst_y1: i32,
    mask: u32,
    filter: u32,
) {
    // Validation comes FIRST, whatever the mask names. Testing the colour bit before the framebuffers
    // meant a depth- or stencil-only blit skipped the completeness check entirely and reported
    // GL_NO_ERROR against an incomplete framebuffer — the mask decides what is COPIED, not whether the
    // call is legal (ES 3.0 §4.3.2).
    if ctx.framebuffer_status(ctx.local.read_fbo) != GL_FRAMEBUFFER_COMPLETE
        || ctx.framebuffer_status(ctx.local.bound_fbo) != GL_FRAMEBUFFER_COMPLETE
    {
        ctx.set_gl_error(GL_INVALID_FRAMEBUFFER_OPERATION);
        return;
    }
    // Only the COLOUR aspect is copied. A depth or stencil blit has no lowering here, and dropping it in
    // silence is worse than not supporting it: nothing is recorded, so no later diagnostic can fire for
    // it either — the copy simply never happened and the frame that needed it has no account of why.
    // Say so once per context; a compositor blits every frame.
    if mask & (GL_DEPTH_BUFFER_BIT | GL_STENCIL_BUFFER_BIT) != 0
        && !ctx.local.depth_stencil_blit_reported
    {
        ctx.local.depth_stencil_blit_reported = true;
        hl_log::hl_warn!(
            hl_log::tag::GL,
            "glBlitFramebuffer: the depth/stencil aspects of mask {mask:#x} are NOT copied (only the \
             colour aspect is lowered). Reported once per context."
        );
    }
    if mask & GL_COLOR_BUFFER_BIT == 0 {
        return;
    }
    // GL defines only GL_NEAREST and GL_LINEAR for a color blit; anything else falls back to Nearest.
    let filter = if filter == crate::model::glconst::GL_LINEAR {
        hl_gpu::protocol::model::enums::Filter::Linear
    } else {
        hl_gpu::protocol::model::enums::Filter::Nearest
    };
    let target = |ctx: &GlContext, fbo| {
        (fbo != 0).then(|| {
            let texture = ctx.local.framebuffers.color_attachment(fbo);
            ctx.textures
                .get(texture)
                .filter(|texture| texture.w > 0 && texture.h > 0)
                .map(|texture| crate::model::program::TargetSnapshot {
                    texture: ctx.local.framebuffers.color_attachment(fbo),
                    generation: texture.gen,
                    shared_storage: texture.shared_storage(),
                    shared_revision: texture
                        .shared_current_identity()
                        .map(|(_, revision)| revision),
                    width: texture.w,
                    height: texture.h,
                    format: texture.ir_format,
                })
        })?
    };
    ctx.record_blit(crate::model::context::BlitOp {
        read_fbo: ctx.local.read_fbo,
        draw_fbo: ctx.local.bound_fbo,
        read_target: target(ctx, ctx.local.read_fbo),
        draw_target: target(ctx, ctx.local.bound_fbo),
        read_ir: None,
        draw_ir: None,
        src: [src_x0, src_y0, src_x1, src_y1],
        dst: [dst_x0, dst_y0, dst_x1, dst_y1],
        filter,
    });
}

// ---- vertex array objects ------------------------------------------------------------------------

/// `glGenVertexArrays` (one name).
pub const GEN_VERTEX_ARRAY: fn(&mut GlContext) -> u32 = GlContext::gen_vertex_array;

/// `glBindVertexArray(vao)` — swap the captured attrib/element-buffer state (see
/// [`GlContext::bind_vertex_array`]).
pub const BIND_VERTEX_ARRAY: fn(&mut GlContext, u32) = GlContext::bind_vertex_array;

/// `glDeleteVertexArrays` (one name).
pub const DELETE_VERTEX_ARRAY: fn(&mut GlContext, u32) -> bool = GlContext::delete_vertex_array;

/// `glIsVertexArray(vao)`.
pub const IS_VERTEX_ARRAY: fn(&GlContext, u32) -> bool = GlContext::is_vertex_array;
pub use BIND_VERTEX_ARRAY as bind_vertex_array;
pub use DELETE_VERTEX_ARRAY as delete_vertex_array;
pub use GEN_VERTEX_ARRAY as gen_vertex_array;
pub use IS_VERTEX_ARRAY as is_vertex_array;

/// Whether a sized internal format may back a colour attachment (ES 3.0 table 3.13).
///
/// `0` means the application gave an UNSIZED format (`GL_RGB` / `GL_RGBA`), which this driver materializes
/// as RGBA8 and which is renderable — so an ordinary `glTexImage2D` attachment stays complete.
///
/// The renderable set is the unorm colour formats plus `SRGB8_ALPHA8` plus the one-, two- and
/// four-component integer formats. Deliberately EXCLUDED, each of which this driver reported complete:
/// every depth/stencil format; the signed-normalized formats; `GL_RGB9_E5` and `GL_R11F_G11F_B10F`
/// (shared-exponent / packed float); `GL_SRGB8` (unlike `GL_SRGB8_ALPHA8`); and the THREE-component
/// integer formats, which the specification omits while including their one-, two- and four-component
/// siblings.
fn colour_renderable(internal_format: u32) -> bool {
    matches!(
        internal_format,
        0 | GL_RGB
            | GL_RGBA
            | GL_R8
            | GL_RG8
            | GL_RGB8
            | GL_RGB565
            | GL_RGBA4
            | GL_RGB5_A1
            | GL_RGBA8
            | GL_RGB10_A2
            | GL_RGB10_A2UI
            | GL_SRGB8_ALPHA8
            | GL_BGRA8_EXT
            | GL_R8UI
            | GL_R8I
            | GL_R16UI
            | GL_R16I
            | GL_R32UI
            | GL_R32I
            | GL_RG8UI
            | GL_RG8I
            | GL_RG16UI
            | GL_RG16I
            | GL_RG32UI
            | GL_RG32I
            | GL_RGBA8UI
            | GL_RGBA8I
            | GL_RGBA16UI
            | GL_RGBA16I
            | GL_RGBA32UI
            | GL_RGBA32I
    )
}
