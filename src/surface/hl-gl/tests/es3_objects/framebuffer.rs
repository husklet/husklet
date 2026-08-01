use super::*;

#[test]
fn framebuffer_attachment_parameter_reflects_default_and_texture_attachment() {
    let mut c = ctx();

    // The default framebuffer's back buffer reports FRAMEBUFFER_DEFAULT.
    assert_eq!(
        intro::framebuffer_attachment_parameter(
            &c,
            GL_DRAW_FRAMEBUFFER,
            GL_BACK,
            GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE
        ),
        GL_FRAMEBUFFER_DEFAULT as i32
    );

    // Bind an FBO with a color texture attachment.
    let tex = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(&mut c, 8, 8, &[0u8; 8 * 8 * 4]);
    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        tex,
        0,
    );

    assert_eq!(
        intro::framebuffer_attachment_parameter(
            &c,
            GL_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE
        ),
        GL_TEXTURE as i32,
        "a texture-backed color attachment reports OBJECT_TYPE == GL_TEXTURE"
    );
    assert_eq!(
        intro::framebuffer_attachment_parameter(
            &c,
            GL_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME
        ),
        tex as i32,
        "the attachment object NAME is the attached texture"
    );
}

// ---------------------------------------------------------------------------------------------------
// Colour-renderability decides completeness (ES 3.0 §4.4.4 + table 3.13)
// ---------------------------------------------------------------------------------------------------

/// Attach a texture of `internal_format` at `GL_COLOR_ATTACHMENT0` and report the completeness status.
fn status_for_colour_attachment(internal_format: u32) -> u32 {
    let mut c = ctx();
    let tex = c.textures.gen();
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(&mut c, 16, 16, &[]);
    record::tex_internal_format(&mut c, internal_format);
    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        tex,
        0,
    );
    c.check_framebuffer_status(GL_FRAMEBUFFER)
}

/// The driver reported COMPLETE for every one of these, handing the application a framebuffer that cannot
/// work and no way to discover it. Each is absent from ES 3.0 table 3.13 for its own reason: depth is not
/// a colour format; snorm and shared-exponent/packed-float are sampled-only; `GL_SRGB8` is excluded while
/// `GL_SRGB8_ALPHA8` is included; and the THREE-component integer formats are omitted where their one-,
/// two- and four-component siblings are present.
#[test]
fn a_non_colour_renderable_attachment_is_incomplete() {
    for format in [
        GL_DEPTH_COMPONENT16,
        GL_DEPTH_COMPONENT24,
        GL_RGB8_SNORM,
        GL_RGB9_E5,
        GL_SRGB8,
        GL_RGB8UI,
        GL_R11F_G11F_B10F,
    ] {
        assert_eq!(
            status_for_colour_attachment(format),
            GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT,
            "{format:#x} is not colour-renderable"
        );
    }
}

#[test]
fn a_colour_renderable_attachment_is_complete() {
    for format in [
        0, // an UNSIZED GL_RGB/GL_RGBA upload — the ordinary path, which must stay complete
        GL_RGBA8,
        GL_RGB8,
        GL_R8,
        GL_RG8,
        GL_RGB565,
        GL_RGBA4,
        GL_RGB5_A1,
        GL_RGB10_A2,
        GL_SRGB8_ALPHA8,
        GL_RGBA8UI,
        GL_R32I,
    ] {
        assert_eq!(
            status_for_colour_attachment(format),
            GL_FRAMEBUFFER_COMPLETE,
            "{format:#x} is colour-renderable"
        );
    }
}

/// The same question asked through the RENDERBUFFER path, which is the other way to reach a colour
/// attachment and was not gated at all: `glRenderbufferStorage` dropped its `internalformat` on the
/// floor, so completeness had nothing to consult and answered COMPLETE for every format there is. The
/// texture path above has enforced ES 3.0 table 3.13 since the rule was written; two ways in, one rule,
/// and only one of them applied it.
///
/// The pairing is the point. A test that only asserted the refusals would pass just as well against a
/// path that refuses everything, so the renderable formats are asserted through the same helper.
fn status_for_renderbuffer_attachment(internal_format: u32) -> u32 {
    let mut c = ctx();
    let rbo = c.gen_renderbuffer();
    record::bind_renderbuffer(&mut c, GL_RENDERBUFFER, rbo);
    record::renderbuffer_storage(&mut c, GL_RENDERBUFFER, internal_format, 16, 16);
    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_renderbuffer(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_RENDERBUFFER,
        rbo,
    );
    c.check_framebuffer_status(GL_FRAMEBUFFER)
}

