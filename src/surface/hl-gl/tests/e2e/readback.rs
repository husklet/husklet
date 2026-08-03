use super::*;

// ---------------------------------------------------------------------------------------------------
// glReadPixels: render the frame, then read the rendered default target back through the sink and assert
// the pixels — the GL device→host readback path end-to-end (render → CopyTextureToBuffer → read_buffer).
// ---------------------------------------------------------------------------------------------------

#[test]
fn glreadpixels_reads_the_rendered_default_target_back() {
    let mut c = GlContext::new();
    c.set_surface(GlSurface {
        have: true,
        width: W as u32,
        height: H as u32,
    });
    let mut sink = cpu_sink();

    record::clear_color(&mut c, [0.0, 0.0, 1.0, 1.0]); // blue background
    record::clear(&mut c);
    record_triangle(&mut c, [1.0, 0.0, 0.0, 1.0]); // red triangle
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    // glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, …) — the whole default (Bgra8) target, returned
    // converted to RGBA in GL's bottom-left row order.
    let px = readpixels::read_pixels(
        &mut c,
        &mut sink,
        0,
        0,
        W as i32,
        H as i32,
        readpixels::PixelFormat::new(GL_RGBA, GL_UNSIGNED_BYTE),
    )
    .expect("glReadPixels device->host readback");
    assert_eq!(px.len(), W * H * 4, "packed RGBA rectangle is w*h*4 bytes");

    // The centered triangle covers the center → red; a corner is the blue clear. (A vertical flip maps a
    // corner to a corner and the center to itself, so the symmetric assertions hold regardless of origin.)
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

    // The readback really executed a draw (not a skipped no-op), and every pixel is red-or-blue.
    assert_eq!(
        sink.executor().draws,
        1,
        "exactly one draw executed for the readback"
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
        "both drawn ({red}) and cleared ({blue}) pixels read back"
    );
    assert_eq!(
        red + blue,
        W * H,
        "every read-back pixel is the triangle or the clear color"
    );

    // glReadPixels is not a frame boundary: `eglSwapBuffers` after it must still post the frame the app
    // drew, and the frame must execute exactly once — the readback already rendered it, so the swap
    // presents the resident default target rather than replaying the draw-list.
    assert!(
        hl_gl::service::swap::swap_buffers(&mut c, &mut sink).unwrap(),
        "eglSwapBuffers after glReadPixels still presents the rendered frame"
    );
    assert_eq!(
        sink.executor().draws,
        1,
        "the frame executed once across the readback and the swap"
    );
    let target = c
        .resident_default_read_target()
        .expect("the readback left the default target resident")
        .0;
    let mut presented = vec![0u8; W * H * 4];
    sink.executor()
        .read_texture(sink.resources(), TextureId(target), &mut presented)
        .unwrap();
    assert_eq!(
        texel(&presented, W / 2, H / 2),
        [0, 0, 255, 255],
        "the presented default target still holds the rendered frame (Bgra8 red center)"
    );
}

#[test]
fn glreadpixels_of_a_subrectangle_in_bgra() {
    let mut c = GlContext::new();
    c.set_surface(GlSurface {
        have: true,
        width: W as u32,
        height: H as u32,
    });
    let mut sink = cpu_sink();

    record::clear_color(&mut c, [0.0, 0.0, 1.0, 1.0]);
    record::clear(&mut c);
    record_triangle(&mut c, [1.0, 0.0, 0.0, 1.0]);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    // A 2x2 rectangle around the center, read back as GL_BGRA_EXT (bytes [B, G, R, A]).
    let (rx, ry) = (W as i32 / 2 - 1, H as i32 / 2 - 1);
    let px = readpixels::read_pixels(
        &mut c,
        &mut sink,
        rx,
        ry,
        2,
        2,
        readpixels::PixelFormat::new(GL_BGRA_EXT, GL_UNSIGNED_BYTE),
    )
    .expect("glReadPixels sub-rectangle in BGRA");
    assert_eq!(px.len(), 2 * 2 * 4);
    // Red triangle in BGRA is [0, 0, 255, 255]; the center rectangle is fully covered.
    for t in px.chunks_exact(4) {
        assert_eq!(
            t,
            [0, 0, 255, 255],
            "the center sub-rect is the red triangle (BGRA)"
        );
    }
}
