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
    let px = readpixels::read_pixels(&mut c, &mut sink, 0, 0, W as i32, H as i32, readpixels::PixelFormat::new(GL_RGBA, GL_UNSIGNED_BYTE))
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

    let px = readpixels::read_pixels(&mut c, &mut sink, 0, 0, W as i32, H as i32, readpixels::PixelFormat::new(GL_RGBA, GL_UNSIGNED_BYTE))
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

/// A NARROW colour attachment reads back the channel it holds, not the neighbouring texels' bytes.
///
/// `GL_R8` is colour-renderable by ES 3.0 table 3.13 and this driver has always accepted it, so this is a
/// live defect with no extension involved. The render target's plane is one byte a texel — the executor
/// derives the readback copy's tight row from the texture's own format — while this path asked for a row
/// of `width * 4` and then read texels at four bytes. The copy therefore landed one byte of every four
/// with padding between, and `glReadPixels` returned the R channel of four ADJACENT texels as one RGBA
/// pixel: an 8x8 R8 target cleared to red read back pure WHITE, `[255, 255, 255, 255]`, at every pixel.
///
/// The RGBA8 arm is the control. It is the case that already worked, it must keep working byte for byte,
/// and without it a readback that returned red for everything would pass the R8 assertion alone.
#[test]
fn glreadpixels_of_a_narrow_colour_attachment_reads_its_own_channel() {
    for (declared, plane, expected) in [
        (
            GL_R8,
            hl_gpu::protocol::model::enums::TextureFormat::R8Unorm,
            // A one-channel target: green and blue read as zero, alpha as one.
            [255u8, 0, 0, 255],
        ),
        (
            GL_RGBA8,
            hl_gpu::protocol::model::enums::TextureFormat::Rgba8Unorm,
            [255, 0, 0, 255],
        ),
    ] {
        let mut c = GlContext::new();
        c.set_surface(GlSurface {
            have: true,
            width: W as u32,
            height: H as u32,
        });
        let mut sink = cpu_sink();

        let tex = c.textures.gen();
        c.active_texture(GL_TEXTURE0);
        record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
        record::tex_image_2d_declared(&mut c, declared, W as i32, H as i32, &[]);
        assert_eq!(
            c.textures.get(tex).map(|t| t.ir_format),
            Some(plane),
            "{declared:#x} allocates the plane it declared"
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
            record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
            GL_FRAMEBUFFER_COMPLETE,
            "{declared:#x} is a renderable colour attachment"
        );

        record::clear_color(&mut c, [1.0, 0.0, 0.0, 1.0]);
        record::clear(&mut c);
        let px = readpixels::read_pixels(&mut c, &mut sink, 0, 0, W as i32, H as i32, readpixels::PixelFormat::new(GL_RGBA, GL_UNSIGNED_BYTE))
            .expect("glReadPixels of the colour attachment");

        assert_eq!(px.len(), W * H * 4, "the PACKED rectangle is always w*h*4");
        // Report the FIRST pixel that disagrees, not pixel zero. A wrong stride is often right at the
        // origin and wrong afterwards, so printing `px[..4]` shows a matching pixel beside a failure
        // and reads as though the assertion itself were broken.
        let wrong = px
            .chunks_exact(4)
            .enumerate()
            .find(|(_, texel)| *texel != expected);
        assert!(
            wrong.is_none(),
            "{declared:#x} must read back {expected:?} at every pixel; pixel {} is {:?}",
            wrong.map_or(0, |(index, _)| index),
            wrong.map(|(_, texel)| texel),
        );
    }
}

