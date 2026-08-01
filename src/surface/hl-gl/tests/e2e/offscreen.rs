use super::*;

// ---------------------------------------------------------------------------------------------------
// offscreen FBO: render the same frame into an Rgba8 render-target texture and read it back
// ---------------------------------------------------------------------------------------------------

#[test]
fn clear_and_triangle_render_to_an_offscreen_rgba8_fbo() {
    let mut c = GlContext::new();
    c.set_surface(GlSurface {
        have: true,
        width: W as u32,
        height: H as u32,
    });
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

#[test]
fn glreadpixels_preserves_an_offscreen_fbo_across_flush() {
    let mut context = GlContext::new();
    context.set_surface(GlSurface {
        have: true,
        width: W as u32,
        height: H as u32,
    });
    let mut sink = cpu_sink();
    let texture = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, texture);
    record::tex_image_2d_format(
        &mut context,
        W as i32,
        H as i32,
        &[],
        hl_gpu::protocol::model::enums::TextureFormat::Rgba8Unorm,
    );
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        texture,
        0,
    );
    record::clear_color(
        &mut context,
        [17.0 / 255.0, 34.0 / 255.0, 51.0 / 255.0, 1.0],
    );
    record::clear(&mut context);
    record::bind_framebuffer(&mut context, GL_DRAW_FRAMEBUFFER, 0);
    assert_eq!(context.bound_framebuffer(), 0);
    assert_eq!(context.read_framebuffer(), framebuffer);

    assert!(hl_gl::service::swap::flush(&mut context, &mut sink).unwrap());
    assert!(
        context.draws().is_empty(),
        "flush consumes the offscreen work"
    );

    let pixels = readpixels::read_pixels(&mut context, &mut sink, 2, 2, 1, 1, readpixels::PixelFormat::new(GL_RGBA, GL_UNSIGNED_BYTE))
        .expect("read the persistent target rendered before glFlush");
    assert_eq!(pixels, [17, 34, 51, 255]);
}

#[test]
fn glreadpixels_uses_read_fbo_while_unrelated_draw_fbo_work_is_pending() {
    let mut context = GlContext::new();
    context.set_surface(GlSurface {
        have: true,
        width: W as u32,
        height: H as u32,
    });
    let mut sink = cpu_sink();

    let read_texture = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, read_texture);
    record::tex_image_2d_format(
        &mut context,
        W as i32,
        H as i32,
        &[],
        hl_gpu::protocol::model::enums::TextureFormat::Rgba8Unorm,
    );
    let read_framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, read_framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        read_texture,
        0,
    );
    record::clear_color(
        &mut context,
        [17.0 / 255.0, 34.0 / 255.0, 51.0 / 255.0, 1.0],
    );
    record::clear(&mut context);
    assert!(hl_gl::service::swap::flush(&mut context, &mut sink).unwrap());

    let draw_texture = context.textures.gen();
    record::bind_texture(&mut context, GL_TEXTURE_2D, draw_texture);
    record::tex_image_2d_format(
        &mut context,
        W as i32,
        H as i32,
        &[],
        hl_gpu::protocol::model::enums::TextureFormat::Rgba8Unorm,
    );
    let draw_framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_DRAW_FRAMEBUFFER, draw_framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_DRAW_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        draw_texture,
        0,
    );
    record::clear_color(&mut context, [1.0, 0.0, 0.0, 1.0]);
    record::clear(&mut context);
    assert_eq!(context.read_framebuffer(), read_framebuffer);
    assert_eq!(context.bound_framebuffer(), draw_framebuffer);
    assert!(!context.draws().is_empty());

    let pixels = readpixels::read_pixels(&mut context, &mut sink, 2, 2, 1, 1, readpixels::PixelFormat::new(GL_RGBA, GL_UNSIGNED_BYTE))
        .expect("read the selected read FBO, not the pending draw FBO");
    assert_eq!(pixels, [17, 34, 51, 255]);
}
