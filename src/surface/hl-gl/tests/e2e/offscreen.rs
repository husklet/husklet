use super::*;

// ---------------------------------------------------------------------------------------------------
// offscreen FBO: render the same frame into an Rgba8 render-target texture and read it back
// ---------------------------------------------------------------------------------------------------

#[test]
fn clear_and_triangle_render_to_an_offscreen_rgba8_fbo() {
    let mut c = GlContext::new();
    c.surf = GlSurface {
        have: true,
        width: W as u32,
        height: H as u32,
    };
    let mut sink = cpu_sink();

    // An 8x8 Rgba8 offscreen color texture attached to a bound framebuffer.
    let tex = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d_format(
        &mut c,
        W as i32,
        H as i32,
        &[],
        hl_gpu::protocol::model::enums::TextureFormat::Rgba8Unorm,
    );
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

    record::clear_color(&mut c, [0.0, 0.0, 1.0, 1.0]); // blue background
    record::clear(&mut c);
    record_triangle(&mut c, [1.0, 0.0, 0.0, 1.0]); // red triangle
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    let target = present_frame(&mut c, &mut sink);

    // Read back the offscreen Rgba8Unorm render target (stored bytes are [R, G, B, A]).
    let mut px = vec![0u8; W * H * 4];
    sink.executor()
        .read_texture(sink.resources(), TextureId(target), &mut px)
        .unwrap();

    assert_eq!(
        texel(&px, W / 2, H / 2),
        [255, 0, 0, 255],
        "center is the red triangle (RGBA)"
    );
    assert_eq!(
        texel(&px, 0, 0),
        [0, 0, 255, 255],
        "top-left corner is the blue clear (RGBA)"
    );
    assert_eq!(
        sink.executor().draws,
        1,
        "exactly one draw executed into the FBO"
    );
}
