use super::*;

// ---- textures ------------------------------------------------------------------------------------

/// `glActiveTexture(GL_TEXTURE0 + i)`.
impl GlContext {
    pub fn active_texture(&mut self, texture: u32) {
        let unit = texture.wrapping_sub(GL_TEXTURE0) as usize;
        if unit < self.local.tex_unit.len() {
            self.local.active_texture = unit;
        }
    }
}

/// `glBindTexture(GL_TEXTURE_2D, name)` — binds to the active texture unit.
pub fn bind_texture(ctx: &mut GlContext, _target: u32, name: u32) {
    if ctx.texture_is_deleted(name) {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    ctx.textures.ensure(name);
    let unit = ctx.local.active_texture;
    if unit < ctx.local.tex_unit.len() {
        ctx.local.tex_unit[unit] = name;
    }
}

/// `glTexImage2D` — `pixels` is the already-RGBA8-converted image (`w*h*4`) bound to the active unit; the
/// texture lowers to the default `Rgba8Unorm` neutral format.
pub fn tex_image_2d(ctx: &mut GlContext, w: i32, h: i32, pixels: &[u8]) {
    tex_image_2d_format(ctx, w, h, pixels, TextureFormat::Rgba8Unorm);
}

/// `glTexImage2D` with an explicit neutral texel `format` selected from the GL internal format — used for
/// FBO color attachments (which are rendered into, so the format becomes the render-target + surface
/// format) and for non-RGBA8 sampled uploads (e.g. a `GL_BGRA_EXT` image → `Bgra8Unorm`).
pub fn tex_image_2d_format(
    ctx: &mut GlContext,
    w: i32,
    h: i32,
    pixels: &[u8],
    format: TextureFormat,
) {
    // A negative or over-max extent is GL_INVALID_VALUE (real GL rejects beyond GL_MAX_TEXTURE_SIZE). This
    // also bounds the zeroed-storage allocation `image_2d` does for an empty (storage-only) upload, so a
    // hostile `glTexImage2D(40000, 40000, NULL)` can never trigger a multi-GiB host allocation.
    if w < 0
        || h < 0
        || w > crate::service::query::MAX_TEXTURE_SIZE
        || h > crate::service::query::MAX_TEXTURE_SIZE
    {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    let name = ctx.local.tex_unit[ctx.local.active_texture];
    if name != 0 {
        ctx.textures.image_2d(name, w, h, pixels, format);
    }
}

/// `glTexParameteri(GL_TEXTURE_2D, pname, value)` on the active unit's texture.
pub fn tex_parameter(ctx: &mut GlContext, pname: u32, value: u32) {
    let name = ctx.local.tex_unit[ctx.local.active_texture];
    if name != 0 {
        ctx.textures.set_param(name, pname, value);
    }
}

/// Vector texture parameter setter. Only `GL_TEXTURE_SWIZZLE_RGBA` consumes four values.
pub fn tex_parameter_vector(ctx: &mut GlContext, pname: u32, values: &[u32]) {
    let name = ctx.local.tex_unit[ctx.local.active_texture];
    if name == 0 {
        return;
    }
    if pname == GL_TEXTURE_SWIZZLE_RGBA {
        if let Ok(swizzle) = <[u32; 4]>::try_from(values) {
            ctx.textures.set_swizzle(name, swizzle);
        } else {
            ctx.set_gl_error(GL_INVALID_VALUE);
        }
    } else if let Some(&value) = values.first() {
        ctx.textures.set_param(name, pname, value);
    }
}

/// `glGenerateMipmap(target)` — validate the request. This model samples only the base level (the
/// neutral-IR textures carry a single mip), so the mip chain is not materialized — an honest no-op on
/// the pixel data. `target` must be a 2D/cube texture target (else `GL_INVALID_ENUM`) with a texture
/// bound to the active unit (else `GL_INVALID_OPERATION`); the state is otherwise unchanged.
impl GlContext {
    pub fn generate_mipmap(&mut self, target: u32) {
        if target != GL_TEXTURE_2D && target != GL_TEXTURE_CUBE_MAP {
            self.set_gl_error(GL_INVALID_ENUM);
            return;
        }
        if self.local.tex_unit[self.local.active_texture] == 0 {
            self.set_gl_error(GL_INVALID_OPERATION);
        }
    }
}

/// `glTexStorage2D(target, levels, internalformat, w, h)` — immutable-format allocation of the bound
/// 2D texture. Mirrors `gl_shim.c`: allocate the RGBA8 base plane (so a later `glTexSubImage2D` has a
/// target), mark the texture immutable, and record `levels`. Honest GL errors: bad `target` →
/// `GL_INVALID_ENUM`; `levels != 1`, non-positive extent, or an unmodeled internalformat →
/// `GL_INVALID_VALUE`; no bound texture, unknown texture, or an already-immutable texture →
/// `GL_INVALID_OPERATION`.
pub fn tex_storage_2d(
    ctx: &mut GlContext,
    target: u32,
    levels: i32,
    internalformat: u32,
    w: i32,
    h: i32,
) {
    if target != GL_TEXTURE_2D {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    // glTexStorage2D requires a SIZED internal format (Chrome's tiles use GL_RGBA8); the unsized
    // GL_RGB/GL_RGBA spellings are accepted leniently. An unmodeled format is GL_INVALID_ENUM.
    let Ok(fmt) = TextureFormat::try_from(InternalFormat(internalformat)) else {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    };
    if levels != 1 || w <= 0 || h <= 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if w > crate::service::query::MAX_TEXTURE_SIZE || h > crate::service::query::MAX_TEXTURE_SIZE {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    let name = ctx.local.tex_unit[ctx.local.active_texture];
    match ctx.textures.get(name) {
        _ if name == 0 => {
            ctx.set_gl_error(GL_INVALID_OPERATION);
            return;
        }
        Some(t) if !t.immutable => {}
        _ => {
            ctx.set_gl_error(GL_INVALID_OPERATION);
            return;
        }
    }
    if !ctx.textures.storage_2d(name, w, h, levels, fmt) {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
}

/// Map a `glTexStorage2D`/`glTexStorage3D` (sized) internal format to the neutral IR [`TextureFormat`] the
/// model materializes. glTexStorage* mandates a *sized* format; this also leniently accepts the unsized
/// `GL_RGB`/`GL_RGBA` spellings the earlier shim allowed. Returns `None` for a format this driver does not
/// model (→ `GL_INVALID_ENUM`). Formats without a distinct neutral variant fall back to `Rgba8Unorm` — the
/// model stores an RGBA8 plane regardless, so the choice only steers an FBO color attachment's surface
/// format (sRGB / BGRA / float), which the executor honors.
struct InternalFormat(u32);
impl TryFrom<InternalFormat> for TextureFormat {
    type Error = ();
    fn try_from(internalformat: InternalFormat) -> Result<Self, Self::Error> {
        Ok(match internalformat.0 {
            // 8-bit unorm RGBA/RGB — sized + the lenient unsized spellings, plus the packed <=8bpc formats.
            GL_RGBA | GL_RGBA8 | GL_RGB | GL_RGB8 | GL_RGB565 | GL_RGBA4 | GL_RGB5_A1
            | GL_RGB10_A2 | GL_RGB10_A2UI | GL_R11F_G11F_B10F | GL_RGB9_E5 => {
                TextureFormat::Rgba8Unorm
            }
            GL_SRGB8_ALPHA8 | GL_SRGB8 => TextureFormat::Rgba8Srgb,
            GL_BGRA8_EXT => TextureFormat::Bgra8Unorm,
            GL_R8 | GL_R16F => TextureFormat::R8Unorm,
            GL_RG8 | GL_RG16F => TextureFormat::Rg8Unorm,
            GL_RGB16F | GL_RGBA16F => TextureFormat::Rgba16Float,
            GL_RG32F | GL_RGBA32F => TextureFormat::Rgba32Float,
            GL_R32F => TextureFormat::R32Float,
            GL_DEPTH_COMPONENT16 | GL_DEPTH_COMPONENT24 | GL_DEPTH_COMPONENT32F => {
                TextureFormat::Depth32Float
            }
            GL_DEPTH24_STENCIL8 | GL_DEPTH32F_STENCIL8 => TextureFormat::Depth24PlusStencil8,
            _ => return Err(()),
        })
    }
}

/// `glTexStorage3D(target, levels, internalformat, w, h, depth)` — immutable storage for a 2D-array / 3D
/// texture; `gl_shim.c` allocates the layer-0 RGBA8 plane. A non-array/3D `target` is `GL_INVALID_ENUM`;
/// a non-positive extent is `GL_INVALID_VALUE`.
pub fn tex_storage_3d(ctx: &mut GlContext, target: u32, levels: i32, w: i32, h: i32, depth: i32) {
    if target != GL_TEXTURE_2D_ARRAY && target != GL_TEXTURE_3D {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    if levels < 1 || w <= 0 || h <= 0 || depth <= 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    let name = ctx.local.tex_unit[ctx.local.active_texture];
    if name == 0 || ctx.textures.get(name).is_none() {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if ctx
        .textures
        .storage_2d(name, w, h, levels, TextureFormat::Rgba8Unorm)
    {
        // storage_2d marks immutable; that is correct for glTexStorage3D too.
    } else {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
}

/// `glTexImage3D` — 2D-array / 3D upload; `gl_shim.c` allocates RGBA8 and stores the layer-0 plane.
/// `rgba` is the already-converted RGBA8 layer-0 image (`w*h*4`) or empty (storage-only). A bad `target`
/// / `level != 0` / unknown texture is an honest no-op (matches the reference).
pub fn tex_image_3d(
    ctx: &mut GlContext,
    target: u32,
    level: i32,
    w: i32,
    h: i32,
    depth: i32,
    rgba: &[u8],
) {
    if level != 0 || (target != GL_TEXTURE_2D_ARRAY && target != GL_TEXTURE_3D) {
        return;
    }
    let name = ctx.local.tex_unit[ctx.local.active_texture];
    if name == 0 || ctx.textures.get(name).is_none() {
        return;
    }
    if !ctx.textures.alloc_rgba(name, w, h) {
        return;
    }
    if !rgba.is_empty() && depth > 0 {
        ctx.textures.sub_image_2d(name, 0, 0, w, h, rgba);
    }
}

/// `glTexSubImage2D(target, level, xo, yo, w, h, format, type, pixels)` — overwrite a sub-rect of the
/// bound 2D texture with the already-converted `rgba` (`w*h*4`). Honest GL errors: bad `target` →
/// `GL_INVALID_ENUM`; `level != 0`, negative extent/offset, no/unknown texture, or an out-of-bounds rect
/// → `GL_INVALID_VALUE`.
// The arguments intentionally mirror glTexSubImage2D after pixel-format conversion at the ABI adapter.
#[allow(clippy::too_many_arguments)]
pub fn tex_sub_image_2d(
    ctx: &mut GlContext,
    target: u32,
    level: i32,
    xo: i32,
    yo: i32,
    w: i32,
    h: i32,
    rgba: &[u8],
) {
    if target != GL_TEXTURE_2D {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    let name = ctx.local.tex_unit[ctx.local.active_texture];
    if level != 0 || w < 0 || h < 0 || xo < 0 || yo < 0 || name == 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if w == 0 || h == 0 {
        return;
    }
    if !ctx.textures.sub_image_2d(name, xo, yo, w, h, rgba) {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
}

/// `glTexSubImage3D` — layer-0 sub-image update for a 2D-array / 3D texture (`gl_shim.c` parity). A bad
/// `target` / `level != 0` / `zoffset != 0` / non-positive depth / dataless texture is an honest no-op.
// The arguments intentionally mirror glTexSubImage3D after pixel-format conversion at the ABI adapter.
#[allow(clippy::too_many_arguments)]
pub fn tex_sub_image_3d(
    ctx: &mut GlContext,
    target: u32,
    level: i32,
    xo: i32,
    yo: i32,
    zo: i32,
    w: i32,
    h: i32,
    depth: i32,
    rgba: &[u8],
) {
    if level != 0
        || zo != 0
        || depth <= 0
        || (target != GL_TEXTURE_2D_ARRAY && target != GL_TEXTURE_3D)
    {
        return;
    }
    let name = ctx.local.tex_unit[ctx.local.active_texture];
    if name == 0 {
        return;
    }
    ctx.textures.sub_image_2d(name, xo, yo, w, h, rgba);
}

/// `glCopyTexSubImage2D(target, level, xo, yo, x, y, w, h)` — copy a rect of the READ framebuffer's color
/// attachment into the bound texture's sub-rect.
///
/// HONEST LIMIT: like `glBlitFramebuffer`, the deferred model has no materialized source color plane at
/// record time (a frame's pixels exist only after swap), and the default framebuffer has no backing
/// texture object. So when the read framebuffer's color attachment carries materialized pixels the rect
/// is copied CPU-side; otherwise the call validates and is a documented no-op. Honest GL errors: bad
/// `target` / `level != 0` → `GL_INVALID_ENUM` / `GL_INVALID_VALUE`; a negative extent/offset or an
/// out-of-bounds destination rect → `GL_INVALID_VALUE`.
#[allow(clippy::too_many_arguments)]
pub fn copy_tex_sub_image_2d(
    ctx: &mut GlContext,
    target: u32,
    level: i32,
    xo: i32,
    yo: i32,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
) {
    if target != GL_TEXTURE_2D {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    let dst = ctx.local.tex_unit[ctx.local.active_texture];
    if level != 0 || w < 0 || h < 0 || xo < 0 || yo < 0 || x < 0 || y < 0 || dst == 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if w == 0 || h == 0 {
        return;
    }
    // Validate the destination rect fits the bound texture. The `+` are widened to i64 so a huge (or
    // adversarial) offset/extent can never overflow an i32 and panic — it just fails the fit and raises
    // GL_INVALID_VALUE.
    match ctx.textures.get(dst) {
        Some(t)
            if !t.data.is_empty()
                && xo as i64 + w as i64 <= t.w as i64
                && yo as i64 + h as i64 <= t.h as i64 => {}
        Some(_) => {
            ctx.set_gl_error(GL_INVALID_VALUE);
            return;
        }
        None => {
            ctx.set_gl_error(GL_INVALID_VALUE);
            return;
        }
    }
    // Source: the read framebuffer's color-attachment texture (if any). Copy the overlapping RGBA rows;
    // an absent/dataless source (default framebuffer, or an FBO not yet rendered) is the honest no-op.
    let src = ctx.local.framebuffers.color_attachment(ctx.local.read_fbo);
    let src_rows: Option<Vec<u8>> = ctx.textures.get(src).and_then(|st| {
        if st.data.is_empty()
            || x as i64 + w as i64 > st.w as i64
            || y as i64 + h as i64 > st.h as i64
        {
            return None;
        }
        let (sw, w, h) = (st.w as usize, w as usize, h as usize);
        let (x, y) = (x as usize, y as usize);
        let mut buf = Vec::with_capacity(w * h * 4);
        for row in 0..h {
            let base = ((y + row) * sw + x) * 4;
            buf.extend_from_slice(&st.data[base..base + w * 4]);
        }
        Some(buf)
    });
    if let Some(buf) = src_rows {
        ctx.textures.sub_image_2d(dst, xo, yo, w, h, &buf);
    }
}

/// `glDeleteTextures` (one name).
impl GlContext {
    pub fn delete_texture(&mut self, name: u32) -> bool {
        for u in self.local.tex_unit.iter_mut() {
            if *u == name {
                *u = 0;
            }
        }
        // Retire the texture's resident IR ids (sampled texture + FBO render target + depth), queued Destroy for
        // the next frame, so Chrome's fresh-tile churn does not climb the host residency ledger to its cap.
        self.retire_texture(name);
        self.textures.delete(name)
    }
}
