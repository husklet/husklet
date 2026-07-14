//! End-to-end: a GL frame driven through the REAL `hl_gl` recording + swap services, lowered to protocol
//! `Cmd`s, submitted through an in-process [`InProcessCommandSink`] over the reference [`CpuExecutor`], and
//! run through the whole host runtime pipeline (validate → account → dispatch → execute) — then the
//! RENDERED texture is read back and its pixels asserted.
//!
//!   glClearColor + glClear + glDrawArrays(triangle)  ──record──▶  GlContext draw-list
//!        └▶ eglSwapBuffers ──build_frame_ir──▶ protocol Cmds ──submit──▶ InProcessCommandSink
//!              └▶ runtime validate → dispatch → CpuExecutor: clear the target, rasterize the triangle
//!   read_texture(target) ──▶ assert cleared background + drawn geometry.
//!
//! ## CPU-rasterizer limit (honest note)
//! The reference [`CpuExecutor`] advertises only the `KERNEL` shader payload and CANNOT run a GLSL/MSL
//! fragment shader: its raster path (`hl_gpu::cpu::service::raster`) draws triangle *coverage* using the
//! per-vertex color read straight from the vertex buffer (position at byte 0, straight-alpha RGBA color at
//! byte 8), ignoring the bound shader/textures. So this test drives what the CPU CAN execute — a
//! clear-to-color plus a vertex-colored solid triangle — and asserts the resulting image. Texture SAMPLING
//! inside a draw (and therefore a textured-quad's sampled output) is not observable on the CPU oracle; that
//! is a real-GPU-executor concern, exercised in `tests/lowering.rs` at the `Cmd`-stream level instead.
//!
//! One deliberate accommodation: the frame's shader lowers to a `ShaderPayloadKind::LegacyMsl`
//! `CreateShader` (correct for the real Metal host). The CPU executor accepts an MSL module as an opaque
//! placeholder (it never runs it), but the runtime's capability *validation* would reject the payload since
//! the CPU advertises `KERNEL` only — so the session's negotiated caps are widened to allow `MSL`, exactly
//! as a real Metal host would advertise. Nothing else about the pipeline is stubbed.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{frame, readpixels, record};

use hl_gpu::protocol::model::capability::shader_payload;
use hl_gpu::{
    CommandSink, Cmd, CpuExecutor, FakeClock, GlobalLedger, GpuExecutor, InProcessCommandSink, Limits,
    Session, TextureId,
};

const VS: &str = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const FS: &str = "precision mediump float;\nvoid main(){ gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }\n";

const W: usize = 8;
const H: usize = 8;

/// One vertex: NDC position (2 f32) then straight-alpha RGBA color (4 f32) — the 24-byte layout the CPU
/// rasterizer reads (position at byte 0, color at byte 8).
fn vertex(pos: [f32; 2], color: [f32; 4]) -> Vec<u8> {
    let mut v = Vec::with_capacity(24);
    for f in pos.iter().chain(color.iter()) {
        v.extend_from_slice(&f.to_le_bytes());
    }
    v
}

/// A solid triangle covering the target center (leaving the corners as background), all vertices `color`.
fn triangle(color: [f32; 4]) -> Vec<u8> {
    let mut b = Vec::new();
    for pos in [[-0.8f32, -0.8], [0.8, -0.8], [0.0, 0.8]] {
        b.extend(vertex(pos, color));
    }
    b
}

/// Build the in-process sink over a fresh CpuExecutor, with the session's negotiated caps widened to
/// accept the frame's `LegacyMsl` shader payload (see the module note) and byte-addressable copies.
fn cpu_sink() -> InProcessCommandSink<CpuExecutor> {
    let exec = CpuExecutor::new();
    let mut limits = Limits::from_capabilities(exec.capabilities());
    limits.copy_alignment = 1;
    limits.caps.shader_payloads |= shader_payload::MSL;
    let session = Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)));
    InProcessCommandSink::with_session(session, exec)
}

