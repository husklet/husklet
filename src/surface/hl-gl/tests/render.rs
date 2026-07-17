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
const FS: &str =
    "precision mediump float;\nvoid main(){ gl_FragColor = vec4(1.0, 0.0, 0.0, 1.0); }\n";

fn ctx_64() -> GlContext {
    let mut c = GlContext::new();
    c.surf = GlSurface {
        have: true,
        width: 64,
        height: 64,
    };
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
    assert_eq!(
        ops.iter()
            .filter(|o| matches!(o, Enc::BeginRenderPass { .. }))
            .count(),
        1
    );
    assert_eq!(
        ops.iter()
            .filter(|o| matches!(o, Enc::EndRenderPass))
            .count(),
        1
    );
    // Both draws were replayed: two pipeline binds + two draws.
    assert_eq!(
        ops.iter()
            .filter(|o| matches!(o, Enc::SetPipeline(_)))
            .count(),
        2
    );
    assert_eq!(
        ops.iter().filter(|o| matches!(o, Enc::Draw { .. })).count(),
        2
    );
    // The pass opens before any draw and closes after the last.
    let begin = ops
        .iter()
        .position(|o| matches!(o, Enc::BeginRenderPass { .. }))
        .unwrap();
    let end = ops
        .iter()
        .position(|o| matches!(o, Enc::EndRenderPass))
        .unwrap();
    let first_draw = ops
        .iter()
        .position(|o| matches!(o, Enc::Draw { .. }))
        .unwrap();
    let last_draw = ops
        .iter()
        .rposition(|o| matches!(o, Enc::Draw { .. }))
        .unwrap();
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
    assert_eq!(
        ops.iter().filter(|o| matches!(o, Enc::Draw { .. })).count(),
        1
    );
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
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        tex,
        0,
    );

    flat_program(&mut c);
    tri_vbo(&mut c, 8);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];

    // The render target is sized + formatted from the FBO's color attachment, not the window surface.
    let (rt_id, d) = render_target_desc(batch);
    assert_eq!(
        d.format,
        TextureFormat::Rgba8Unorm,
        "offscreen target inherits the attachment format"
    );
    assert_eq!(
        (d.width, d.height),
        (32, 32),
        "offscreen target sized to the attachment"
    );
    assert_eq!(d.label, "offscreen-fbo");

    // No default window target was created, and the frame presents the offscreen render target.
    assert!(!batch
        .iter()
        .any(|c| matches!(c, Cmd::CreateTexture(_, dd) if dd.label == "default-fbo")));
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
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, 0); // back to the default framebuffer
    record::clear(&mut c);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let (_id, d) = render_target_desc(&sink.batches[0]);
    assert_eq!(d.label, "default-fbo");
    assert_eq!(d.format, TextureFormat::Bgra8Unorm);
}

// ---------------------------------------------------------------------------------------------------
// multi-FBO frame graph: render into an offscreen FBO, then sample it from the default framebuffer
// ---------------------------------------------------------------------------------------------------

const FS_TEX: &str = "precision mediump float;\nuniform sampler2D uTex;\nvoid main(){ gl_FragColor = texture2D(uTex, vec2(0.5, 0.5)); }\n";

/// Link + bind a program that samples `uTex` (one sampler, no data uniforms) and return its GL name.
fn textured_program(c: &mut GlContext) -> u32 {
    let vs = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, vs, VS);
    record::compile_shader(c, vs);
    let fs = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, fs, FS_TEX);
    record::compile_shader(c, fs);
    let prog = record::create_program(c);
    record::attach_shader(c, prog, vs);
    record::attach_shader(c, prog, fs);
    assert!(record::link_program(c, prog));
    record::use_program(c, prog);
    prog
}

