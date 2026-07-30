use super::*;

#[test]
fn surfaceless_default_framebuffer_is_incomplete_but_user_fbo_works() {
    let mut c = ctx();
    c.set_surface_available(false);

    assert_eq!(
        record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT
    );

    let texture = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, texture);
    record::tex_image_2d(&mut c, 4, 4, &[0; 64]);
    let framebuffer = c.gen_framebuffer();
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        texture,
        0,
    );

    assert_eq!(
        record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_COMPLETE
    );
}

// ===================================================================================================
// draw with no program / incomplete framebuffer / mismatched attachment → GL error, no panic
// ===================================================================================================

/// An incomplete framebuffer reports the right completeness status, a blit against it is
/// `GL_INVALID_FRAMEBUFFER_OPERATION`, and a draw against it merely records (no panic).
#[test]
fn incomplete_framebuffer_blit_and_draw_are_safe() {
    let mut c = ctx();
    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    // No color attachment yet → INCOMPLETE_MISSING_ATTACHMENT.
    assert_eq!(
        record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT
    );
    // A blit sourcing/targeting the incomplete FBO raises GL_INVALID_FRAMEBUFFER_OPERATION, no panic.
    record::blit_framebuffer(
        &mut c,
        0,
        0,
        4,
        4,
        0,
        0,
        4,
        4,
        GL_COLOR_BUFFER_BIT,
        GL_NEAREST,
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_FRAMEBUFFER_OPERATION);
    // Drawing with no program + an incomplete FBO bound just records the draw (dropped at lowering).
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.draws().len(), 1);

    // Attaching a real texture makes it complete.
    let tex = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(&mut c, 4, 4, &[0u8; 64]);
    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        tex,
        0,
    );
    assert_eq!(
        record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_COMPLETE
    );
}

/// `glFramebufferTexture2D` with a bad `textarget` / unmodeled attachment is `GL_INVALID_VALUE`; with a
/// dangling texture name it is `GL_INVALID_OPERATION`; a valid attach then succeeds.
#[test]
fn framebuffer_texture_2d_bad_attachment_and_dangling_texture() {
    let mut c = ctx();
    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    // A non-2D textarget is GL_INVALID_VALUE.
    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_3D,
        0,
        0,
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    // A dangling texture name is GL_INVALID_OPERATION.
    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        4242,
        0,
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);

    // A valid attach succeeds.
    let tex = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(&mut c, 4, 4, &[0u8; 64]);
    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        tex,
        0,
    );
    assert_eq!(c.take_gl_error(), GL_NO_ERROR);
    assert_eq!(c.framebuffer_color_attachment(fbo), tex);
}