/// Link + bind the flat program and point attribute 0 at a freshly uploaded triangle VBO (`color`).
fn record_triangle(c: &mut GlContext, color: [f32; 4]) {
    let vbo = record::gen_buffer(c);
    record::bind_buffer(c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(c, GL_ARRAY_BUFFER, &triangle(color), 0x88E4);
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

/// Lower the recorded frame, submit it + its `Present` through the sink (the `eglSwapBuffers` body), and
/// return the presented render-target texture id.
fn present_frame(c: &mut GlContext, sink: &mut InProcessCommandSink<CpuExecutor>) -> u32 {
    let mut f = frame::build_frame_ir(c).expect("a frame to present");
    let (surface, texture) = f.present;
    f.cmds.push(Cmd::Present { surface, texture });
    sink.submit(&f.cmds).expect("submit + execute the frame on the CPU executor");
    c.reset_frame();
    texture
}

/// The BGRA/RGBA texel at pixel `(x, y)` in a tight-packed `W`×`H` 4-byte plane.
fn texel(px: &[u8], x: usize, y: usize) -> [u8; 4] {
    let o = (y * W + x) * 4;
    [px[o], px[o + 1], px[o + 2], px[o + 3]]
}

// ---------------------------------------------------------------------------------------------------
// default framebuffer: clear to blue, draw a red triangle, read back the presented Bgra8 target
// ---------------------------------------------------------------------------------------------------

#[test]
fn clear_and_triangle_render_to_the_default_surface() {
    let mut c = GlContext::new();
    c.surf = GlSurface { have: true, width: W as u32, height: H as u32 };
    let mut sink = cpu_sink();

    record::clear_color(&mut c, [0.0, 0.0, 1.0, 1.0]); // blue background
    record::clear(&mut c);
    record_triangle(&mut c, [1.0, 0.0, 0.0, 1.0]); // red triangle
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    let target = present_frame(&mut c, &mut sink);

    // Read back the rendered Bgra8Unorm target (stored bytes are [B, G, R, A]).
    let mut px = vec![0u8; W * H * 4];
    sink.executor().read_texture(sink.resources(), TextureId(target), &mut px).unwrap();

    // The triangle covers the center → red; a corner is untouched → the blue clear.
    assert_eq!(texel(&px, W / 2, H / 2), [0, 0, 255, 255], "center is the red triangle (BGRA)");
    assert_eq!(texel(&px, 0, 0), [255, 0, 0, 255], "top-left corner is the blue clear (BGRA)");

    // The draw really reached the rasterizer (not a silently-skipped no-op), and some pixels are red.
    assert_eq!(sink.executor().draws, 1, "exactly one draw executed");
    let red = px.chunks_exact(4).filter(|t| *t == [0, 0, 255, 255]).count();
    let blue = px.chunks_exact(4).filter(|t| *t == [255, 0, 0, 255]).count();
    assert!(red > 0 && blue > 0, "both drawn ({red}) and cleared ({blue}) pixels present");
    assert_eq!(red + blue, W * H, "every pixel is either the triangle or the clear color");
}

// ---------------------------------------------------------------------------------------------------
// offscreen FBO: render the same frame into an Rgba8 render-target texture and read it back
// ---------------------------------------------------------------------------------------------------

#[test]
fn clear_and_triangle_render_to_an_offscreen_rgba8_fbo() {
    let mut c = GlContext::new();
    c.surf = GlSurface { have: true, width: W as u32, height: H as u32 };
    let mut sink = cpu_sink();

    // An 8x8 Rgba8 offscreen color texture attached to a bound framebuffer.
    let tex = record::gen_texture(&mut c);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d_format(&mut c, W as i32, H as i32, &[], hl_gpu::protocol::model::enums::TextureFormat::Rgba8Unorm);
    let fbo = record::gen_framebuffer(&mut c);
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_texture_2d(&mut c, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);

    record::clear_color(&mut c, [0.0, 0.0, 1.0, 1.0]); // blue background
    record::clear(&mut c);
    record_triangle(&mut c, [1.0, 0.0, 0.0, 1.0]); // red triangle
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    let target = present_frame(&mut c, &mut sink);

    // Read back the offscreen Rgba8Unorm render target (stored bytes are [R, G, B, A]).
    let mut px = vec![0u8; W * H * 4];
    sink.executor().read_texture(sink.resources(), TextureId(target), &mut px).unwrap();

    assert_eq!(texel(&px, W / 2, H / 2), [255, 0, 0, 255], "center is the red triangle (RGBA)");
    assert_eq!(texel(&px, 0, 0), [0, 0, 255, 255], "top-left corner is the blue clear (RGBA)");
    assert_eq!(sink.executor().draws, 1, "exactly one draw executed into the FBO");
}

// ---------------------------------------------------------------------------------------------------
// glReadPixels: render the frame, then read the rendered default target back through the sink and assert
// the pixels — the GL device→host readback path end-to-end (render → CopyTextureToBuffer → read_buffer).
// ---------------------------------------------------------------------------------------------------

/// The RGBA texel at GL pixel `(x, y)` in a `glReadPixels`-packed `w`-wide RGBA plane (bottom-left origin).
fn read_texel(px: &[u8], x: usize, y: usize, w: usize) -> [u8; 4] {
    let o = (y * w + x) * 4;
    [px[o], px[o + 1], px[o + 2], px[o + 3]]
}

#[test]
fn glreadpixels_reads_the_rendered_default_target_back() {
    let mut c = GlContext::new();
    c.surf = GlSurface { have: true, width: W as u32, height: H as u32 };
    let mut sink = cpu_sink();

    record::clear_color(&mut c, [0.0, 0.0, 1.0, 1.0]); // blue background
    record::clear(&mut c);
    record_triangle(&mut c, [1.0, 0.0, 0.0, 1.0]); // red triangle
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    // glReadPixels(0, 0, W, H, GL_RGBA, GL_UNSIGNED_BYTE, …) — the whole default (Bgra8) target, returned
    // converted to RGBA in GL's bottom-left row order.
    let px = readpixels::read_pixels(&mut c, &mut sink, 0, 0, W as i32, H as i32, GL_RGBA)
        .expect("glReadPixels device->host readback");
    assert_eq!(px.len(), W * H * 4, "packed RGBA rectangle is w*h*4 bytes");

    // The centered triangle covers the center → red; a corner is the blue clear. (A vertical flip maps a
    // corner to a corner and the center to itself, so the symmetric assertions hold regardless of origin.)
    assert_eq!(read_texel(&px, W / 2, H / 2, W), [255, 0, 0, 255], "center is the red triangle (RGBA)");
    assert_eq!(read_texel(&px, 0, 0, W), [0, 0, 255, 255], "a corner is the blue clear (RGBA)");

    // The readback really executed a draw (not a skipped no-op), and every pixel is red-or-blue.
    assert_eq!(sink.executor().draws, 1, "exactly one draw executed for the readback");
    let red = px.chunks_exact(4).filter(|t| *t == [255, 0, 0, 255]).count();
    let blue = px.chunks_exact(4).filter(|t| *t == [0, 0, 255, 255]).count();
    assert!(red > 0 && blue > 0, "both drawn ({red}) and cleared ({blue}) pixels read back");
    assert_eq!(red + blue, W * H, "every read-back pixel is the triangle or the clear color");

    // glReadPixels is not a frame boundary: the draw-list survives so a later eglSwapBuffers still presents.
    assert!(!c.draws.is_empty(), "readback left the recorded frame intact");
}

#[test]
fn glreadpixels_of_a_subrectangle_in_bgra() {
    let mut c = GlContext::new();
    c.surf = GlSurface { have: true, width: W as u32, height: H as u32 };
    let mut sink = cpu_sink();

    record::clear_color(&mut c, [0.0, 0.0, 1.0, 1.0]);
    record::clear(&mut c);
    record_triangle(&mut c, [1.0, 0.0, 0.0, 1.0]);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    // A 2x2 rectangle around the center, read back as GL_BGRA_EXT (bytes [B, G, R, A]).
    let (rx, ry) = (W as i32 / 2 - 1, H as i32 / 2 - 1);
    let px = readpixels::read_pixels(&mut c, &mut sink, rx, ry, 2, 2, GL_BGRA_EXT)
        .expect("glReadPixels sub-rectangle in BGRA");
    assert_eq!(px.len(), 2 * 2 * 4);
    // Red triangle in BGRA is [0, 0, 255, 255]; the center rectangle is fully covered.
    for t in px.chunks_exact(4) {
        assert_eq!(t, [0, 0, 255, 255], "the center sub-rect is the red triangle (BGRA)");
    }
}

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
    c.surf = GlSurface { have: true, width: W as u32, height: H as u32 };
    let mut sink = cpu_sink();

    // Guest FBO bring-up: an 8x8 Rgba8 color texture attached to a freshly-bound framebuffer, driven
    // entirely through the wired framebuffer record ops.
    let tex = record::gen_texture(&mut c);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d_format(
        &mut c,
        W as i32,
        H as i32,
        &[],
        hl_gpu::protocol::model::enums::TextureFormat::Rgba8Unorm,
    );
    let fbo = record::gen_framebuffer(&mut c);
    assert!(record::is_framebuffer(&c, fbo), "generated name is a framebuffer object");
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_texture_2d(&mut c, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);

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

    assert_eq!(read_texel(&px, W / 2, H / 2, W), [255, 0, 0, 255], "center is the red triangle (RGBA)");
    assert_eq!(read_texel(&px, 0, 0, W), [0, 0, 255, 255], "a corner is the blue clear (RGBA)");
    assert_eq!(sink.executor().draws, 1, "exactly one draw executed into the FBO");
    let red = px.chunks_exact(4).filter(|t| *t == [255, 0, 0, 255]).count();
    let blue = px.chunks_exact(4).filter(|t| *t == [0, 0, 255, 255]).count();
    assert!(red > 0 && blue > 0, "both drawn ({red}) and cleared ({blue}) FBO pixels read back");
    assert_eq!(red + blue, W * H, "every read-back FBO pixel is the triangle or the clear color");
}

#[test]
fn guest_renderbuffer_backed_fbo_renders_and_glreadpixels_reads_it_back() {
    let mut c = GlContext::new();
    c.surf = GlSurface { have: true, width: W as u32, height: H as u32 };
    let mut sink = cpu_sink();

    // A renderbuffer color attachment, attached to the FBO BEFORE its storage is defined (the common
    // "attach then size" ordering) — proving the renderbuffer's stable texture backing keeps the
    // attachment wired once storage lands.
    let rbo = record::gen_renderbuffer(&mut c);
    assert!(record::is_renderbuffer(&c, rbo), "generated name is a renderbuffer object");
    record::bind_renderbuffer(&mut c, GL_RENDERBUFFER, rbo);

    let fbo = record::gen_framebuffer(&mut c);
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_renderbuffer(&mut c, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_RENDERBUFFER, rbo);
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
    assert_eq!(read_texel(&px, W / 2, H / 2, W), [255, 0, 0, 255], "center is the red triangle (RGBA)");
    assert_eq!(read_texel(&px, 0, 0, W), [0, 0, 255, 255], "a corner is the blue clear (RGBA)");
    assert_eq!(sink.executor().draws, 1, "exactly one draw executed into the renderbuffer FBO");
}

#[test]
fn framebuffer_status_and_object_lifecycle_are_honest() {
    let mut c = GlContext::new();
    c.surf = GlSurface { have: true, width: W as u32, height: H as u32 };

    // The default framebuffer (name 0) is always complete; an unknown name is not a framebuffer.
    assert_eq!(record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER), GL_FRAMEBUFFER_COMPLETE);
    assert!(!record::is_framebuffer(&c, 7), "an unminted name is not a framebuffer");

    // A freshly-bound FBO with no attachment reports MISSING_ATTACHMENT.
    let fbo = record::gen_framebuffer(&mut c);
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    assert_eq!(
        record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER),
        GL_FRAMEBUFFER_INCOMPLETE_MISSING_ATTACHMENT,
        "an FBO with no color attachment is missing its attachment",
    );

    // Deleting the bound FBO reverts to the default framebuffer (complete again) and drops the object.
    assert!(record::delete_framebuffer(&mut c, fbo), "deleting a live FBO reports success");
    assert!(!record::is_framebuffer(&c, fbo), "a deleted FBO is no longer a framebuffer");
    assert_eq!(c.bound_fbo, 0, "deleting the bound FBO reverts to the default framebuffer");
    assert_eq!(record::check_framebuffer_status(&mut c, GL_FRAMEBUFFER), GL_FRAMEBUFFER_COMPLETE);
}