/// A FLOAT colour buffer, end to end: allocate the plane, clear it, and read it back.
///
/// This is the case the float work exists for, and it needed five separate paths to be right before it
/// could run at all. Four were allocation and upload; the fifth was the clear, which refused every float
/// format on BOTH executors while `capability::COLOR_FORMATS` had listed all three as formats every
/// backend materializes. The shape of that one was the dangerous part: an unscissored clear is normally
/// folded into the render pass load op, so clear-plus-draw worked and clear-alone did not — and a
/// clear-only frame is exactly what a test harness or a screenshot produces.
///
/// The RGBA8 arm is the control. R32Float is here because a single-channel float target is the case that
/// separates "reads the plane" from "reads four bytes and calls them a pixel": it must report its one
/// channel and leave green and blue at zero.
#[test]
fn a_float_colour_buffer_clears_and_reads_back() {
    use hl_gpu::protocol::model::enums::TextureFormat;

    for (plane, expected) in [
        (TextureFormat::Rgba16Float, [255u8, 128, 0, 255]),
        (TextureFormat::Rgba32Float, [255, 128, 0, 255]),
        // One channel: the green the clear carried is not storable and must not be invented.
        (TextureFormat::R32Float, [255, 0, 0, 255]),
        (TextureFormat::Rgba8Unorm, [255, 128, 0, 255]),
    ] {
        let mut c = GlContext::new();
        c.set_surface(GlSurface {
            have: true,
            width: W as u32,
            height: H as u32,
        });
        let mut sink = cpu_sink();

        let tex = c.textures.gen();
        c.active_texture(GL_TEXTURE0);
        record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
        record::tex_image_2d_format(&mut c, W as i32, H as i32, &[], plane);
        assert_eq!(
            c.textures.get(tex).map(|t| t.data.len()),
            Some(W * H * plane.bytes_per_texel().expect("a colour plane")),
            "{plane:?} allocates a plane sized by its own texel"
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
            record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
            GL_FRAMEBUFFER_COMPLETE,
            "{plane:?} is a complete colour attachment"
        );

        // A clear with NO draw after it: the frame lowers to a clear-only submit, which is the path that
        // becomes an `Enc::ClearRect` rather than a render-pass load op.
        record::clear_color(&mut c, [1.0, 0.5, 0.0, 1.0]);
        record::clear(&mut c);
        let px = readpixels::read_pixels(&mut c, &mut sink, 0, 0, W as i32, H as i32, readpixels::PixelFormat::new(GL_RGBA, GL_UNSIGNED_BYTE))
            .unwrap_or_else(|error| panic!("{plane:?} clear-only readback failed: {error:?}"));

        let wrong = px
            .chunks_exact(4)
            .enumerate()
            .find(|(_, texel)| *texel != expected);
        assert!(
            wrong.is_none(),
            "{plane:?} must read back {expected:?} at every pixel; pixel {} is {:?}",
            wrong.map_or(0, |(index, _)| index),
            wrong.map(|(_, texel)| texel),
        );
    }
}

/// A float colour buffer read back through `GL_RGBA`/`GL_FLOAT` returns the values it holds, unclamped.
///
/// This is the pair ES 3.0 §4.3.1 makes always-acceptable for a floating-point colour buffer, and it is
/// the readback a conformant application performs on one. It used to be refused outright — the shim
/// accepted no type but `GL_UNSIGNED_BYTE` — so the extension would have advertised a renderable float
/// framebuffer whose defining property could not be observed through the call designed to observe it.
///
/// Driven at a value ABOVE one on purpose. Every clear colour inside `0..=1` reads back the same through
/// both types, so a test at 0.5 would pass against the byte-only path and prove nothing; 4.0 comes back as
/// 4.0 through `GL_FLOAT` and saturates to 255 through `GL_UNSIGNED_BYTE`, and the byte arm is kept here
/// as the control showing exactly what the float pair buys.
#[test]
fn a_float_colour_buffer_reads_back_unclamped_through_the_float_pair() {
    use hl_gpu::protocol::model::enums::TextureFormat;
    use hl_gl::service::readpixels::PixelFormat;

    const GL_FLOAT: u32 = 0x1406;

    let mut c = GlContext::new();
    c.set_surface(GlSurface {
        have: true,
        width: W as u32,
        height: H as u32,
    });
    let mut sink = cpu_sink();

    let tex = c.textures.gen();
    c.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d_format(&mut c, W as i32, H as i32, &[], TextureFormat::Rgba16Float);
    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_texture_2d(&mut c, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);

    // The read framebuffer is now floating-point, so the driver must say so — this is what tells an
    // application which pair to use, and it answered GL_UNSIGNED_BYTE for every buffer before.
    assert!(
        c.read_colour_buffer_is_float(),
        "a half-float colour attachment makes the read buffer floating-point"
    );

    record::clear_color(&mut c, [4.0, -2.5, 0.5, 1.0]);
    record::clear(&mut c);

    let floats = readpixels::read_pixels(
        &mut c,
        &mut sink,
        0,
        0,
        W as i32,
        H as i32,
        PixelFormat::new(GL_RGBA, GL_FLOAT),
    )
    .expect("a float colour buffer must be readable through GL_RGBA/GL_FLOAT");
    assert_eq!(
        floats.len(),
        W * H * 16,
        "four 32-bit channels a pixel, not four bytes"
    );
    let texel: Vec<f32> = floats[..16]
        .chunks_exact(4)
        .map(|b| f32::from_le_bytes(b.try_into().expect("four bytes")))
        .collect();
    assert_eq!(
        texel,
        vec![4.0, -2.5, 0.5, 1.0],
        "the values the buffer holds, above one and below zero, come back as they are"
    );
}

