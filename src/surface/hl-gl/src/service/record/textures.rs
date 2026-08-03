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
    // Deletion immediately releases the public name. Container references retain the OLD object by stable
    // identity, so binding the same spelling creates a distinct texture rather than resurrecting it.
    if ctx.take_deleted_texture_name(name) {
        // Any unbound framebuffer attachments already carry this object's stable identity. Move it out of
        // the public name table before materializing the replacement.
        if let Some(generation) = ctx.textures.get(name).map(|texture| texture.gen) {
            ctx.retire_sampled_texture_generation(name, generation);
        }
        ctx.textures.retire_name(name);
    }
    ctx.textures.ensure(name);
    let unit = ctx.local.active_texture;
    if unit < ctx.local.tex_unit.len() {
        ctx.local.tex_unit[unit] = name;
    }
}

/// Validate the `internalformat`/`format`/`type` vocabulary of `glTexImage2D` for the active API.
pub fn validate_tex_image_2d(
    ctx: &mut GlContext,
    internalformat: u32,
    format: u32,
    type_: u32,
) -> bool {
    if ctx.client_version().0 >= 3 {
        return true;
    }
    let valid = (internalformat == format
        && matches!(
            (format, type_),
            (GL_RGB, GL_UNSIGNED_BYTE | GL_UNSIGNED_SHORT_5_6_5)
                | (
                    GL_RGBA,
                    GL_UNSIGNED_BYTE | GL_UNSIGNED_SHORT_4_4_4_4 | GL_UNSIGNED_SHORT_5_5_5_1
                )
                | (GL_BGRA_EXT, GL_UNSIGNED_BYTE)
        ))
        || (internalformat == GL_BGRA8_EXT && format == GL_BGRA_EXT && type_ == GL_UNSIGNED_BYTE);
    if !valid {
        ctx.set_gl_error(GL_INVALID_ENUM);
    }
    valid
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
    let stored = name != 0 && ctx.textures.image_2d(name, w, h, pixels, format);
    // INSTRUMENTED BUILDS ONLY. Whether the bytes the shim read were ACCEPTED into the CPU shadow, taken
    // from the real return value rather than from a second predicate that could disagree with it. The
    // shim is known to read the application's pixels correctly, so a refusal here leaves the texture
    // with zeroed storage and everything downstream working from an image nobody wrote -- which reads
    // as "the upload was corrupted" when the upload was discarded.
    #[cfg(feature = "verbose")]
    {
        use std::sync::atomic::{AtomicUsize, Ordering};
        static SEEN: AtomicUsize = AtomicUsize::new(0);
        let traced: Option<usize> = std::env::var("HL_GL_UPLOAD_TRACE_SPAN")
            .ok()
            .and_then(|value| value.parse().ok());
        let extension_bytes = pixels.starts_with(b"L_OE");
        if SEEN.fetch_add(1, Ordering::Relaxed) < 12
            || traced == Some(pixels.len())
            || extension_bytes
        {
            let head: Vec<String> = pixels.iter().take(8).map(|b| format!("{b:02x}")).collect();
            hl_log::hl_error!(
                hl_log::tag::GL,
                "record tex_image name={name} {w}x{h} format={format:?} bytes={} stored={stored} \
                 extension_bytes={extension_bytes} head=[{}]",
                pixels.len(),
                head.join(" ")
            );
        }
    }
    if name != 0 && !stored {
        // Supplied pixels that are not the size this plane's texel requires. The texture keeps the zeroed
        // plane its format names; the application is told, rather than left with an image whose bytes and
        // whose declared format disagree.
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
}

/// `glTexImage2D` with the SIZED internal format the application declared, which selects the plane
/// instead of being kept only as metadata — the same table `glTexStorage2D` reads, so the two ways of
/// allocating one texture stop disagreeing about what the format means. A format this driver does not
/// model keeps the RGBA8 plane rather than failing: `glTexImage2D` accepts unsized spellings, and the
/// declared format is still recorded for the completeness check either way.
///
/// The plane a declared sized `internalformat` names, or the RGBA8 plane for a format this driver does
/// not model — which is also the answer for the unsized `GL_RGB`/`GL_RGBA` spellings. One function so the
/// texture and renderbuffer allocation paths cannot come to different conclusions about one enum.
pub fn declared_plane(internalformat: u32) -> TextureFormat {
    TextureFormat::try_from(InternalFormat(internalformat)).unwrap_or(TextureFormat::Rgba8Unorm)
}

/// `glTexImage2D` with the SIZED internal format the application declared, WITH OR WITHOUT pixels.
///
/// A supplied image used to force the RGBA8 plane regardless of what was declared, because the upload
/// conversion narrowed every source to eight bits per channel before the destination was known — so
/// `glTexImage2D(GL_RGBA16F, …, floats)` produced an eight-bit plane, and giving it the declared plane
/// instead would only have put eight-bit bytes into a sixteen-bit-float buffer. The conversion now emits
/// texels of the destination plane ([`crate::service::upload::Upload::plane`]), so the declared format
/// selects the plane in both cases and this is the fourth allocation path that honours a float format.
pub fn tex_image_2d_declared(
    ctx: &mut GlContext,
    internalformat: u32,
    w: i32,
    h: i32,
    pixels: &[u8],
) {
    tex_image_2d_format(ctx, w, h, pixels, declared_plane(internalformat));
    tex_internal_format(ctx, internalformat);
}

/// Target-aware base-level define. Cube-face targets retain six independent planes; ordinary 2D targets
/// use the established single-plane path.
pub fn tex_image_2d_target_declared(
    ctx: &mut GlContext,
    target: u32,
    internalformat: u32,
    w: i32,
    h: i32,
    pixels: &[u8],
) {
    let Some(face) = cube_face_index(target) else {
        tex_image_2d_declared(ctx, internalformat, w, h, pixels);
        return;
    };
    let name = ctx.local.tex_unit[ctx.local.active_texture];
    let format = declared_plane(internalformat);
    let stored = name != 0 && ctx.textures.image_cube_face(name, face, w, h, pixels, format);
    tex_internal_format(ctx, internalformat);
    if name != 0 && !stored {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
}

fn cube_face_index(target: u32) -> Option<usize> {
    (GL_TEXTURE_CUBE_MAP_POSITIVE_X..=GL_TEXTURE_CUBE_MAP_NEGATIVE_Z)
        .contains(&target)
        .then_some((target - GL_TEXTURE_CUBE_MAP_POSITIVE_X) as usize)
}

/// Record the SIZED internal format the application declared for the active unit's texture. Kept as
/// metadata beside the neutral `ir_format` (which stays RGBA8, the plane this model materializes) so a
/// framebuffer completeness check can ask what the format actually WAS — `GL_SRGB8`, `GL_RGB9_E5`,
/// `GL_RGB8_SNORM` and `GL_RGB8UI` are indistinguishable once mapped to a neutral format, and none of
/// them may back a colour attachment. An unsized `GL_RGB`/`GL_RGBA` records `0`.
pub fn tex_internal_format(ctx: &mut GlContext, internalformat: u32) {
    let name = ctx.local.tex_unit[ctx.local.active_texture];
    if name == 0 {
        return;
    }
    if let Some(texture) = ctx.textures.get_mut(name) {
        texture.internal_format = internalformat;
    }
}

/// `glTexImage2D` at a level ABOVE the base — one image of a mip chain, stored beside the base image on
/// the active unit's texture rather than replacing it (see [`crate::model::texture::GlTexture::mips`]).
pub fn tex_image_2d_level(ctx: &mut GlContext, level: u32, w: i32, h: i32, pixels: &[u8]) {
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
        ctx.textures.image_2d_level(name, level, w, h, pixels);
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

/// `glGenerateMipmap(target)` — derive the whole mip chain from the base level by box filtering, down to
/// 1x1 (ES 3.0 §3.8.10). `target` must be a 2D/cube texture target (else `GL_INVALID_ENUM`) with a texture
/// bound to the active unit (else `GL_INVALID_OPERATION`).
///
/// This used to validate and then do nothing, because the model carried a single level. It is not a
/// harmless partial: an application that uploads level 0, calls this, and selects a mipmap min filter gets
/// a texture that is mipmap-COMPLETE on a conformant driver and was left with one level here.
impl GlContext {
    pub fn generate_mipmap(&mut self, target: u32) {
        if target != GL_TEXTURE_2D && target != GL_TEXTURE_CUBE_MAP {
            self.set_gl_error(GL_INVALID_ENUM);
            return;
        }
        let name = self.local.tex_unit[self.local.active_texture];
        if name == 0 {
            self.set_gl_error(GL_INVALID_OPERATION);
            return;
        }
        let Some(texture) = self.textures.get(name) else {
            self.set_gl_error(GL_INVALID_OPERATION);
            return;
        };
        if !texture.has_data() || texture.w <= 0 || texture.h <= 0 {
            return;
        }
        // `box_filter` averages texels as four unsigned bytes. On a half-float or float plane that
        // arithmetic produces a plausible image made of nonsense — bytes of an IEEE encoding averaged as
        // if they were colour channels — and the level extents would be wrong as well, since the reduction
        // walks rows at four bytes a texel. Declining leaves the texture with the levels it has, which a
        // completeness check can still see; the alternative writes a wrong answer no reader can question.
        if !texture.is_rgba8_plane() {
            return;
        }
        let mut levels: Vec<(i32, i32, Vec<u8>)> = Vec::new();
        let (mut w, mut h, mut source) = (texture.w, texture.h, (*texture.data).clone());
        while w > 1 || h > 1 {
            let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
            source = box_filter(&source, w, h, nw, nh);
            levels.push((nw, nh, source.clone()));
            w = nw;
            h = nh;
        }
        for (index, (lw, lh, data)) in levels.into_iter().enumerate() {
            self.textures
                .image_2d_level(name, index as u32 + 1, lw, lh, &data);
        }
    }
}

/// Average each 2x2 source block into one destination texel (the standard box reduction ES 3.0 §3.8.10
/// describes for a power-of-two halving). Odd source extents clamp the second sample to the last row or
/// column rather than reading past the image.
fn box_filter(source: &[u8], sw: i32, sh: i32, dw: i32, dh: i32) -> Vec<u8> {
    let mut out = vec![0u8; (dw as usize) * (dh as usize) * 4];
    let texel = |x: i32, y: i32, channel: usize| -> u32 {
        let x = x.min(sw - 1).max(0) as usize;
        let y = y.min(sh - 1).max(0) as usize;
        source
            .get((y * sw as usize + x) * 4 + channel)
            .copied()
            .unwrap_or(0) as u32
    };
    for y in 0..dh {
        for x in 0..dw {
            for channel in 0..4 {
                let sum = texel(x * 2, y * 2, channel)
                    + texel(x * 2 + 1, y * 2, channel)
                    + texel(x * 2, y * 2 + 1, channel)
                    + texel(x * 2 + 1, y * 2 + 1, channel);
                out[(y as usize * dw as usize + x as usize) * 4 + channel] = ((sum + 2) / 4) as u8;
            }
        }
    }
    out
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
    // `levels` may name a whole mip chain; it used to be required to be exactly 1, which is a large part
    // of why immutable storage was unusable. The ceiling is the chain a `w x h` base can actually have.
    let max_levels = 32 - (w.max(h).max(1) as u32).leading_zeros() as i32;
    if levels < 1 || levels > max_levels || w <= 0 || h <= 0 {
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
    if !ctx
        .textures
        .storage_2d(name, w, h, levels, fmt, internalformat)
    {
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
            | GL_RGB10_A2 | GL_RGB10_A2UI | GL_RGB9_E5 => TextureFormat::Rgba8Unorm,
            GL_SRGB8_ALPHA8 | GL_SRGB8 => TextureFormat::Rgba8Srgb,
            // Signed-normalized and integer sized formats. The model stores an RGBA8 plane for all of
            // them, so the neutral format is the same; keeping them ACCEPTED here is what makes immutable
            // storage usable at all — `glTexStorage2D` was raising GL_INVALID_ENUM for twenty valid sized
            // formats. Which of them may back a colour attachment is a separate question, answered by
            // `colour_renderable` (ES 3.0 table 3.13), not by this mapping.
            GL_R8_SNORM | GL_RG8_SNORM | GL_RGB8_SNORM | GL_RGBA8_SNORM => {
                TextureFormat::Rgba8Unorm
            }
            // The 8-bit INTEGER formats have real integer storage in the IR, so they keep it: an integer
            // texture carries raw texels a `usampler2D`/`isampler2D` reads with `texelFetch`, and routing
            // them onto an RGBA8 unorm plane would normalize values that have no normalized reading (a
            // texel of 200 is 200, not 200/255). The WIDER integer formats have no IR storage yet and stay
            // on the RGBA8 plane, an honest partial — their texels are narrowed on upload.
            GL_R8UI => TextureFormat::R8Uint,
            GL_R8I => TextureFormat::R8Sint,
            GL_RG8UI => TextureFormat::Rg8Uint,
            GL_RG8I => TextureFormat::Rg8Sint,
            GL_RGBA8UI => TextureFormat::Rgba8Uint,
            GL_RGBA8I => TextureFormat::Rgba8Sint,
            GL_R16UI | GL_R16I | GL_R32UI | GL_R32I => TextureFormat::Rgba8Unorm,
            GL_RG16UI | GL_RG16I | GL_RG32UI | GL_RG32I => TextureFormat::Rgba8Unorm,
            GL_RGB8UI | GL_RGB8I | GL_RGB16UI | GL_RGB16I | GL_RGB32UI | GL_RGB32I => {
                TextureFormat::Rgba8Unorm
            }
            GL_RGBA16UI | GL_RGBA16I => TextureFormat::Rgba8Unorm,
            GL_RGBA32UI => TextureFormat::Rgba32Uint,
            GL_RGBA32I => TextureFormat::Rgba32Sint,
            GL_RGB32F => TextureFormat::Rgba32Float,
            // Both spellings, because the extension that offers BGRA defines the UNSIZED one as the
            // internal format: `EXT_texture_format_BGRA8888` requires `internalformat == format ==
            // GL_BGRA_EXT`, so an application following the extension we advertise never names the sized
            // constant at all. Accepting only the sized spelling advertised a format and then refused it.
            GL_BGRA8_EXT | GL_BGRA_EXT => TextureFormat::Bgra8Unorm,
            GL_R8 => TextureFormat::R8Unorm,
            GL_RG8 => TextureFormat::Rg8Unorm,
            // Every FLOAT format lands on a float plane, widening to more channels where the IR has no
            // exact match — the same trade `GL_RG32F` already takes onto `Rgba32Float`. A half-float plane
            // represents every value of `GL_R11F_G11F_B10F` exactly (its components have a 5-bit exponent
            // and at most a 6-bit mantissa, against half's 5 and 10), so widening costs memory and never
            // precision. What it replaces cost precision and range: `GL_R16F` was an 8-bit UNORM plane, so
            // a texture asked for sixteen-bit floats was given one that quantises to 256 levels and clamps
            // everything above 1.0 — wrong today, whatever is decided about advertising a float colour
            // buffer. The extra channels are writable but not readable through the GL format's own
            // spelling, which is the ordinary rule for channels a format does not have.
            GL_R16F | GL_RG16F | GL_R11F_G11F_B10F => TextureFormat::Rgba16Float,
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
        .storage_2d(name, w, h, levels, TextureFormat::Rgba8Unorm, 0)
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
    if !ctx.textures.alloc_volume(name, w, h, depth, rgba) {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
}

/// `glTexSubImage2D(target, level, xo, yo, w, h, format, type, pixels)` — overwrite a sub-rect of the
/// bound 2D texture with the already-converted `rgba`, in the texel format of the destination plane
/// (`w * h * GlTexture::bytes_per_texel`). An upload sized for a different texel is refused rather than
/// laid down at the wrong stride, so it reaches the application as `GL_INVALID_VALUE`. Honest GL errors: bad `target` →
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
        || depth <= 0
        || (target != GL_TEXTURE_2D_ARRAY && target != GL_TEXTURE_3D)
    {
        return;
    }
    let name = ctx.local.tex_unit[ctx.local.active_texture];
    if name == 0 {
        return;
    }
    if !ctx.textures.sub_image_3d(name, xo, yo, zo, w, h, depth, rgba) {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
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
    let src_rows: Option<Vec<u8>> = ctx
        .framebuffer_color_texture(ctx.local.read_fbo, 0)
        .and_then(|(_, st)| {
        if st.data.is_empty()
            || x as i64 + w as i64 > st.w as i64
            || y as i64 + h as i64 > st.h as i64
        {
            return None;
        }
        // Rows come out at the SOURCE plane's own texel. The destination decides whether they are
        // acceptable: `sub_image_2d` measures the rect against its own plane's texel, so a copy between
        // planes of different width fails the length test rather than reinterpreting one as the other.
        let texel = st.bytes_per_texel();
        let (sw, w, h) = (st.w as usize, w as usize, h as usize);
        let (x, y) = (x as usize, y as usize);
        if st.data.len() != sw * st.h as usize * texel {
            return None;
        }
        let mut buf = Vec::with_capacity(w * h * texel);
        for row in 0..h {
            let base = ((y + row) * sw + x) * texel;
            buf.extend_from_slice(&st.data[base..base + w * texel]);
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
        // ES 3.0 §4.4.2.3 detaches the deleted object from attachment points of the CURRENTLY bound
        // framebuffer. Other framebuffer containers retain the object itself. Its public name is nevertheless
        // released immediately and may identify a distinct new texture on the next bind.
        let object = self.textures.object(name);
        self.local
            .framebuffers
            .detach_color_texture_from(self.local.bound_fbo, name);
        if self.local.read_fbo != self.local.bound_fbo {
            self.local
                .framebuffers
                .detach_color_texture_from(self.local.read_fbo, name);
        }
        if object.is_some_and(|object| self.local.framebuffers.references_object(object)) {
            if let Some(generation) = self.textures.get(name).map(|texture| texture.gen) {
                self.retire_sampled_texture_generation(name, generation);
            }
            self.textures.retire_name(name)
        } else {
            self.retire_texture(name);
            self.textures.delete(name)
        }
    }
}
