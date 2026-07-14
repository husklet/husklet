//! Lowering tests for the frame shapes deepened in this pass — multi-draw, clear-then-draw, offscreen-FBO
//! render targets, and render-target format selection — driven through the `record` + `swap` services
//! against a `hl_gpu::RecordingSink` (no socket, no GPU: just the emitted `Cmd`/`Enc` stream).
//!
//! Complements `tests/lowering.rs` (the single-draw / clear-only / GLSL surface) and `tests/e2e.rs` (the
//! whole seam executed on the CPU reference rasterizer with pixel readback).

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{record, swap};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::enums::{texture_usage, LoadOp, TextureFormat};
use hl_gpu::{Cmd, RecordingSink};

const VS: &str = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const FS: &str = "precision mediump float;\nvoid main(){ gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }\n";

fn ctx_64() -> GlContext {
    let mut c = GlContext::new();
    c.surf = GlSurface { have: true, width: 64, height: 64 };
    c
}

/// Link + bind a flat-color program (no uniforms/samplers) and return its GL name.
fn flat_program(c: &mut GlContext) -> u32 {
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
    prog
}

/// Upload a 3-vertex position VBO and point attribute 0 at it (`stride` bytes/vertex).
fn tri_vbo(c: &mut GlContext, stride: i32) -> u32 {
    let vbo = record::gen_buffer(c);
    record::bind_buffer(c, GL_ARRAY_BUFFER, vbo);
    let verts = vec![0u8; 3 * stride as usize];
    record::buffer_data(c, GL_ARRAY_BUFFER, &verts, 0x88E4);
    record::vertex_attrib_pointer(c, 0, 2, GL_FLOAT, false, stride, 0);
    record::enable_vertex_attrib(c, 0);
    vbo
}

/// The single `Cmd::Submit` encoder ops in a batch.
fn submit_ops(batch: &[Cmd]) -> &[Enc] {
    for cmd in batch {
        if let Cmd::Submit(cb) = cmd {
            return &cb.encoder;
        }
    }
    panic!("no Submit in batch: {batch:?}");
}

/// The render-target `CreateTexture` (the one carrying `RENDER_TARGET | PRESENT` usage).
fn render_target_desc(batch: &[Cmd]) -> (u32, &hl_gpu::protocol::model::descriptor::TextureDesc) {
    batch
        .iter()
        .find_map(|c| match c {
            Cmd::CreateTexture(id, d)
                if d.usage & (texture_usage::RENDER_TARGET | texture_usage::PRESENT)
                    == (texture_usage::RENDER_TARGET | texture_usage::PRESENT) =>
            {
                Some((*id, d))
            }
            _ => None,
        })
        .expect("a render-target CreateTexture")
}

// ---------------------------------------------------------------------------------------------------
// multi-draw: two geometry draws → ONE render pass, two SetPipeline + two Draw
// ---------------------------------------------------------------------------------------------------

#[test]
fn multi_draw_frame_replays_every_draw_in_one_pass() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut c);
    tri_vbo(&mut c, 8);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let ops = submit_ops(batch);

    // Exactly one render pass wraps both draws.
    assert_eq!(ops.iter().filter(|o| matches!(o, Enc::BeginRenderPass { .. })).count(), 1);
    assert_eq!(ops.iter().filter(|o| matches!(o, Enc::EndRenderPass)).count(), 1);
    // Both draws were replayed: two pipeline binds + two draws.
    assert_eq!(ops.iter().filter(|o| matches!(o, Enc::SetPipeline(_))).count(), 2);
    assert_eq!(ops.iter().filter(|o| matches!(o, Enc::Draw { .. })).count(), 2);
    // The pass opens before any draw and closes after the last.
    let begin = ops.iter().position(|o| matches!(o, Enc::BeginRenderPass { .. })).unwrap();
    let end = ops.iter().position(|o| matches!(o, Enc::EndRenderPass)).unwrap();
    let first_draw = ops.iter().position(|o| matches!(o, Enc::Draw { .. })).unwrap();
    let last_draw = ops.iter().rposition(|o| matches!(o, Enc::Draw { .. })).unwrap();
    assert!(begin < first_draw && last_draw < end);
    assert!(matches!(batch.last().unwrap(), Cmd::Present { .. }));
}

