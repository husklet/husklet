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

#[test]
fn depth_only_framebuffer_completeness_uses_its_attachment_format() {
    let mut c = ctx();
    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    let depth = record::gen_renderbuffer(&mut c);
    record::bind_renderbuffer(&mut c, GL_RENDERBUFFER, depth);
    record::renderbuffer_storage(&mut c, GL_RENDERBUFFER, GL_DEPTH_COMPONENT16, 64, 64);
    record::framebuffer_renderbuffer(
        &mut c,
        GL_FRAMEBUFFER,
        GL_DEPTH_ATTACHMENT,
        GL_RENDERBUFFER,
        depth,
    );
    assert_eq!(
        record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_COMPLETE
    );

    record::renderbuffer_storage(&mut c, GL_RENDERBUFFER, GL_RGBA4, 64, 64);
    assert_eq!(
        record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT
    );
}

#[test]
fn framebuffer_with_distinct_attachment_sizes_is_incomplete() {
    let mut c = ctx();
    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);

    let color = record::gen_renderbuffer(&mut c);
    record::bind_renderbuffer(&mut c, GL_RENDERBUFFER, color);
    record::renderbuffer_storage(&mut c, GL_RENDERBUFFER, GL_RGBA4, 64, 64);
    record::framebuffer_renderbuffer(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_RENDERBUFFER,
        color,
    );

    let depth = record::gen_renderbuffer(&mut c);
    record::bind_renderbuffer(&mut c, GL_RENDERBUFFER, depth);
    record::renderbuffer_storage(&mut c, GL_RENDERBUFFER, GL_DEPTH_COMPONENT16, 128, 128);
    record::framebuffer_renderbuffer(
        &mut c,
        GL_FRAMEBUFFER,
        GL_DEPTH_ATTACHMENT,
        GL_RENDERBUFFER,
        depth,
    );
    assert_eq!(
        record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_INCOMPLETE_DIMENSIONS
    );
}

#[test]
fn gles2_completeness_uses_only_gles2_attachment_formats() {
    let mut c = ctx();
    let mut es2 = hl_gl::model::context::ContextState::with_version(2, 0, false);
    c.switch_state(&mut es2);
    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);

    let texture = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, texture);
    record::tex_image_2d_declared(&mut c, GL_R8, 64, 64, &[]);
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
        GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT
    );

    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        0,
        0,
    );
    let packed = record::gen_renderbuffer(&mut c);
    record::bind_renderbuffer(&mut c, GL_RENDERBUFFER, packed);
    record::renderbuffer_storage(&mut c, GL_RENDERBUFFER, GL_DEPTH24_STENCIL8, 64, 64);
    record::framebuffer_renderbuffer(
        &mut c,
        GL_FRAMEBUFFER,
        GL_DEPTH_ATTACHMENT,
        GL_RENDERBUFFER,
        packed,
    );
    assert_eq!(
        record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT
    );
}

#[test]
fn gles2_texture_images_reject_unadvertised_format_type_pairs() {
    let mut c = ctx();
    let mut es2 = hl_gl::model::context::ContextState::with_version(2, 0, false);
    c.switch_state(&mut es2);
    assert!(!record::validate_tex_image_2d(
        &mut c,
        GL_RGB,
        GL_RGB,
        GL_FLOAT
    ));
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    assert!(record::validate_tex_image_2d(
        &mut c,
        GL_RGB,
        GL_RGB,
        GL_UNSIGNED_BYTE
    ));
    assert!(record::validate_tex_image_2d(
        &mut c,
        GL_BGRA8_EXT,
        GL_BGRA_EXT,
        GL_UNSIGNED_BYTE
    ));
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
    // ES 3.0 §2.8.3: a draw into an incomplete framebuffer is GL_INVALID_FRAMEBUFFER_OPERATION and
    // records nothing. This asserted GL_NO_ERROR and a recorded draw, so an application that checks
    // glGetError to learn whether its framebuffer is usable was told that it was.
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert_eq!(c.take_gl_error(), GL_INVALID_FRAMEBUFFER_OPERATION);
    assert!(c.draws().is_empty());

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

fn depth_framebuffer(c: &mut GlContext, format: u32, samples: i32) -> u32 {
    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(c, GL_FRAMEBUFFER, fbo);
    let storage = record::gen_renderbuffer(c);
    record::bind_renderbuffer(c, GL_RENDERBUFFER, storage);
    record::renderbuffer_storage_multisample(c, GL_RENDERBUFFER, samples, format, 8, 8);
    let attachment = if format == GL_STENCIL_INDEX8 {
        GL_STENCIL_ATTACHMENT
    } else if format == GL_DEPTH24_STENCIL8 {
        GL_DEPTH_STENCIL_ATTACHMENT
    } else {
        GL_DEPTH_ATTACHMENT
    };
    record::framebuffer_renderbuffer(c, GL_FRAMEBUFFER, attachment, GL_RENDERBUFFER, storage);
    fbo
}

#[test]
fn depth_stencil_blit_rejects_linear_incompatible_formats_and_multisample_scaling() {
    let mut c = ctx();
    let depth16 = depth_framebuffer(&mut c, GL_DEPTH_COMPONENT16, 0);
    let depth24 = depth_framebuffer(&mut c, GL_DEPTH_COMPONENT24, 0);
    record::bind_framebuffer(&mut c, GL_READ_FRAMEBUFFER, depth16);
    record::bind_framebuffer(&mut c, GL_DRAW_FRAMEBUFFER, depth24);
    record::blit_framebuffer(
        &mut c, 0, 0, 8, 8, 0, 0, 8, 8, GL_DEPTH_BUFFER_BIT, GL_NEAREST,
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);

    record::bind_framebuffer(&mut c, GL_DRAW_FRAMEBUFFER, depth16);
    record::blit_framebuffer(
        &mut c, 0, 0, 8, 8, 0, 0, 8, 8, GL_DEPTH_BUFFER_BIT, GL_LINEAR,
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);

    let multisampled = depth_framebuffer(&mut c, GL_DEPTH_COMPONENT16, 4);
    record::bind_framebuffer(&mut c, GL_READ_FRAMEBUFFER, multisampled);
    record::bind_framebuffer(&mut c, GL_DRAW_FRAMEBUFFER, depth16);
    record::blit_framebuffer(
        &mut c, 0, 0, 8, 8, 0, 0, 4, 4, GL_DEPTH_BUFFER_BIT, GL_NEAREST,
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_OPERATION);
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
