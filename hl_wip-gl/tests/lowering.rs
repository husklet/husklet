//! Lowering tests: drive the GL recording ops + the swap service against a `hl_gpu::RecordingSink` and
//! assert the exact protocol `Cmd`/`Enc` sequence a frame lowers to (plus the GLSL→shader-IR adapter).
//!
//! This is the acceptance gate for the GL→IR lowering layer: no socket, no GPU — just the recorded
//! command stream emitted at `eglSwapBuffers`. GL is deferred-lowering, so `gl*` recording submits
//! nothing; only `swap_buffers` touches the sink.

use hl_gl::adapter::glsl;
use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{record, swap};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::BindResource;
use hl_gpu::protocol::model::enums::{buffer_usage, LoadOp};
use hl_gpu::{Cmd, RecordingSink, ShaderPayloadKind};

const VS: &str = "attribute vec2 aPos;\nvarying vec2 vUV;\nvoid main(){ vUV = aPos; gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const FS: &str = "precision mediump float;\nvarying vec2 vUV;\nuniform sampler2D uTex;\nvoid main(){ gl_FragColor = texture2D(uTex, vUV); }\n";

fn ctx_640x480() -> GlContext {
    let mut c = GlContext::new();
    c.surf = GlSurface { have: true, width: 640, height: 480 };
    c
}

/// Find the single `Cmd::Submit` in a batch and return its encoder ops.
fn submit_ops(batch: &[Cmd]) -> &[Enc] {
    for cmd in batch {
        if let Cmd::Submit(cb) = cmd {
            return &cb.encoder;
        }
    }
    panic!("no Submit in batch: {batch:?}");
}

// ---------------------------------------------------------------------------------------------------
// recording layer (deferred — submits nothing)
// ---------------------------------------------------------------------------------------------------

#[test]
fn gl_recording_submits_nothing() {
    let mut c = ctx_640x480();
    let sink = RecordingSink::with_full_caps();

    let vbo = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[1u8; 48], 0x88E4);

    // Not one command was submitted — GL only emits IR at swap.
    assert!(sink.batches.is_empty());
    assert!(c.buffers.has_data(vbo));
}

#[test]
fn empty_swap_presents_nothing() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    // No draws recorded → swap is a no-op, submits nothing, returns false.
    assert!(!swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert!(sink.batches.is_empty());
}

// ---------------------------------------------------------------------------------------------------
// clear-only frame
// ---------------------------------------------------------------------------------------------------

#[test]
fn clear_only_frame_lowers_to_clear_pass_and_present() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record::clear_color(&mut c, [0.1, 0.2, 0.3, 1.0]);
    record::clear(&mut c);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert_eq!(sink.batches.len(), 1);
    let batch = &sink.batches[0];

    // default render target + surface created once.
    assert!(matches!(batch[0], Cmd::CreateTexture(1, _)));
    assert!(matches!(batch[1], Cmd::CreateSurface(1, _)));

    // the render pass clears the default target to the recorded color.
    let ops = submit_ops(batch);
    match &ops[0] {
        Enc::BeginRenderPass { color, depth } => {
            assert!(depth.is_none());
            assert_eq!(color.len(), 1);
            assert_eq!(color[0].texture, 1);
            assert_eq!(color[0].load, LoadOp::Clear);
            assert_eq!(color[0].clear, [0.1, 0.2, 0.3, 1.0]);
        }
        other => panic!("expected BeginRenderPass, got {other:?}"),
    }
    assert!(matches!(ops[1], Enc::EndRenderPass));

    // present the rendered target through its surface.
    assert_eq!(*batch.last().unwrap(), Cmd::Present { surface: 1, texture: 1 });
}

// ---------------------------------------------------------------------------------------------------
// single textured-quad draw — the full core path
// ---------------------------------------------------------------------------------------------------

