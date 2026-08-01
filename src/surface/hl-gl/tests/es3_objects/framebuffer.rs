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