#[test]
fn multi_fbo_frame_lowers_a_pass_per_framebuffer_and_presents_the_window() {
    use hl_gpu::protocol::model::descriptor::BindResource;
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();

    // A 16x16 offscreen atlas texture attached to FBO A (the GskGL glyph-atlas shape, tiny vs the window).
    let atlas = record::gen_texture(&mut c);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, atlas);
    record::tex_image_2d_format(&mut c, 16, 16, &[], TextureFormat::Rgba8Unorm);
    let fbo = record::gen_framebuffer(&mut c);
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        atlas,
        0,
    );

    // Pass 1 — render geometry INTO the offscreen atlas.
    flat_program(&mut c);
    tri_vbo(&mut c, 8);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    // Pass 2 — bind the DEFAULT framebuffer and draw a quad that SAMPLES the atlas (the composite).
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, 0);
    let comp = textured_program(&mut c);
    record::use_program(&mut c, comp);
    record::uniform_sampler(&mut c, 0, 0); // uTex -> texture unit 0
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, atlas); // sample the FBO's color attachment
    tri_vbo(&mut c, 8);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];

    // One render-pass Submit per framebuffer run — the frame is NOT collapsed onto the first draw's FBO.
    let submits = batch.iter().filter(|c| matches!(c, Cmd::Submit(_))).count();
    assert_eq!(
        submits, 2,
        "a render pass per framebuffer (offscreen atlas, then default window)"
    );

    // The offscreen target is sized to the 16x16 attachment and carries SAMPLED (the composite reads it).
    let (atlas_rt, ad) = batch
        .iter()
        .find_map(|c| match c {
            Cmd::CreateTexture(id, d) if d.label == "offscreen-fbo" => Some((*id, d)),
            _ => None,
        })
        .expect("an offscreen render target");
    assert_eq!(
        (ad.width, ad.height),
        (16, 16),
        "offscreen target sized to its attachment"
    );
    assert!(
        ad.usage & texture_usage::SAMPLED != 0,
        "offscreen target is SAMPLED by the composite pass"
    );

    // The DEFAULT target is the WINDOW size and is what the frame presents (not the 16x16 atlas).
    let (def_rt, dd) = batch
        .iter()
        .find_map(|c| match c {
            Cmd::CreateTexture(id, d) if d.label == "default-fbo" => Some((*id, d)),
            _ => None,
        })
        .expect("a default window render target");
    assert_eq!(
        (dd.width, dd.height),
        (64, 64),
        "default target is the window size"
    );
    match batch.last().unwrap() {
        Cmd::Present { texture, .. } => {
            assert_eq!(
                *texture, def_rt,
                "presents the WINDOW target, not the offscreen atlas"
            );
            assert_ne!(*texture, atlas_rt);
        }
        other => panic!("expected Present of the window target, got {other:?}"),
    }

    // Cross-pass sampling: the composite's bind group references the offscreen RENDER-TARGET texture id
    // (the rendered atlas), NOT a fresh upload of the attachment's CPU storage.
    let samples_atlas = batch.iter().any(|c| match c {
        Cmd::CreateBindGroup(_, d) => d
            .entries
            .iter()
            .any(|e| matches!(&e.resource, BindResource::Texture { id } if *id == atlas_rt)),
        _ => false,
    });
    assert!(
        samples_atlas,
        "the composite pass samples the offscreen render-target texture (cross-pass)"
    );
}

// ---------------------------------------------------------------------------------------------------
// client-side vertex arrays (glVertexAttribPointer into CLIENT memory, NO VBO bound)
// ---------------------------------------------------------------------------------------------------

/// The `CreateBuffer(VERTEX)` id + the bytes of its immediately-following `WriteBuffer`.
fn vertex_buffer_upload(batch: &[Cmd]) -> (u32, Vec<u8>) {
    use hl_gpu::protocol::model::enums::buffer_usage;
    let pos = batch
        .iter()
        .position(|c| matches!(c, Cmd::CreateBuffer(_, d) if d.usage == buffer_usage::VERTEX))
        .expect("a VERTEX CreateBuffer");
    let id = match &batch[pos] {
        Cmd::CreateBuffer(id, _) => *id,
        _ => unreachable!(),
    };
    match &batch[pos + 1] {
        Cmd::WriteBuffer {
            id: wid,
            offset: 0,
            data,
        } if *wid == id => (id, data.clone()),
        other => panic!("expected the VERTEX buffer's WriteBuffer, got {other:?}"),
    }
}

#[test]
fn client_side_vertex_array_lowers_a_transient_vertex_buffer_and_binds_slot_0() {
    // A real client-array draw: glVertexAttribPointer points at a STACK array with NO glBindBuffer for
    // vertices (buffer 0). Before the client-array lowering this produced a pipeline needing vertex buffer
    // 0 but emitted no SetVertexBuffer → the executor rejected the draw. It must now capture the client
    // bytes into a transient VERTEX buffer and bind it to slot 0.
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let _prog = flat_program(&mut c);

    // Client-side positions (NO VBO bound): a centered triangle, tightly packed (stride 0).
    let verts: [f32; 6] = [0.0, 0.9, -0.9, -0.9, 0.9, -0.9];
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 0, verts.as_ptr() as usize);
    record::enable_vertex_attrib(&mut c, 0);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];

    // The captured client bytes were uploaded verbatim (3 verts * vec2 f32 = 24 bytes).
    let (vb_id, data) = vertex_buffer_upload(batch);
    let mut expect = Vec::new();
    for f in verts {
        expect.extend_from_slice(&f.to_le_bytes());
    }
    assert_eq!(
        data, expect,
        "the transient vertex buffer holds the captured client array"
    );

    // The pass binds that transient buffer to slot 0 and draws 3 vertices.
    let ops = submit_ops(batch);
    assert!(
        ops.iter().any(
            |o| matches!(o, Enc::SetVertexBuffer { slot: 0, buffer, offset: 0 } if *buffer == vb_id)
        ),
        "the client-array draw binds its transient buffer to vertex slot 0"
    );
    assert!(ops.iter().any(|o| matches!(
        o,
        Enc::Draw {
            vertex_count: 3,
            ..
        }
    )));

    // The pipeline declares exactly one vertex-buffer slot carrying attribute location 0.
    let desc = batch
        .iter()
        .find_map(|c| match c {
            Cmd::CreateRenderPipeline(_, d) => Some(d),
            _ => None,
        })
        .expect("a render pipeline");
    assert_eq!(desc.vertex_buffers.len(), 1, "one client-array slot");
    assert_eq!(desc.vertex_buffers[0].attrs.len(), 1);
    assert_eq!(desc.vertex_buffers[0].attrs[0].location, 0);
}

