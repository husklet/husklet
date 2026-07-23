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
    let fbo = c.framebuffers.gen();
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