/// Record a complete textured-quad frame: a VBO upload, a shader/program link, a 2x2 texture upload,
/// and one `glDrawArrays`, then swap.
fn record_textured_quad(c: &mut GlContext) {
    // vertex buffer
    let vbo = record::gen_buffer(c);
    record::bind_buffer(c, GL_ARRAY_BUFFER, vbo);
    let verts: Vec<u8> = (0..48).map(|i| i as u8).collect(); // 6 verts * vec2 f32
    record::buffer_data(c, GL_ARRAY_BUFFER, &verts, 0x88E4);
    record::vertex_attrib_pointer(c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(c, 0);

    // program
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
    record::uniform_sampler(c, 0, 0); // uTex -> texture unit 0

    // texture
    let tex = record::gen_texture(c);
    record::active_texture(c, GL_TEXTURE0);
    record::bind_texture(c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(c, 2, 2, &[0xABu8; 16]); // 2x2 RGBA8
    record::tex_parameter(c, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    record::tex_parameter(c, GL_TEXTURE_MAG_FILTER, GL_LINEAR);

    record::viewport(c, [0, 0, 640, 480]);
    record::draw_arrays(c, GL_TRIANGLES, 0, 6);
}

#[test]
fn textured_quad_uploads_buffer_and_texture() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut c);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];

    // a vertex buffer upload: CreateBuffer(VERTEX) immediately followed by its WriteBuffer.
    let vbo_pos = batch
        .iter()
        .position(|c| matches!(c, Cmd::CreateBuffer(_, d) if d.usage == buffer_usage::VERTEX))
        .expect("vertex CreateBuffer");
    let vbo_id = match &batch[vbo_pos] {
        Cmd::CreateBuffer(id, _) => *id,
        _ => unreachable!(),
    };
    assert!(matches!(&batch[vbo_pos + 1], Cmd::WriteBuffer { id, offset: 0, data } if *id == vbo_id && data.len() == 48));

    // a texture upload: CreateTexture + CreateSampler + a COPY_SRC staging buffer + WriteBuffer.
    assert!(batch.iter().any(|c| matches!(c, Cmd::CreateSampler(_, _))));
    let stage_pos = batch
        .iter()
        .position(|c| matches!(c, Cmd::CreateBuffer(_, d) if d.usage == buffer_usage::COPY_SRC))
        .expect("staging CreateBuffer");
    let stage_id = match &batch[stage_pos] {
        Cmd::CreateBuffer(id, _) => *id,
        _ => unreachable!(),
    };
    assert!(matches!(&batch[stage_pos + 1], Cmd::WriteBuffer { id, offset: 0, data } if *id == stage_id && data.len() == 16));

    // the shader carries the translated GLSL as a LegacyMsl payload; a render pipeline references it.
    assert!(batch.iter().any(|c| matches!(c, Cmd::CreateShader { kind: ShaderPayloadKind::LegacyMsl, .. })));
    assert!(batch.iter().any(|c| matches!(c, Cmd::CreateRenderPipeline(_, _))));
}

#[test]
fn textured_quad_encoder_sequence_and_present() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut c);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let ops = submit_ops(batch);

    // Expected ordered encoder shape: stage copy, then the render pass.
    assert!(matches!(ops[0], Enc::CopyBufferToTexture { width: 2, height: 2, .. }));
    assert!(matches!(ops[1], Enc::BeginRenderPass { .. }));
    assert!(matches!(ops[2], Enc::SetPipeline(_)));
    assert!(matches!(ops[3], Enc::SetViewport { .. }));
    assert!(matches!(ops[4], Enc::SetScissor { .. }));
    assert!(matches!(ops[5], Enc::SetBindGroup { index: 0, .. }));
    assert!(matches!(ops[6], Enc::SetVertexBuffer { slot: 0, .. }));
    match &ops[7] {
        Enc::Draw { vertex_count, instance_count, first_vertex, .. } => {
            assert_eq!(*vertex_count, 6);
            assert_eq!(*instance_count, 1);
            assert_eq!(*first_vertex, 0);
        }
        other => panic!("expected Draw, got {other:?}"),
    }
    assert!(matches!(ops[8], Enc::EndRenderPass));

    // the frame ends with a Present of the rendered default target.
    assert!(matches!(batch.last().unwrap(), Cmd::Present { .. }));
}

#[test]
fn textured_quad_bind_group_binds_texture_and_sampler() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut c);
    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let batch = &sink.batches[0];

    let bg = batch.iter().find_map(|c| match c {
        Cmd::CreateBindGroup(_, d) => Some(d),
        _ => None,
    });
    let bg = bg.expect("CreateBindGroup");
    assert!(bg.entries.iter().any(|e| matches!(e.resource, BindResource::Texture { .. })));
    assert!(bg.entries.iter().any(|e| matches!(e.resource, BindResource::Sampler { .. })));
}

#[test]
fn swap_resets_frame_state() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut c);
    swap::swap_buffers(&mut c, &mut sink).unwrap();
    assert!(c.draws.is_empty(), "draw-list reset after swap");
    // a second, empty swap presents nothing.
    assert!(!swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert_eq!(sink.batches.len(), 1);
}

// ---------------------------------------------------------------------------------------------------
// adapter/glsl — GLSL-ES → shader-IR
// ---------------------------------------------------------------------------------------------------

#[test]
fn glsl_translate_emits_metal_entry_points() {
    let msl = glsl::translate(VS, FS);
    assert!(msl.contains("vertex VOut vmain"), "vertex entry present");
    assert!(msl.contains("fragment float4 fmain"), "fragment entry present");
    // the GLSL vec/texture calls are lowered to MSL forms.
    assert!(msl.contains("float4"), "vec4 -> float4");
    assert!(msl.contains(".sample("), "texture2D -> sample");
}

#[test]
fn glsl_collects_vertex_attrs_and_samplers() {
    let attrs = glsl::collect_vertex_attrs(VS);
    assert_eq!(attrs.len(), 1);
    assert_eq!(attrs[0].name, "aPos");
    assert_eq!(attrs[0].ty, "vec2");
    assert_eq!(glsl::program_samplers(VS, FS), vec!["uTex".to_string()]);
}

#[test]
fn glsl_to_words_round_trips_the_source_length() {
    let words = glsl::to_words("abcdef");
    assert_eq!(words[0], 6); // byte length prefix
    // 6 bytes -> 1 length word + 2 payload words.
    assert_eq!(words.len(), 3);
}