#[test]
fn a_non_colour_renderable_renderbuffer_is_incomplete() {
    for format in [GL_RGB8_SNORM, GL_RGB9_E5, GL_SRGB8, GL_RGB8UI, GL_R11F_G11F_B10F] {
        assert_eq!(
            status_for_renderbuffer_attachment(format),
            GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT,
            "{format:#x} is not colour-renderable through a renderbuffer either"
        );
    }
}

#[test]
fn a_colour_renderable_renderbuffer_is_complete() {
    for format in [
        GL_RGBA8,
        GL_RGB8,
        GL_RGB565,
        GL_RGB5_A1,
        GL_SRGB8_ALPHA8,
        GL_R8UI,
        GL_RGBA8UI,
        GL_RGBA32I,
    ] {
        assert_eq!(
            status_for_renderbuffer_attachment(format),
            GL_FRAMEBUFFER_COMPLETE,
            "{format:#x} is colour-renderable and must stay usable"
        );
    }
}

/// An IMPORTED image is a colour buffer by construction — a dma-buf or IOSurface the compositor already
/// owns — so a framebuffer wrapping one is complete whatever the texture name was used for before.
///
/// `external_image_2d` resets the extent, the neutral format, the pixel data and the generation, which
/// makes it read as a full redefinition. It does not reset the DECLARED internal format, so completeness
/// judged the import by the format of whatever the texture used to be. Skia performs exactly that
/// sequence — allocate a backend texture with a sized format, then wrap a shared image over it — and a
/// predecessor format outside the renderable set left the wrapped surface permanently incomplete.
#[test]
fn an_imported_image_is_judged_by_itself_not_by_what_the_texture_used_to_be() {
    for predecessor in [GL_RGBA8, GL_RGB8_SNORM, GL_SRGB8, GL_RGB9_E5, GL_RGBA16F] {
        let mut c = ctx();
        let tex = c.textures.gen();
        record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
        // What the texture was before the import: immutable storage of a sized format.
        record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 1, predecessor, 16, 16);
        // The import, exactly as `glEGLImageTargetTexture2DOES` performs it.
        c.textures.external_image_2d(
            tex,
            16,
            16,
            hl_gpu::protocol::model::enums::TextureFormat::Bgra8Unorm,
        );

        let fbo = c.gen_framebuffer();
        record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
        record::framebuffer_texture_2d(
            &mut c,
            GL_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_TEXTURE_2D,
            tex,
            0,
        );
        assert_eq!(
            c.check_framebuffer_status(GL_FRAMEBUFFER),
            GL_FRAMEBUFFER_COMPLETE,
            "an imported image after a {predecessor:#x} texture must be complete on its own terms"
        );
    }
}

/// `GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE` must distinguish a renderbuffer attachment from a texture one.
///
/// A renderbuffer is BACKED by a texture in this model, so reading the colour table alone reported
/// `GL_TEXTURE` — and `_OBJECT_NAME` reported the backing texture's name rather than the renderbuffer's.
/// That is precisely the distinction the query exists to make (ES 3.0 §6.1.13), and the texture case
/// agreeing is what showed the attachment tracking was otherwise sound.
#[test]
fn the_attachment_query_names_the_object_that_was_attached() {
    let mut c = ctx();
    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);

    // A RENDERBUFFER attachment.
    let rbo = c.gen_renderbuffer();
    record::bind_renderbuffer(&mut c, GL_RENDERBUFFER, rbo);
    record::renderbuffer_storage(&mut c, GL_RENDERBUFFER, GL_RGBA4, 16, 16);
    record::framebuffer_renderbuffer(&mut c, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_RENDERBUFFER, rbo);
    assert_eq!(
        intro::framebuffer_attachment_parameter(
            &c,
            GL_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE
        ),
        GL_RENDERBUFFER as i32,
        "a renderbuffer attachment is a renderbuffer, not the texture backing it"
    );
    assert_eq!(
        intro::framebuffer_attachment_parameter(
            &c,
            GL_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME
        ),
        rbo as i32,
        "and its name is the renderbuffer's"
    );

    // A TEXTURE attachment on the same slot — this case already agreed and must keep agreeing.
    let tex = c.textures.gen();
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(&mut c, 16, 16, &[]);
    record::framebuffer_texture_2d(&mut c, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
    assert_eq!(
        intro::framebuffer_attachment_parameter(
            &c,
            GL_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE
        ),
        GL_TEXTURE as i32
    );
    assert_eq!(
        intro::framebuffer_attachment_parameter(
            &c,
            GL_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_FRAMEBUFFER_ATTACHMENT_OBJECT_NAME
        ),
        tex as i32
    );

    // Detaching leaves NONE — the post-delete case that also already agreed.
    record::framebuffer_texture_2d(&mut c, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, 0, 0);
    assert_eq!(
        intro::framebuffer_attachment_parameter(
            &c,
            GL_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_FRAMEBUFFER_ATTACHMENT_OBJECT_TYPE
        ),
        GL_NONE as i32
    );
}

