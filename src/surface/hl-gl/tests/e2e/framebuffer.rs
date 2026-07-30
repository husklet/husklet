use super::*;

// ---------------------------------------------------------------------------------------------------
// GUEST-DRIVEN offscreen FBO + glReadPixels: exercise the framebuffer object surface a guest actually
// drives — bind a framebuffer, attach a color texture through the newly-wired
// `record::framebuffer_texture_2d` (the body the shim's `glFramebufferTexture2D` symbol now calls),
// prove it is GL_FRAMEBUFFER_COMPLETE, render a triangle INTO the FBO, then read the FBO back with
// `glReadPixels`. This proves the offscreen path works through the shim-reachable FBO ops, not just the
// frame builder's direct `present`.
// ---------------------------------------------------------------------------------------------------

const GL_RGBA8: u32 = 0x8058;

#[test]
fn guest_bound_texture_fbo_renders_and_glreadpixels_reads_it_back() {
    let mut c = GlContext::new();
    c.set_surface(GlSurface {
        have: true,
        width: W as u32,
        height: H as u32,
    });
    let mut sink = cpu_sink();

    // Guest FBO bring-up: an 8x8 Rgba8 color texture attached to a freshly-bound framebuffer, driven
    // entirely through the wired framebuffer record ops.
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
    assert!(
        record::is_framebuffer(&c, fbo),
        "generated name is a framebuffer object"
    );
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        tex,
        0,
    );

    // The wired completeness check reports COMPLETE once a sized color texture is attached.
    assert_eq!(
        record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_COMPLETE,
        "a bound FBO with a sized color texture is framebuffer-complete",
    );

    // Render the frame WHILE the FBO is bound → the draw's render target is the FBO's color texture.
    record::clear_color(&mut c, [0.0, 0.0, 1.0, 1.0]); // blue background
    record::clear(&mut c);
    record_triangle(&mut c, [1.0, 0.0, 0.0, 1.0]); // red triangle
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    // glReadPixels reads back the RENDERED FBO target (Rgba8, bottom-left origin) — the offscreen path
    // end-to-end (render into the attachment → CopyTextureToBuffer → read_buffer).
    let px = readpixels::read_pixels(&mut c, &mut sink, 0, 0, W as i32, H as i32, GL_RGBA)
        .expect("glReadPixels of the offscreen FBO");
    assert_eq!(px.len(), W * H * 4, "packed RGBA rectangle is w*h*4 bytes");

    assert_eq!(
        read_texel(&px, W / 2, H / 2, W),
        [255, 0, 0, 255],
        "center is the red triangle (RGBA)"
    );
    assert_eq!(
        read_texel(&px, 0, 0, W),
        [0, 0, 255, 255],
        "a corner is the blue clear (RGBA)"
    );
    assert_eq!(
        sink.executor().draws,
        1,
        "exactly one draw executed into the FBO"
    );
    let red = px
        .chunks_exact(4)
        .filter(|t| *t == [255, 0, 0, 255])
        .count();
    let blue = px
        .chunks_exact(4)
        .filter(|t| *t == [0, 0, 255, 255])
        .count();
    assert!(
        red > 0 && blue > 0,
        "both drawn ({red}) and cleared ({blue}) FBO pixels read back"
    );
    assert_eq!(
        red + blue,
        W * H,
        "every read-back FBO pixel is the triangle or the clear color"
    );
}

#[test]
fn guest_renderbuffer_backed_fbo_renders_and_glreadpixels_reads_it_back() {
    let mut c = GlContext::new();
    c.set_surface(GlSurface {
        have: true,
        width: W as u32,
        height: H as u32,
    });
    let mut sink = cpu_sink();

    // A renderbuffer color attachment, attached to the FBO BEFORE its storage is defined (the common
    // "attach then size" ordering) — proving the renderbuffer's stable texture backing keeps the
    // attachment wired once storage lands.
    let rbo = record::gen_renderbuffer(&mut c);
    assert!(
        record::is_renderbuffer(&c, rbo),
        "generated name is a renderbuffer object"
    );
    record::bind_renderbuffer(&mut c, GL_RENDERBUFFER, rbo);

    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_renderbuffer(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_RENDERBUFFER,
        rbo,
    );
    // Attached but unsized → the color attachment is incomplete until storage is defined.
    assert_eq!(
        record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_INCOMPLETE_ATTACHMENT,
        "a renderbuffer attachment without storage is incomplete",
    );

    record::renderbuffer_storage(&mut c, GL_RENDERBUFFER, GL_RGBA8, W as i32, H as i32);
    assert_eq!(
        record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_COMPLETE,
        "defining storage on the attached renderbuffer completes the framebuffer",
    );

    record::clear_color(&mut c, [0.0, 0.0, 1.0, 1.0]); // blue background
    record::clear(&mut c);
    record_triangle(&mut c, [1.0, 0.0, 0.0, 1.0]); // red triangle
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    let px = readpixels::read_pixels(&mut c, &mut sink, 0, 0, W as i32, H as i32, GL_RGBA)
        .expect("glReadPixels of the renderbuffer-backed FBO");
    assert_eq!(
        read_texel(&px, W / 2, H / 2, W),
        [255, 0, 0, 255],
        "center is the red triangle (RGBA)"
    );
    assert_eq!(
        read_texel(&px, 0, 0, W),
        [0, 0, 255, 255],
        "a corner is the blue clear (RGBA)"
    );
    assert_eq!(
        sink.executor().draws,
        1,
        "exactly one draw executed into the renderbuffer FBO"
    );
}

#[test]
fn framebuffer_status_and_object_lifecycle_are_honest() {
    let mut c = GlContext::new();
    c.set_surface(GlSurface {
        have: true,
        width: W as u32,
        height: H as u32,
    });

    // The default framebuffer (name 0) is always complete; an unknown name is not a framebuffer.
    assert_eq!(
        record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_COMPLETE
    );
    assert!(
        !record::is_framebuffer(&c, 7),
        "an unminted name is not a framebuffer"
    );

    // A freshly-bound FBO with no attachment reports MISSING_ATTACHMENT.
    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    assert_eq!(
        record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT,
        "an FBO with no color attachment is missing its attachment",
    );

    // Deleting the bound FBO reverts to the default framebuffer (complete again) and drops the object.
    assert!(
        record::delete_framebuffer(&mut c, fbo),
        "deleting a live FBO reports success"
    );
    assert!(
        !record::is_framebuffer(&c, fbo),
        "a deleted FBO is no longer a framebuffer"
    );
    assert_eq!(
        c.bound_framebuffer(),
        0,
        "deleting the bound FBO reverts to the default framebuffer"
    );
    assert_eq!(
        record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_COMPLETE
    );
}
