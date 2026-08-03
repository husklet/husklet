use super::*;

// ---- textures ------------------------------------------------------------------------------------

/// `glActiveTexture(GL_TEXTURE0 + i)`.
impl GlContext {
    pub fn active_texture(&mut self, texture: u32) {
        let unit = texture.wrapping_sub(GL_TEXTURE0) as usize;
        if unit < self.local.tex_unit.len() {
            self.local.active_texture = unit;
        } else {
            self.set_gl_error(GL_INVALID_ENUM);
        }
    }
}

/// `glBindTexture(GL_TEXTURE_2D, name)` — binds to the active texture unit.
pub fn bind_texture(ctx: &mut GlContext, target: u32, name: u32) {
    if !matches!(target, GL_TEXTURE_2D | GL_TEXTURE_CUBE_MAP) {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
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
    if name != 0 {
        ctx.textures.ensure(name);
        let texture = ctx.textures.get_mut(name).expect("ensured texture");
        if texture.target != 0 && texture.target != target {
            ctx.set_gl_error(GL_INVALID_OPERATION);
            return;
        }
        texture.target = target;
    }
    let unit = ctx.local.active_texture;
    if unit < ctx.local.tex_unit.len() {
        if target == GL_TEXTURE_CUBE_MAP {
            ctx.local.cube_tex_unit[unit] = name;
        } else {
            ctx.local.tex_unit[unit] = name;
        }
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
    let valid = valid_tex_image_2d_es2(internalformat, format, type_)
        || (internalformat == GL_BGRA8_EXT && format == GL_BGRA_EXT && type_ == GL_UNSIGNED_BYTE);
    if !valid {
        ctx.set_gl_error(GL_INVALID_ENUM);
    }
    valid
}

/// Validate the implementation-defined compressed-format vocabulary. This driver advertises zero
/// compressed formats to ES2, so accepting any spelling would contradict `GL_COMPRESSED_TEXTURE_FORMATS`.
pub fn validate_compressed_tex_image_2d_format(
    ctx: &mut GlContext,
    internalformat: u32,
) -> bool {
    if ctx.client_version().0 < 3 {
        let _ = internalformat;
        ctx.set_gl_error(GL_INVALID_ENUM);
        return false;
    }
    true
}

#[allow(clippy::too_many_arguments)]
pub fn validate_tex_image_2d_call(
    ctx: &mut GlContext,
    target: u32,
    level: i32,
    internalformat: i32,
    width: i32,
    height: i32,
    border: i32,
    format: u32,
    type_: u32,
) -> bool {
    let cube_face = cube_face_index(target).is_some();
    if target != GL_TEXTURE_2D && !cube_face {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return false;
    }
    let max_level = (crate::service::query::MAX_TEXTURE_SIZE as u32).ilog2() as i32;
    let max_extent = level
        .try_into()
        .ok()
        .and_then(|level: u32| crate::service::query::MAX_TEXTURE_SIZE.checked_shr(level))
        .unwrap_or(0);
    if level < 0
        || level > max_level
        || width < 0
        || height < 0
        || width > max_extent
        || height > max_extent
        || border != 0
        || (cube_face && width != height)
    {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return false;
    }
    let internalformat = match u32::try_from(internalformat) {
        Ok(value)
            if matches!(
                value,
                GL_ALPHA
                    | GL_LUMINANCE
                    | GL_LUMINANCE_ALPHA
                    | GL_RGB
                    | GL_RGBA
                    | GL_BGRA_EXT
                    | GL_BGRA8_EXT
            ) => value,
        _ => {
            ctx.set_gl_error(GL_INVALID_VALUE);
            return false;
        }
    };
    if !matches!(
        format,
        GL_ALPHA | GL_LUMINANCE | GL_LUMINANCE_ALPHA | GL_RGB | GL_RGBA | GL_BGRA_EXT
    )
        || !matches!(
            type_,
            GL_UNSIGNED_BYTE
                | GL_UNSIGNED_SHORT_5_6_5
                | GL_UNSIGNED_SHORT_4_4_4_4
                | GL_UNSIGNED_SHORT_5_5_5_1
        )
    {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return false;
    }
    let internal_matches = internalformat == format
        || (internalformat == GL_BGRA8_EXT && format == GL_BGRA_EXT);
    let type_matches = matches!((format, type_),
        (GL_ALPHA | GL_LUMINANCE | GL_LUMINANCE_ALPHA, GL_UNSIGNED_BYTE)
            | (GL_RGB, GL_UNSIGNED_BYTE | GL_UNSIGNED_SHORT_5_6_5)
            | (GL_RGBA, GL_UNSIGNED_BYTE | GL_UNSIGNED_SHORT_4_4_4_4 | GL_UNSIGNED_SHORT_5_5_5_1)
            | (GL_BGRA_EXT, GL_UNSIGNED_BYTE));
    if !internal_matches || !type_matches {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return false;
    }
    true
}

/// ES 2.0 table 3.4 legal unsized `glTexImage2D` format/type triples.
fn valid_tex_image_2d_es2(internalformat: u32, format: u32, type_: u32) -> bool {
    internalformat == format
        && matches!(
            (format, type_),
            (
                GL_ALPHA | GL_LUMINANCE | GL_LUMINANCE_ALPHA,
                GL_UNSIGNED_BYTE
            ) | (GL_RGB, GL_UNSIGNED_BYTE | GL_UNSIGNED_SHORT_5_6_5)
                | (
                    GL_RGBA,
                    GL_UNSIGNED_BYTE | GL_UNSIGNED_SHORT_4_4_4_4 | GL_UNSIGNED_SHORT_5_5_5_1
                )
                | (GL_BGRA_EXT, GL_UNSIGNED_BYTE)
        )
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    #[test]
    fn es2_accepts_legacy_single_and_dual_channel_uploads() {
        for format in [GL_ALPHA, GL_LUMINANCE, GL_LUMINANCE_ALPHA] {
            assert!(valid_tex_image_2d_es2(format, format, GL_UNSIGNED_BYTE));
            let mut context = GlContext::new();
            assert!(validate_tex_image_2d_call(
                &mut context,
                GL_TEXTURE_2D,
                0,
                format as i32,
                1,
                1,
                0,
                format,
                GL_UNSIGNED_BYTE,
            ));
            assert_eq!(context.take_gl_error(), GL_NO_ERROR);
        }
    }

    #[test]
    fn es2_legacy_formats_still_require_matching_unsigned_byte_input() {
        assert!(!valid_tex_image_2d_es2(
            GL_LUMINANCE,
            GL_ALPHA,
            GL_UNSIGNED_BYTE
        ));
        assert!(!valid_tex_image_2d_es2(
            GL_LUMINANCE,
            GL_LUMINANCE,
            GL_FLOAT
        ));
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
    let name = ctx.bound_texture_for_target(target);
    let key = if name == 0 {
        crate::model::context::DEFAULT_TEXTURE_CUBE
    } else {
        u64::from(name)
    };
    let format = declared_plane(internalformat);
    let stored = ctx
        .textures
        .image_cube_face(key, face, w, h, pixels, format, internalformat);
    if let Some(texture) = ctx.textures.get_internal_mut(key) {
        texture.internal_format = internalformat;
    }
    if !stored {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
}

fn cube_face_index(target: u32) -> Option<usize> {
    (GL_TEXTURE_CUBE_MAP_POSITIVE_X..=GL_TEXTURE_CUBE_MAP_NEGATIVE_Z)
        .contains(&target)
        .then(|| (target - GL_TEXTURE_CUBE_MAP_POSITIVE_X) as usize)
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

/// Target-aware non-base define. Cube faces retain independent mip images.
pub fn tex_image_2d_target_level(
    ctx: &mut GlContext,
    target: u32,
    level: u32,
    w: i32,
    h: i32,
    pixels: &[u8],
    internalformat: u32,
) {
    let Some(face) = cube_face_index(target) else {
        tex_image_2d_level(ctx, level, w, h, pixels);
        return;
    };
    if w < 0
        || h < 0
        || w > crate::service::query::MAX_TEXTURE_SIZE
        || h > crate::service::query::MAX_TEXTURE_SIZE
    {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    let name = ctx.bound_texture_for_target(target);
    let key = if name == 0 {
        crate::model::context::DEFAULT_TEXTURE_CUBE
    } else {
        u64::from(name)
    };
    if !ctx
        .textures
        .image_cube_level(key, face, level, w, h, pixels, internalformat)
    {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
}

/// `glTexParameteri(GL_TEXTURE_2D, pname, value)` on the active unit's texture.
pub fn validate_tex_parameter(ctx: &mut GlContext, target: u32, pname: u32, value: u32) -> bool {
    let valid_target = matches!(target, GL_TEXTURE_2D | GL_TEXTURE_CUBE_MAP);
    let valid_value = match pname {
        GL_TEXTURE_MAG_FILTER => matches!(value, GL_NEAREST | GL_LINEAR),
        GL_TEXTURE_MIN_FILTER => matches!(
            value,
            GL_NEAREST
                | GL_LINEAR
                | GL_NEAREST_MIPMAP_NEAREST
                | GL_LINEAR_MIPMAP_NEAREST
                | GL_NEAREST_MIPMAP_LINEAR
                | GL_LINEAR_MIPMAP_LINEAR
        ),
        GL_TEXTURE_WRAP_S | GL_TEXTURE_WRAP_T => {
            matches!(value, GL_CLAMP_TO_EDGE | GL_REPEAT | GL_MIRRORED_REPEAT)
        }
        GL_TEXTURE_SWIZZLE_R
        | GL_TEXTURE_SWIZZLE_G
        | GL_TEXTURE_SWIZZLE_B
        | GL_TEXTURE_SWIZZLE_A
        | GL_TEXTURE_SWIZZLE_RGBA
        | GL_TEXTURE_BASE_LEVEL
        | GL_TEXTURE_MAX_LEVEL
        | GL_TEXTURE_MIN_LOD
        | GL_TEXTURE_MAX_LOD
        | GL_TEXTURE_COMPARE_MODE
        | GL_TEXTURE_COMPARE_FUNC
            if ctx.client_version().0 >= 3 =>
        {
            true
        }
        _ => false,
    };
    if !valid_target || !valid_value {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return false;
    }
    true
}

/// `glTexParameteri(GL_TEXTURE_2D, pname, value)` on the active unit's texture. Callers crossing the GL
/// API boundary validate target/pname/value with [`validate_tex_parameter`] first.
pub fn tex_parameter(ctx: &mut GlContext, pname: u32, value: u32) {
    tex_parameter_target(ctx, GL_TEXTURE_2D, pname, value);
}

pub fn tex_parameter_target(ctx: &mut GlContext, target: u32, pname: u32, value: u32) {
    let name = ctx.bound_texture_for_target(target);
    if target == GL_TEXTURE_CUBE_MAP && name == 0 {
        ctx.textures
            .set_param_internal(crate::model::context::DEFAULT_TEXTURE_CUBE, pname, value);
    } else if name != 0 {
        ctx.textures.set_param(name, pname, value);
    }
}

/// Vector texture parameter setter. Only `GL_TEXTURE_SWIZZLE_RGBA` consumes four values.
pub fn tex_parameter_vector(ctx: &mut GlContext, pname: u32, values: &[u32]) {
    tex_parameter_vector_target(ctx, GL_TEXTURE_2D, pname, values);
}

pub fn tex_parameter_vector_target(
    ctx: &mut GlContext,
    target: u32,
    pname: u32,
    values: &[u32],
) {
    let name = ctx.bound_texture_for_target(target);
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
        let name = self.bound_texture_for_target(target);
        if name == 0 {
            self.set_gl_error(GL_INVALID_OPERATION);
            return;
        }
        let Some(texture) = self.textures.get(name) else {
            self.set_gl_error(GL_INVALID_OPERATION);
            return;
        };
        if self.client_version().0 < 3 {
            if target == GL_TEXTURE_CUBE_MAP && !texture.cube_complete(false) {
                self.set_gl_error(GL_INVALID_OPERATION);
                return;
            }
            if !(texture.w as u32).is_power_of_two() || !(texture.h as u32).is_power_of_two() {
                self.set_gl_error(GL_INVALID_OPERATION);
                return;
            }
        }
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
        let faces = if target == GL_TEXTURE_CUBE_MAP {
            texture
                .cube_faces()
                .iter()
                .map(|face| face.as_ref().cloned())
                .collect::<Option<Vec<_>>>()
        } else {
            Some(vec![std::sync::Arc::clone(&texture.data)])
        };
        let Some(faces) = faces else { return };
        let (base_w, base_h, internal_format) = (texture.w, texture.h, texture.internal_format);
        for (face, base) in faces.into_iter().enumerate() {
            let (mut w, mut h, mut source) = (base_w, base_h, (*base).clone());
            let mut level = 1u32;
            while w > 1 || h > 1 {
                let (nw, nh) = ((w / 2).max(1), (h / 2).max(1));
                source = box_filter(&source, w, h, nw, nh);
                if target == GL_TEXTURE_CUBE_MAP {
                    self.textures.image_cube_level(
                        u64::from(name),
                        face,
                        level,
                        nw,
                        nh,
                        &source,
                        internal_format,
                    );
                } else {
                    self.textures.image_2d_level(name, level, nw, nh, &source);
                }
                w = nw;
                h = nh;
                level += 1;
            }
        }
    }
}

pub fn validate_texture_object_count(ctx: &mut GlContext, count: i32) -> bool {
    if count < 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return false;
    }
    true
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
    let name = ctx.bound_texture_for_target(target);
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
    if level < 0 || (target != GL_TEXTURE_2D_ARRAY && target != GL_TEXTURE_3D) {
        return;
    }
    let name = ctx.local.tex_unit[ctx.local.active_texture];
    if name == 0 || ctx.textures.get(name).is_none() {
        return;
    }
    let accepted = if level == 0 {
        ctx.textures.alloc_volume(name, w, h, depth, rgba)
    } else {
        ctx.textures
            .image_3d_level(name, level as u32, w, h, depth, rgba)
    };
    if !accepted {
        ctx.set_gl_error(GL_INVALID_VALUE);
    }
}

/// `glTexSubImage2D(target, level, xo, yo, w, h, format, type, pixels)` — overwrite a sub-rect of the
/// bound 2D texture with the already-converted `rgba`, in the texel format of the destination plane
/// (`w * h * GlTexture::bytes_per_texel`). An upload sized for a different texel is refused rather than
/// laid down at the wrong stride, so it reaches the application as `GL_INVALID_VALUE`. Honest GL errors: bad `target` →
/// `GL_INVALID_ENUM`; a negative level/extent/offset, no/unknown texture, or an out-of-bounds rect →
/// `GL_INVALID_VALUE`.
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
    let face = cube_face_index(target);
    if target != GL_TEXTURE_2D && face.is_none() {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    let name = ctx.bound_texture_for_target(target);
    if level < 0 || w < 0 || h < 0 || xo < 0 || yo < 0 || name == 0 {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if w == 0 || h == 0 {
        return;
    }
    let before = ctx.textures.get(name).cloned();
    let producer = before.as_ref().and_then(|texture| {
        ctx.local.recording.operations.iter().rev().find_map(|operation| match operation {
            crate::model::context::FrameOp::Draw(draw)
                if draw.target.is_some_and(|target| {
                    target.texture == name && target.generation == texture.gen
                }) => Some(draw.fbo),
            _ => None,
        })
    });
    let stored = match (level, face) {
        (0, Some(face)) => ctx
            .textures
            .sub_image_cube_face(name, face, xo, yo, w, h, rgba),
        (0, None) => ctx.textures.sub_image_2d(name, xo, yo, w, h, rgba),
        (level, face) => ctx.textures.sub_image_2d_level(
            name,
            face,
            level as u32,
            xo,
            yo,
            w,
            h,
            rgba,
        ),
    };
    if !stored {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if level == 0 && face.is_none() {
        if let (Some(before), Some(fbo), Some(after)) = (before, producer, ctx.textures.get(name)) {
            ctx.record_tex_sub_image(crate::model::context::TexSubImageOp {
                fbo,
                texture: name,
                source_generation: before.gen,
                destination_generation: after.gen,
                offset: [xo, yo],
                extent: [w, h],
                texture_extent: [after.w, after.h],
                format: after.ir_format,
                pixels: std::sync::Arc::new(rgba.to_vec()),
            });
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn validate_tex_sub_image_2d_call(
    ctx: &mut GlContext,
    target: u32,
    level: i32,
    xoffset: i32,
    yoffset: i32,
    width: i32,
    height: i32,
    format: u32,
    type_: u32,
) -> bool {
    if target != GL_TEXTURE_2D && cube_face_index(target).is_none() {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return false;
    }
    if !matches!(format, GL_RGB | GL_RGBA | GL_BGRA_EXT)
        || !matches!(
            type_,
            GL_UNSIGNED_BYTE
                | GL_UNSIGNED_SHORT_5_6_5
                | GL_UNSIGNED_SHORT_4_4_4_4
                | GL_UNSIGNED_SHORT_5_5_5_1
        )
    {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return false;
    }
    let type_matches = matches!(
        (format, type_),
        (GL_RGB, GL_UNSIGNED_BYTE | GL_UNSIGNED_SHORT_5_6_5)
            | (
                GL_RGBA,
                GL_UNSIGNED_BYTE | GL_UNSIGNED_SHORT_4_4_4_4 | GL_UNSIGNED_SHORT_5_5_5_1
            )
            | (GL_BGRA_EXT, GL_UNSIGNED_BYTE)
    );
    if !type_matches {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return false;
    }
    let max_level = (crate::service::query::MAX_TEXTURE_SIZE as u32).ilog2() as i32;
    if level < 0
        || level > max_level
        || xoffset < 0
        || yoffset < 0
        || width < 0
        || height < 0
    {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return false;
    }
    if level == 0 {
        let name = ctx.bound_texture_for_target(target);
        if let Some(texture) = ctx.textures.get(name) {
            let xend = xoffset.checked_add(width);
            let yend = yoffset.checked_add(height);
            if xend.is_none_or(|end| end > texture.w) || yend.is_none_or(|end| end > texture.h) {
                ctx.set_gl_error(GL_INVALID_VALUE);
                return false;
            }
        }
    }
    true
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
    if level < 0 || depth <= 0 || (target != GL_TEXTURE_2D_ARRAY && target != GL_TEXTURE_3D) {
        return;
    }
    let name = ctx.local.tex_unit[ctx.local.active_texture];
    if name == 0 {
        return;
    }
    if !ctx
        .textures
        .sub_image_3d_level(name, level as u32, xo, yo, zo, w, h, depth, rgba)
    {
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
    let face = cube_face_index(target);
    if target != GL_TEXTURE_2D && face.is_none() {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    let dst = ctx.bound_texture_for_target(target);
    let max_level = (crate::service::query::MAX_TEXTURE_SIZE as u32).ilog2() as i32;
    if level < 0
        || level > max_level
        || w < 0
        || h < 0
        || xo < 0
        || yo < 0
        || x < 0
        || y < 0
        || dst == 0
    {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if ctx.framebuffer_status(ctx.local.read_fbo) != GL_FRAMEBUFFER_COMPLETE {
        ctx.set_gl_error(GL_INVALID_FRAMEBUFFER_OPERATION);
        return;
    }
    if w == 0 || h == 0 {
        return;
    }
    // Validate the destination rect fits the bound texture. The `+` are widened to i64 so a huge (or
    // adversarial) offset/extent can never overflow an i32 and panic — it just fails the fit and raises
    // GL_INVALID_VALUE.
    let image_extent = ctx.textures.get(dst).and_then(|texture| {
        if level == 0 {
            Some((texture.w, texture.h))
        } else {
            texture.mips.get(level as usize - 1).map(|mip| (mip.w, mip.h))
        }
    });
    match image_extent {
        Some((tw, th))
            if xo as i64 + w as i64 <= tw as i64 && yo as i64 + h as i64 <= th as i64 => {}
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
    let source_written_in_frame = ctx.local.recording.operations.iter().any(|operation| match operation {
        crate::model::context::FrameOp::Draw(draw) => draw.fbo == ctx.local.read_fbo,
        crate::model::context::FrameOp::Blit(blit) => blit.draw_fbo == ctx.local.read_fbo,
        crate::model::context::FrameOp::CopyTex(_) => false,
        crate::model::context::FrameOp::TexSubImage(_) => false,
    });
    let source_gpu_authoritative = ctx
        .framebuffer_color_texture(ctx.local.read_fbo, 0)
        .is_some_and(|(_, texture)| texture.gpu_authoritative());
    let src_rows: Option<Vec<u8>> = (!source_written_in_frame && !source_gpu_authoritative)
        .then(|| ctx.framebuffer_color_texture(ctx.local.read_fbo, 0))
        .flatten()
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
    if level == 0 && face.is_none() {
        if let Some(buf) = src_rows {
            if ctx.textures.sub_image_2d(dst, xo, yo, w, h, &buf) {
                return;
            }
        }
    }
    let generation = ctx.textures.get(dst).map(|texture| texture.gen).unwrap_or(0);
    let read_target = (ctx.local.read_fbo != 0)
        .then(|| ctx.framebuffer_color_texture(ctx.local.read_fbo, 0))
        .flatten()
        .map(|(name, texture)| crate::model::program::TargetSnapshot {
            texture: name,
            generation: texture.gen,
            shared_storage: texture.shared_storage(),
            shared_revision: texture.shared_current_identity().map(|(_, revision)| revision),
            width: texture.w,
            height: texture.h,
            format: texture.ir_format,
        });
    ctx.record_copy_tex(crate::model::context::CopyTexOp {
        read_fbo: ctx.local.read_fbo,
        read_target,
        read_ir: None,
        texture: dst,
        generation,
        cube: face.is_some(),
        face: face.unwrap_or(0) as u32,
        level: level as u32,
        src: [x, y],
        dst: [xo, yo],
        extent: [w, h],
    });
}

/// Define a texture image and defer its framebuffer pixel transfer until the ordered frame is lowered.
#[allow(clippy::too_many_arguments)]
pub fn copy_tex_image_2d(
    ctx: &mut GlContext,
    target: u32,
    level: i32,
    internalformat: u32,
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    border: i32,
) {
    let face = cube_face_index(target);
    if target != GL_TEXTURE_2D && face.is_none() {
        ctx.set_gl_error(GL_INVALID_ENUM);
        return;
    }
    let max_level = (crate::service::query::MAX_TEXTURE_SIZE as u32).ilog2() as i32;
    let max_extent = level
        .try_into()
        .ok()
        .and_then(|level: u32| crate::service::query::MAX_TEXTURE_SIZE.checked_shr(level))
        .unwrap_or(0);
    if level < 0
        || level > max_level
        || border != 0
        || width < 0
        || height < 0
        || width > max_extent
        || height > max_extent
        || (face.is_some() && width != height)
    {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if !matches!(
        internalformat,
        GL_ALPHA
            | GL_LUMINANCE
            | GL_LUMINANCE_ALPHA
            | GL_RGB
            | GL_RGBA
            | GL_BGRA_EXT
            | GL_BGRA8_EXT
    ) {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    // Completeness is validated before redefining the destination. A refused framebuffer copy cannot
    // partially mutate texture storage and then report failure.
    if ctx.framebuffer_status(ctx.local.read_fbo) != GL_FRAMEBUFFER_COMPLETE {
        ctx.set_gl_error(GL_INVALID_FRAMEBUFFER_OPERATION);
        return;
    }
    let name = ctx.bound_texture_for_target(target);
    if name == 0 {
        ctx.set_gl_error(GL_INVALID_OPERATION);
        return;
    }
    if level == 0 {
        tex_image_2d_target_declared(ctx, target, internalformat, width, height, &[]);
    } else if let Some(face) = face {
        if !ctx.textures.image_cube_level(
            u64::from(name), face, level as u32, width, height, &[], internalformat,
        ) {
            ctx.set_gl_error(GL_INVALID_VALUE);
            return;
        }
    } else if !ctx
        .textures
        .image_2d_level(name, level as u32, width, height, &[])
    {
        ctx.set_gl_error(GL_INVALID_VALUE);
        return;
    }
    if level != 0 {
        tex_internal_format(ctx, internalformat);
    }
    copy_tex_sub_image_2d(ctx, target, level, 0, 0, x, y, width, height);
}

/// `glDeleteTextures` (one name).
impl GlContext {
    pub fn delete_texture(&mut self, name: u32) -> bool {
        self.clear_object_label(GL_TEXTURE, name);
        for u in self.local.tex_unit.iter_mut() {
            if *u == name {
                *u = 0;
            }
        }
        for u in self.local.cube_tex_unit.iter_mut() {
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
