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
    Cmd, CommandSink, CpuExecutor, FakeClock, GlobalLedger, GpuExecutor, InProcessCommandSink,
    Limits, Session, TextureId,
};

const VS: &str = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const FS: &str =
    "precision mediump float;\nvoid main(){ gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }\n";

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
/// accept the frame's forwarded `Glsl` shader payloads (opaque to the fixed-function oracle, which
/// rasterizes from the pipeline + vertex data) and byte-addressable copies.
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

/// Link + bind the flat program and point attribute 0 at a freshly uploaded triangle VBO (`color`).
fn record_triangle(c: &mut GlContext, color: [f32; 4]) {
    let vbo = c.buffers.gen();
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
    let mut f = frame::Frame::build(c).expect("a frame to present");
    let (surface, texture) = f.present;
    f.cmds.push(Cmd::Present { surface, texture });
    sink.submit(&f.cmds)
        .expect("submit + execute the frame on the CPU executor");
    c.reset_frame();
    texture
}

/// The BGRA/RGBA texel at pixel `(x, y)` in a tight-packed `W`×`H` 4-byte plane.
fn texel(px: &[u8], x: usize, y: usize) -> [u8; 4] {
    let o = (y * W + x) * 4;
    [px[o], px[o + 1], px[o + 2], px[o + 3]]
}

/// The RGBA texel at GL pixel `(x, y)` in a `glReadPixels`-packed `w`-wide RGBA plane (bottom-left origin).
fn read_texel(px: &[u8], x: usize, y: usize, w: usize) -> [u8; 4] {
    let o = (y * w + x) * 4;
    [px[o], px[o + 1], px[o + 2], px[o + 3]]
}

#[path = "e2e/framebuffer.rs"]
mod framebuffer;
#[path = "e2e/offscreen.rs"]
mod offscreen;
#[path = "e2e/readback.rs"]
mod readback;
#[path = "e2e/surface.rs"]
mod surface;