/// A draw into a NARROW and into a FLOAT colour target, end to end through the software reference.
///
/// The reference used to refuse every draw whose target was not a four-channel eight-bit plane, which is
/// why a `GL_R8` colour attachment reading back pure white survived a green suite: no test in the
/// repository, and no program in the executor differential, could render into one. The refusal was the
/// reference's own limit rather than the executor's, so it read as "unsupported" while the driver
/// happily shipped the format.
///
/// A replace draw needs nothing a narrow or float plane lacks — it writes the fragment through the same
/// packing rule the clear uses — so what remains refused is blending and channel masking, which read the
/// destination back as normalized RGBA and genuinely have no reading here.
///
/// The result names its own correctness: the blue clear survives in the corner of a four-channel target
/// and is GONE from the one- and two-channel ones, because those planes have no blue to store it in. A
/// stride bug cannot produce that pattern, and neither can a reference that quietly wrote four channels
/// into a one-channel plane.
#[test]
fn the_reference_draws_into_narrow_and_float_colour_targets() {
    use hl_gpu::protocol::model::enums::TextureFormat;

    for (plane, corner) in [
        // One and two channels: the blue clear has nowhere to live.
        (TextureFormat::R8Unorm, [0u8, 0, 0, 255]),
        (TextureFormat::Rg8Unorm, [0, 0, 0, 255]),
        // Four channels, so the clear survives where the triangle does not cover.
        (TextureFormat::Rgba16Float, [0, 0, 255, 255]),
        (TextureFormat::Rgba8Unorm, [0, 0, 255, 255]),
    ] {
        let mut c = GlContext::new();
        c.set_surface(GlSurface {
            have: true,
            width: W as u32,
            height: H as u32,
        });
        let mut sink = cpu_sink();

        let tex = c.textures.gen();
        c.active_texture(GL_TEXTURE0);
        record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
        record::tex_image_2d_format(&mut c, W as i32, H as i32, &[], plane);
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

        record::clear_color(&mut c, [0.0, 0.0, 1.0, 1.0]); // blue
        record::clear(&mut c);
        record_triangle(&mut c, [1.0, 0.0, 0.0, 1.0]); // red
        record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

        let px = readpixels::read_pixels(
            &mut c,
            &mut sink,
            0,
            0,
            W as i32,
            H as i32,
            hl_gl::service::readpixels::PixelFormat::new(GL_RGBA, GL_UNSIGNED_BYTE),
        )
        .unwrap_or_else(|error| panic!("{plane:?} draw+readback failed: {error:?}"));

        assert_eq!(
            sink.executor().draws,
            1,
            "{plane:?} must actually execute the draw, not skip it"
        );
        assert_eq!(
            read_texel(&px, W / 2, H / 2, W),
            [255, 0, 0, 255],
            "{plane:?} centre is the red triangle — every one of these planes has a red channel"
        );
        assert_eq!(
            read_texel(&px, 0, 0, W),
            corner,
            "{plane:?} corner is the clear, in the channels this plane actually has"
        );
    }
}
