use super::*;

// ---------------------------------------------------------------------------------------------------
// default render target is Bgra8Unorm; an offscreen FBO renders into its attachment's texture + format
// ---------------------------------------------------------------------------------------------------

#[test]
fn default_target_is_bgra8() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    record::clear(&mut c);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let (_id, d) = render_target_desc(&sink.batches[0]);
    assert_eq!(d.format, TextureFormat::Bgra8Unorm);
    assert_eq!((d.width, d.height), (64, 64));
}

#[test]
fn offscreen_fbo_renders_into_the_attachment_texture_and_format() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();

    // A 32x32 Rgba8 offscreen color texture, attached to a framebuffer that we bind before drawing.
    let tex = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d_format(&mut c, 32, 32, &[], TextureFormat::Rgba8Unorm);
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

    flat_program(&mut c);
    tri_vbo(&mut c, 8);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];

    // The render target is sized + formatted from the FBO's color attachment, not the window surface.
    let (rt_id, d) = render_target_desc(batch);
    assert_eq!(
        d.format,
        TextureFormat::Rgba8Unorm,
        "offscreen target inherits the attachment format"
    );
    assert_eq!(
        (d.width, d.height),
        (32, 32),
        "offscreen target sized to the attachment"
    );
    assert_eq!(d.label, "offscreen-fbo");

    // No default window target was created, and the frame presents the offscreen render target.
    assert!(!batch
        .iter()
        .any(|c| matches!(c, Cmd::CreateTexture(_, dd) if dd.label == "default-fbo")));
    match batch.last().unwrap() {
        Cmd::Present { texture, .. } => assert_eq!(*texture, rt_id),
        other => panic!("expected Present of the offscreen target, got {other:?}"),
    }
}

#[test]
fn rebinding_default_framebuffer_returns_to_the_window_target() {
    // Binding FBO 0 restores the default framebuffer, so the next frame targets the window surface again.
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let fbo = c.framebuffers.gen();
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, 0); // back to the default framebuffer
    record::clear(&mut c);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let (_id, d) = render_target_desc(&sink.batches[0]);
    assert_eq!(d.label, "default-fbo");
    assert_eq!(d.format, TextureFormat::Bgra8Unorm);
}