#[test]
fn client_side_index_array_lowers_a_transient_index_buffer() {
    // glDrawElements with a CLIENT index pointer (no element-array-buffer bound) + a client vertex array:
    // both must be captured into transient buffers, with the u8 indices promoted to u16.
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let _prog = flat_program(&mut c);

    let verts: [f32; 8] = [-0.9, -0.9, 0.9, -0.9, 0.9, 0.9, -0.9, 0.9]; // a quad, 4 verts
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 0, verts.as_ptr() as usize);
    record::enable_vertex_attrib(&mut c, 0);
    let idx: [u8; 6] = [0, 1, 2, 0, 2, 3];
    record::draw_elements(
        &mut c,
        GL_TRIANGLES,
        6,
        GL_UNSIGNED_BYTE,
        idx.as_ptr() as usize,
    );

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];

    // A transient INDEX buffer holding the u8 indices promoted to little-endian u16.
    use hl_gpu::protocol::model::enums::buffer_usage;
    let ipos = batch
        .iter()
        .position(|c| matches!(c, Cmd::CreateBuffer(_, d) if d.usage == buffer_usage::INDEX))
        .expect("an INDEX CreateBuffer");
    let iid = match &batch[ipos] {
        Cmd::CreateBuffer(id, _) => *id,
        _ => unreachable!(),
    };
    let idata = match &batch[ipos + 1] {
        Cmd::WriteBuffer { data, .. } => data.clone(),
        other => panic!("expected the index WriteBuffer, got {other:?}"),
    };
    let mut expect_idx = Vec::new();
    for i in idx {
        expect_idx.extend_from_slice(&(i as u16).to_le_bytes());
    }
    assert_eq!(idata, expect_idx, "u8 client indices promoted to u16");

    // The vertex array captured the 4 quad verts (max index 3 → [0,4)).
    let (_vb, vdata) = vertex_buffer_upload(batch);
    assert_eq!(
        vdata.len(),
        8 * 4,
        "4 verts * vec2 f32 captured for the index range"
    );

    // The pass sets the transient index buffer (offset 0) and issues a 6-index DrawIndexed.
    let ops = submit_ops(batch);
    assert!(ops
        .iter()
        .any(|o| matches!(o, Enc::SetIndexBuffer { buffer, offset: 0, .. } if *buffer == iid)));
    assert!(ops
        .iter()
        .any(|o| matches!(o, Enc::DrawIndexed { index_count: 6, .. })));
}

#[test]
fn flush_offscreen_submits_offscreen_passes_and_retains_window_draws() {
    // glFlush / glFinish drain the accumulated OFFSCREEN passes now (no Present) while retaining the
    // window (default-framebuffer) draws for the eventual eglSwapBuffers — keeping the swap frame bounded.
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();

    // An offscreen FBO with a color texture; render one triangle INTO it (fbo != 0).
    let atlas = record::gen_texture(&mut c);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, atlas);
    record::tex_image_2d_format(&mut c, 16, 16, &[], TextureFormat::Rgba8Unorm);
    let fbo = record::gen_framebuffer(&mut c);
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_texture_2d(
        &mut c,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        atlas,
        0,
    );
    flat_program(&mut c);
    tri_vbo(&mut c, 8);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    // A window draw (default framebuffer, fbo == 0) that must be RETAINED for the swap.
    record::bind_framebuffer(&mut c, GL_FRAMEBUFFER, 0);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert_eq!(c.draws.len(), 2, "two draws recorded before flush");

    // glFlush: drains the ONE offscreen pass (a Submit with NO Present), retains the window draw.
    assert!(
        swap::flush_offscreen(&mut c, &mut sink).unwrap(),
        "offscreen work was flushed"
    );
    assert_eq!(c.draws.len(), 1, "the window draw is retained for the swap");
    let flush_batch = &sink.batches[0];
    assert!(
        flush_batch.iter().any(|c| matches!(c, Cmd::Submit(_))),
        "the offscreen pass is submitted"
    );
    assert!(
        !flush_batch.iter().any(|c| matches!(c, Cmd::Present { .. })),
        "the offscreen flush presents nothing (no window swap)"
    );

    // The retained window draw still presents at swap.
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert!(
        sink.batches[1]
            .iter()
            .any(|c| matches!(c, Cmd::Present { .. })),
        "the swap presents the window"
    );

    // With nothing offscreen pending, a second flush is a no-op (returns false, submits nothing).
    let batches_before = sink.batches.len();
    assert!(!swap::flush_offscreen(&mut c, &mut sink).unwrap());
    assert_eq!(
        sink.batches.len(),
        batches_before,
        "an empty flush submits no batch"
    );
}