/// A float texture must get a float PLANE, sized by that plane's own texel. Both halves were wrong and
/// each hid the other.
///
/// `GL_R16F` and `GL_RG16F` resolved to eight-bit UNORM planes and `GL_R11F_G11F_B10F` to RGBA8, so a
/// texture that asked for floats was given 256 levels and a hard clamp at 1.0 — wrong on its own terms,
/// with or without a float colour-buffer extension. And the zeroed storage was sized at a hardcoded four
/// bytes per texel, so the formats that DID resolve to a float plane were allocated at half or a quarter
/// of their own size: an `Rgba16Float` 8x8 declared 512 bytes' worth of format over a 256-byte buffer.
///
/// Widening to more channels than the GL format names is the trade `GL_RG32F` already takes onto
/// `Rgba32Float`; half-float represents every `GL_R11F_G11F_B10F` component exactly, so it costs memory
/// and never precision. The narrow formats are asserted alongside because the plane size has a FLOOR of
/// RGBA8 — the CPU shadow is an RGBA8 image for them and several writers address it that way.
#[test]
fn a_float_texture_gets_a_float_plane_sized_by_its_own_texel() {
    use hl_gpu::protocol::model::enums::TextureFormat;

    for (declared, plane) in [
        (GL_R16F, TextureFormat::Rgba16Float),
        (GL_RG16F, TextureFormat::Rgba16Float),
        (GL_RGB16F, TextureFormat::Rgba16Float),
        (GL_RGBA16F, TextureFormat::Rgba16Float),
        (GL_R11F_G11F_B10F, TextureFormat::Rgba16Float),
        (GL_R32F, TextureFormat::R32Float),
        (GL_RG32F, TextureFormat::Rgba32Float),
        (GL_RGBA32F, TextureFormat::Rgba32Float),
        // The narrow formats keep the RGBA8-shadow floor they have always had.
        (GL_RGBA8, TextureFormat::Rgba8Unorm),
        (GL_R8, TextureFormat::R8Unorm),
    ] {
        let mut c = ctx();
        let tex = c.textures.gen();
        record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
        record::tex_storage_2d(&mut c, GL_TEXTURE_2D, 1, declared, 8, 8);
        let stored = c.textures.get(tex).expect("immutable storage");
        assert_eq!(stored.ir_format, plane, "{declared:#x} plane");
        let want = 8 * 8 * plane.bytes_per_texel().unwrap_or(4).max(4);
        assert_eq!(
            stored.data.len(),
            want,
            "{declared:#x} plane must be sized by its own texel"
        );
    }
}

/// A renderbuffer allocates the plane its declared format names, through the same table the two texture
/// paths read. It was the last of the three colour-attachment paths still allocating RGBA8 whatever it
/// was asked for while recording the declared format for the completeness check — a combination that can
/// report a half-float renderbuffer as complete over an eight-bit plane, which no caller can check.
///
/// It is also the safest of the three to derive, because a renderbuffer has no pixel upload: its storage
/// is allocated and never filled, so no converted data can disagree with a wider plane.
#[test]
fn a_renderbuffer_allocates_the_plane_its_declared_format_names() {
    use hl_gpu::protocol::model::enums::TextureFormat;

    for (declared, plane) in [
        (GL_RGBA16F, TextureFormat::Rgba16Float),
        (GL_RGBA32F, TextureFormat::Rgba32Float),
        (GL_R32F, TextureFormat::R32Float),
        (GL_RGBA8, TextureFormat::Rgba8Unorm),
        (GL_SRGB8_ALPHA8, TextureFormat::Rgba8Srgb),
    ] {
        let mut c = ctx();
        let rbo = c.gen_renderbuffer();
        record::bind_renderbuffer(&mut c, GL_RENDERBUFFER, rbo);
        record::renderbuffer_storage(&mut c, GL_RENDERBUFFER, declared, 8, 8);
        let backing = c.renderbuffers.backing_tex(rbo);
        assert_eq!(
            c.textures.get(backing).map(|t| t.ir_format),
            Some(plane),
            "{declared:#x} renderbuffer plane"
        );
    }
}