// ---------------------------------------------------------------------------------------------------
// clear-then-draw: the leading glClear color becomes the pass LoadOp::Clear
// ---------------------------------------------------------------------------------------------------

#[test]
fn clear_then_draw_folds_clear_into_the_pass_then_draws() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    record::clear_color(&mut c, [0.2, 0.4, 0.6, 1.0]);
    record::clear(&mut c); // leading glClear
    flat_program(&mut c);
    tri_vbo(&mut c, 8);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let ops = submit_ops(&sink.batches[0]);

    match &ops[0] {
        Enc::BeginRenderPass { color, .. } => {
            assert_eq!(color[0].load, LoadOp::Clear);
            assert_eq!(color[0].clear, [0.2, 0.4, 0.6, 1.0]);
        }
        other => panic!("expected BeginRenderPass first, got {other:?}"),
    }
    assert!(ops.iter().any(|o| matches!(o, Enc::SetPipeline(_))));
    assert_eq!(ops.iter().filter(|o| matches!(o, Enc::Draw { .. })).count(), 1);
}

// ---------------------------------------------------------------------------------------------------
// default render target is Bgra8Unorm; an offscreen FBO renders into its attachment's texture + format
// ---------------------------------------------------------------------------------------------------

#[test]
fn default_target_is_bgra8() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    record::clear(&mut c);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let (_id, d) = render_target_desc(&sink.batches[0]);
    assert_eq!(d.format, TextureFormat::Bgra8Unorm);
    assert_eq!((d.width, d.height), (64, 64));
}

#[test]
fn offscreen_fbo_renders_into_the_attachment_texture_and_format() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();

    // A 32x32 Rgba8 offscreen color texture, attached to a framebuffer that we bind before drawing.
    let tex = record::gen_texture(&mut c);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d_format(&mut c, 32, 32, &[], TextureFormat::Rgba8Unorm);
    let fbo = record::gen_framebuffer(&mut c);
    record::bind_framebuffer(&mut c, 0x8D40 /* GL_FRAMEBUFFER */, fbo);
    record::framebuffer_texture_2d(&mut c, tex);

    flat_program(&mut c);
    tri_vbo(&mut c, 8);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];

    // The render target is sized + formatted from the FBO's color attachment, not the window surface.
    let (rt_id, d) = render_target_desc(batch);
    assert_eq!(d.format, TextureFormat::Rgba8Unorm, "offscreen target inherits the attachment format");
    assert_eq!((d.width, d.height), (32, 32), "offscreen target sized to the attachment");
    assert_eq!(d.label, "offscreen-fbo");

    // No default window target was created, and the frame presents the offscreen render target.
    assert!(!batch.iter().any(|c| matches!(c, Cmd::CreateTexture(_, dd) if dd.label == "default-fbo")));
    match batch.last().unwrap() {
        Cmd::Present { texture, .. } => assert_eq!(*texture, rt_id),
        other => panic!("expected Present of the offscreen target, got {other:?}"),
    }
}

#[test]
fn rebinding_default_framebuffer_returns_to_the_window_target() {
    // Binding FBO 0 restores the default framebuffer, so the next frame targets the window surface again.
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let fbo = record::gen_framebuffer(&mut c);
    record::bind_framebuffer(&mut c, 0x8D40, fbo);
    record::bind_framebuffer(&mut c, 0x8D40, 0); // back to the default framebuffer
    record::clear(&mut c);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let (_id, d) = render_target_desc(&sink.batches[0]);
    assert_eq!(d.label, "default-fbo");
    assert_eq!(d.format, TextureFormat::Bgra8Unorm);
}
