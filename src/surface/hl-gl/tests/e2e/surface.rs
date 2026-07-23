use super::*;

// ---------------------------------------------------------------------------------------------------
// default framebuffer: clear to blue, draw a red triangle, read back the presented Bgra8 target
// ---------------------------------------------------------------------------------------------------

#[test]
fn clear_and_triangle_render_to_the_default_surface() {
    let mut c = GlContext::new();
    c.surf = GlSurface {
        have: true,
        width: W as u32,
        height: H as u32,
    };
    let mut sink = cpu_sink();

    record::clear_color(&mut c, [0.0, 0.0, 1.0, 1.0]); // blue background
    record::clear(&mut c);
    record_triangle(&mut c, [1.0, 0.0, 0.0, 1.0]); // red triangle
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    let target = present_frame(&mut c, &mut sink);

    // Read back the rendered Bgra8Unorm target (stored bytes are [B, G, R, A]).
    let mut px = vec![0u8; W * H * 4];
    sink.executor()
        .read_texture(sink.resources(), TextureId(target), &mut px)
        .unwrap();

    // The triangle covers the center → red; a corner is untouched → the blue clear.
    assert_eq!(
        texel(&px, W / 2, H / 2),
        [0, 0, 255, 255],
        "center is the red triangle (BGRA)"
    );
    assert_eq!(
        texel(&px, 0, 0),
        [255, 0, 0, 255],
        "top-left corner is the blue clear (BGRA)"
    );

    // The draw really reached the rasterizer (not a silently-skipped no-op), and some pixels are red.
    assert_eq!(sink.executor().draws, 1, "exactly one draw executed");
    let red = px
        .chunks_exact(4)
        .filter(|t| *t == [0, 0, 255, 255])
        .count();
    let blue = px
        .chunks_exact(4)
        .filter(|t| *t == [255, 0, 0, 255])
        .count();
    assert!(
        red > 0 && blue > 0,
        "both drawn ({red}) and cleared ({blue}) pixels present"
    );
    assert_eq!(
        red + blue,
        W * H,
        "every pixel is either the triangle or the clear color"
    );
}
