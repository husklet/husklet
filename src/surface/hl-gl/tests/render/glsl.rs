// ===================================================================================================
// real render — drive a corpus shader all the way through record → build_frame_ir → the in-process
// CpuExecutor → texture readback, and assert an EXACT pixel. The reference rasterizer draws coverage from
// the vertex buffer (position @0, straight-alpha RGBA @8) and treats the bound shader as an opaque module,
// so this proves the whole seam (link → frame → submit → execute → read) for the shaders whose geometry it
// can rasterize — a real render, not a compile. (Shader-heavy entries prove out via the naga compile above.)
// ===================================================================================================
use super::*;
use hl_gl::service::{readpixels, record};
use hl_gpu::protocol::model::capability::shader_payload;
use hl_gpu::{
    CpuExecutor, FakeClock, GlobalLedger, GpuExecutor, InProcessCommandSink, Limits, Session,
};

const W: usize = 8;
const H: usize = 8;

fn cpu_sink() -> InProcessCommandSink<CpuExecutor> {
    let exec = CpuExecutor::new();
    let mut limits = Limits::from_capabilities(exec.capabilities());
    limits.copy_alignment = 1;
    limits.caps.shader_payloads |= shader_payload::MSL | shader_payload::GLSL;
    let session = Session::new(
        limits,
        GlobalLedger::unbounded(),
        Box::new(FakeClock::new(0)),
    );
    InProcessCommandSink::with_session(session, exec)
}

fn vertex(pos: [f32; 2], color: [f32; 4]) -> Vec<u8> {
    let mut v = Vec::with_capacity(24);
    for f in pos.iter().chain(color.iter()) {
        v.extend_from_slice(&f.to_le_bytes());
    }
    v
}

/// Link a corpus vertex+fragment pair through the real shim path, bind a colored centered triangle, and
/// read back the presented default target. Returns the packed RGBA plane (bottom-left origin).
fn render_triangle(vs: &str, fs: &str, color: [f32; 4]) -> Vec<u8> {
    let mut c = GlContext::new();
    c.set_surface(GlSurface {
        have: true,
        width: W as u32,
        height: H as u32,
    });
    let mut sink = cpu_sink();

    record::clear_color(&mut c, [0.0, 0.0, 1.0, 1.0]); // blue background
    record::clear(&mut c);

    let vbo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    let mut tri = Vec::new();
    for pos in [[-0.8f32, -0.8], [0.8, -0.8], [0.0, 0.8]] {
        tri.extend(vertex(pos, color));
    }
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &tri, 0x88E4);
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 24, 0);
    record::enable_vertex_attrib(&mut c, 0);

    let vso = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vso, vs);
    record::compile_shader(&mut c, vso);
    let fso = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fso, fs);
    record::compile_shader(&mut c, fso);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vso);
    record::attach_shader(&mut c, prog, fso);
    assert!(record::link_program(&mut c, prog), "corpus shader links");
    record::use_program(&mut c, prog);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    // glReadPixels drives the full render + device→host readback (build_frame_ir → submit → execute).
    let px = readpixels::read_pixels(
        &mut c,
        &mut sink,
        0,
        0,
        W as i32,
        H as i32,
        readpixels::PixelFormat::new(GL_RGBA, GL_UNSIGNED_BYTE),
    )
    .expect("glReadPixels of the rendered corpus frame");
    assert_eq!(sink.executor().draws, 1, "exactly one draw executed");
    px
}

fn texel(px: &[u8], x: usize, y: usize) -> [u8; 4] {
    let o = (y * W + x) * 4;
    [px[o], px[o + 1], px[o + 2], px[o + 3]]
}

#[test]
fn es3_passthrough_renders_exact_pixels() {
    // An ES3 `#version 300 es` in/out shader (translated through the driver's desktop path) rendering a
    // red triangle over a blue clear. Center → red, corner → the blue clear, exactly.
    let vs = "#version 300 es\nin vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
    let fs = "#version 300 es\nprecision highp float;\nout vec4 o;\nvoid main(){ o = vec4(1.0, 0.0, 0.0, 1.0); }\n";
    let px = render_triangle(vs, fs, [1.0, 0.0, 0.0, 1.0]);
    assert_eq!(
        texel(&px, W / 2, H / 2),
        [255, 0, 0, 255],
        "center is the red triangle (RGBA)"
    );
    assert_eq!(
        texel(&px, 0, 0),
        [0, 0, 255, 255],
        "a corner is the blue clear (RGBA)"
    );
}

#[test]
fn es2_helper_shader_renders_exact_pixels() {
    // An ES2 shader carrying a HELPER FUNCTION (the construct the translator now preserves) still drives
    // a real frame end-to-end. Green triangle over the blue clear.
    let vs = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
    let fs = "precision mediump float;\nvec4 tint(){ return vec4(0.0, 1.0, 0.0, 1.0); }\nvoid main(){ gl_FragColor = tint(); }\n";
    let px = render_triangle(vs, fs, [0.0, 1.0, 0.0, 1.0]);
    assert_eq!(
        texel(&px, W / 2, H / 2),
        [0, 255, 0, 255],
        "center is the green triangle (RGBA)"
    );
    assert_eq!(
        texel(&px, 0, 0),
        [0, 0, 255, 255],
        "a corner is the blue clear (RGBA)"
    );
}
