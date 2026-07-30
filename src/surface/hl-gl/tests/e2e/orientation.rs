//! Row order of the presented default framebuffer, end to end through the CPU oracle.
//!
//! The convention under test is `service::frame::geometry::stores_bottom_up_rows`: an internal render
//! target — the default framebuffer included — stores rows TOP-DOWN, so the `wl_shm` plane
//! (`eglSwapBuffers` on the headless path) is upright with no flip, and `glReadPixels` applies exactly one
//! flip to report GL's bottom-left rows. The geometry is asymmetric in Y on purpose: a vertical mirror
//! moves it, so neither assertion can pass with the wrong number of flips.

use super::*;

/// Triangle confined to the UPPER half of clip space (`y` in `0.1..0.9`), i.e. the top of the window in GL
/// window coordinates, filled `color`.
fn upper_half_triangle(color: [f32; 4]) -> Vec<u8> {
    let mut bytes = Vec::new();
    for pos in [[-0.9f32, 0.1], [0.9, 0.1], [0.0, 0.9]] {
        bytes.extend(vertex(pos, color));
    }
    bytes
}

fn record_upper_half(c: &mut GlContext) {
    let vbo = c.buffers.gen();
    record::bind_buffer(c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(
        c,
        GL_ARRAY_BUFFER,
        &upper_half_triangle([1.0, 0.0, 0.0, 1.0]),
        0x88E4,
    );
    record::vertex_attrib_pointer(c, 0, 2, GL_FLOAT, false, 24, 0);
    record::enable_vertex_attrib(c, 0);
    let vs = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, vs, VS);
    record::compile_shader(c, vs);
    let fs = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, fs, FS);
    record::compile_shader(c, fs);
    let prog = record::create_program(c);
    record::attach_shader(c, prog, vs);
    record::attach_shader(c, prog, fs);
    assert!(record::link_program(c, prog));
    record::use_program(c, prog);
}

fn window_context() -> GlContext {
    let mut c = GlContext::new();
    c.set_surface(GlSurface {
        have: true,
        width: W as u32,
        height: H as u32,
    });
    c
}

fn rows_containing_red(plane: &[u8], red: [u8; 4]) -> Vec<usize> {
    (0..H)
        .filter(|y| (0..W).any(|x| texel(plane, x, *y) == red))
        .collect()
}

#[test]
fn presented_shm_plane_keeps_the_rendered_top_in_its_first_rows() {
    let mut c = window_context();
    let mut sink = cpu_sink();
    record::clear_color(&mut c, [0.0, 0.0, 1.0, 1.0]);
    record::clear(&mut c);
    record_upper_half(&mut c);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    let plane = readpixels::swap_xrgb(&mut c, &mut sink, W as i32, H as i32)
        .expect("present the window frame")
        .expect("an XRGB8888 plane for the wl_shm buffer");

    assert_eq!(plane.len(), W * H * 4);
    // XRGB8888 little-endian is [B, G, R, X]; the triangle is red, the clear is blue.
    let rows = rows_containing_red(&plane, [0x00, 0x00, 0xff, 0xff]);
    assert!(!rows.is_empty(), "the triangle must reach the shm buffer");
    assert!(
        rows.iter().all(|row| *row < H / 2),
        "a wl_shm buffer is top-down, so geometry drawn at the top of the GL window must land in the \
         plane's FIRST rows; found rows {rows:?}"
    );
    assert_eq!(
        texel(&plane, 0, H - 1),
        [0xff, 0x00, 0x00, 0xff],
        "the bottom row is the blue clear"
    );
}

#[test]
fn glreadpixels_reports_the_rendered_top_in_its_last_rows() {
    let mut c = window_context();
    let mut sink = cpu_sink();
    record::clear_color(&mut c, [0.0, 0.0, 1.0, 1.0]);
    record::clear(&mut c);
    record_upper_half(&mut c);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    let px = readpixels::read_pixels(&mut c, &mut sink, 0, 0, W as i32, H as i32, GL_RGBA)
        .expect("glReadPixels device->host readback");

    let rows: Vec<usize> = (0..H)
        .filter(|y| (0..W).any(|x| read_texel(&px, x, *y, W) == [255, 0, 0, 255]))
        .collect();
    assert!(!rows.is_empty(), "the triangle must be read back");
    assert!(
        rows.iter().all(|row| *row >= H / 2),
        "glReadPixels returns rows bottom-up, so geometry drawn at the top of the GL window must land in \
         the LAST rows of the packed plane; found rows {rows:?}"
    );
    assert_eq!(
        read_texel(&px, 0, 0, W),
        [0, 0, 255, 255],
        "GL scanline 0 is the bottom of the window — the blue clear"
    );
}

/// GL specifies that `glClear` is scissor-tested. A frame whose only recorded operations are clears must
/// still honor that: the scissored clear paints its sub-rect and nothing else.
#[test]
fn a_scissored_clear_without_geometry_paints_only_its_rectangle() {
    let mut c = window_context();
    let mut sink = cpu_sink();
    record::clear_color(&mut c, [0.0, 0.0, 1.0, 1.0]);
    record::clear(&mut c);
    // GL window rect (0, 0, W, 2) — the BOTTOM two scanlines.
    record::scissor(&mut c, [0, 0, W as i32, 2]);
    record::enable(&mut c, GL_SCISSOR_TEST);
    record::clear_color(&mut c, [0.0, 1.0, 0.0, 1.0]);
    record::clear(&mut c);

    let plane = readpixels::swap_xrgb(&mut c, &mut sink, W as i32, H as i32)
        .expect("present the window frame")
        .expect("an XRGB8888 plane for the wl_shm buffer");

    // Top-down plane: the GL-bottom scissor rect is its LAST two rows.
    for y in 0..H {
        let expected = if y >= H - 2 {
            [0x00, 0xff, 0x00, 0xff]
        } else {
            [0xff, 0x00, 0x00, 0xff]
        };
        assert_eq!(
            texel(&plane, W / 2, y),
            expected,
            "row {y} of the presented plane"
        );
    }
}
