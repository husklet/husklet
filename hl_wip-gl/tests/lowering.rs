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

/// How many `CreateShader` commands a batch carries (the host naga-compiles one per command).
fn count_shaders(batch: &[Cmd]) -> usize {
    batch.iter().filter(|c| matches!(c, Cmd::CreateShader { .. })).count()
}

/// How many `CreateRenderPipeline` commands a batch carries.
fn count_pipelines(batch: &[Cmd]) -> usize {
    batch.iter().filter(|c| matches!(c, Cmd::CreateRenderPipeline(_, _))).count()
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

    // the shader carries the forwarded GLSL as `Glsl` payloads (one per stage); a render pipeline uses them.
    assert!(batch.iter().any(|c| matches!(c, Cmd::CreateShader { kind: ShaderPayloadKind::Glsl, .. })));
    assert_eq!(
        batch.iter().filter(|c| matches!(c, Cmd::CreateShader { kind: ShaderPayloadKind::Glsl, .. })).count(),
        2,
        "vertex + fragment GLSL are two separate Glsl shader modules"
    );
    assert!(batch.iter().any(|c| matches!(c, Cmd::CreateRenderPipeline(_, _))));
}

/// A linked program's shader modules + render pipeline are created ONCE and re-referenced by their stable
/// IR ids on every later frame/draw that reuses the program (the program-keyed residency cache), so a reused
/// GskGpu program costs ZERO host shader compiles + pipeline builds after the first frame — the fix for the
/// per-draw shader recompile that stalled GTK4. A relink invalidates the cache and re-creates both.
#[test]
fn reused_program_is_not_recreated_across_frames() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();

    // ---- shared resources + program, set up once ----
    let vbo = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    let verts: Vec<u8> = (0..48).map(|i| i as u8).collect();
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &verts, 0x88E4);
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(&mut c, 0);

    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, VS);
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, FS);
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);
    assert!(record::link_program(&mut c, prog));
    record::use_program(&mut c, prog);
    record::uniform_sampler(&mut c, 0, 0);

    let tex = record::gen_texture(&mut c);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(&mut c, 2, 2, &[0xABu8; 16]);
    record::tex_parameter(&mut c, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
    record::tex_parameter(&mut c, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
    record::viewport(&mut c, [0, 0, 640, 480]);

    // ---- frame 1: first sight → the program's 2 shader modules + its pipeline are created once ----
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 6);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert_eq!(count_shaders(&sink.batches[0]), 2, "frame 1 compiles the program's 2 shader modules");
    assert_eq!(count_pipelines(&sink.batches[0]), 1, "frame 1 creates the program's pipeline");

    // ---- frame 2: same program, same state → resident shaders + pipeline re-referenced, NOT re-emitted ----
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 6);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let f2 = &sink.batches[1];
    assert_eq!(count_shaders(f2), 0, "a reused program emits NO new CreateShader on the 2nd frame");
    assert_eq!(count_pipelines(f2), 0, "a reused program emits NO new CreateRenderPipeline on the 2nd frame");
    // The frame still draws: the resident pipeline id is bound in the pass.
    assert!(submit_ops(f2).iter().any(|e| matches!(e, Enc::SetPipeline(_))), "the reused pipeline is still bound");

    // ---- relink: a new link generation invalidates the cache → shaders + pipeline created afresh ----
    assert!(record::link_program(&mut c, prog));
    record::use_program(&mut c, prog);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 6);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert_eq!(count_shaders(&sink.batches[2]), 2, "a relinked program re-creates its shader modules");
    assert_eq!(count_pipelines(&sink.batches[2]), 1, "a relinked program re-creates its pipeline");
}

/// Two draws of the SAME program in ONE frame (the GskGpu shape: one program batched across many draws)
/// share a single set of shader modules + pipeline — the 2nd draw adds no CreateShader / CreateRenderPipeline.
#[test]
fn same_program_across_draws_in_one_frame_compiles_once() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut c);
    // A second draw with the identical bound state (same program, same layout) within the same frame.
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 6);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];
    assert_eq!(count_shaders(batch), 2, "two draws of one program compile its 2 shader modules once, not per draw");
    assert_eq!(count_pipelines(batch), 1, "two draws of one program build its pipeline once, not per draw");
    // Both draws lowered into the single pass (two Draw ops share the one resident pipeline).
    assert_eq!(
        submit_ops(batch).iter().filter(|e| matches!(e, Enc::Draw { .. })).count(),
        2,
        "both draws are present in the pass"
    );
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

// GskGpu forwards its GLSL-ES VERBATIM to the host executor's ES route, which numbers samplers by a
// running `layout(binding=)` counter over EVERY host-recognized `uniform <samplerType> NAME;` in fragment
// text order — INCLUDING the inactive `samplerExternalOES` declarations of GskGpu's
// `#ifdef …_IS_EXTERNAL / #else` texture blocks. So the ACTIVE `sampler2D GSK_TEXTURE0` lands on host
// binding k=1 (texture 3 / sampler 4), not k=0 — the external `GSK_TEXTURE0` before it consumed k=0. The
// driver's own reflection skips the external decls and dedups, so its declaration index (0) differs from
// the host's k (1). The lowering must bind the texture at the HOST binding the compiled shader declares,
// or the bind group NACKs against the auto layout ("bindings … (1) does not match … (3)"). This asserts
// the GSK_TEXTURE0 texture/sampler land at binding 3/4, and NOT at the naive declaration-index 1/2.
#[test]
fn gsk_style_verbatim_sampler_binds_at_host_binding_past_the_external_branch() {
    // A vertex shader using `gl_VertexID` forces the forward-VERBATIM path (GskGpu vertex-pulling).
    const GSK_VS: &str = "#version 320 es\nout vec2 vUV;\nvoid main(){ vUV = vec2(0.0); gl_Position = vec4(float(gl_VertexID), 0.0, 0.0, 1.0); }\n";
    // The GskGpu texture-block shape: each texture is EITHER a `samplerExternalOES` OR a `sampler2D` triple,
    // in two preprocessor branches. Both are textually present (neither side preprocesses here), so the host
    // counts the external decl (k=0) before the active `sampler2D GSK_TEXTURE0` (k=1).
    const GSK_FS: &str = "#version 320 es\nprecision highp float;\n\
#ifdef GSK_TEXTURE0_IS_EXTERNAL\n\
uniform samplerExternalOES GSK_TEXTURE0;\n\
#else\n\
uniform sampler2D GSK_TEXTURE0;\n\
uniform sampler2D GSK_TEXTURE0_1;\n\
uniform sampler2D GSK_TEXTURE0_2;\n\
#endif\n\
in vec2 vUV;\nout vec4 fragColor;\nvoid main(){ fragColor = texture(GSK_TEXTURE0, vUV); }\n";

    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();

    // A minimal vertex buffer + attribute so the draw lowers.
    let vbo = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &vec![0u8; 48], 0x88E4);
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(&mut c, 0);

    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, vs, GSK_VS);
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, GSK_FS);
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);
    assert!(record::link_program(&mut c, prog));
    record::use_program(&mut c, prog);

    // The driver reflects three sampler2D samplers (external skipped + deduped), but their HOST bindings
    // start at k=1 because the external `GSK_TEXTURE0` consumed k=0.
    let p = c.programs.program(prog).expect("linked program");
    assert_eq!(p.samp_names, vec!["GSK_TEXTURE0", "GSK_TEXTURE0_1", "GSK_TEXTURE0_2"]);
    assert_eq!(
        p.samp_bindings,
        vec![1, 2, 3],
        "the active sampler2D GSK_TEXTURE0 is host binding k=1 (past the external branch's k=0), not the \
         naive declaration index 0"
    );

    // GSK_TEXTURE0 -> texture unit 0, with a real texture bound there (the only sampled texture).
    record::uniform_sampler(&mut c, 0, 0);
    let tex = record::gen_texture(&mut c);
    record::active_texture(&mut c, GL_TEXTURE0);
    record::bind_texture(&mut c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(&mut c, 2, 2, &[0xABu8; 16]);
    record::viewport(&mut c, [0, 0, 640, 480]);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 6);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let bg = batch
        .iter()
        .find_map(|cmd| match cmd {
            Cmd::CreateBindGroup(_, d) => Some(d),
            _ => None,
        })
        .expect("CreateBindGroup");

    // The GSK_TEXTURE0 texture lands at host binding 3 and its sampler at 4 — the layout the compiled shader
    // declares (UBO@0 / tex@1+2k / sampler@2+2k with k=1). NOT the naive declaration-index binding 1/2.
    let tex_binding = bg
        .entries
        .iter()
        .find(|e| matches!(e.resource, BindResource::Texture { .. }))
        .map(|e| e.binding)
        .expect("a texture bind entry");
    let smp_binding = bg
        .entries
        .iter()
        .find(|e| matches!(e.resource, BindResource::Sampler { .. }))
        .map(|e| e.binding)
        .expect("a sampler bind entry");
    assert_eq!(tex_binding, 3, "GSK_TEXTURE0's texture must bind at the host k=1 texture binding (3)");
    assert_eq!(smp_binding, 4, "GSK_TEXTURE0's sampler must bind at the host k=1 sampler binding (4)");
    assert!(
        !bg.entries.iter().any(|e| e.binding == 1),
        "nothing lands on binding 1 — that is the external branch's k=0 texture slot the shader never uses"
    );
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
// vertex array objects — a VAO captures + restores the attrib array + element buffer
// ---------------------------------------------------------------------------------------------------

#[test]
fn vao_round_trips_the_attrib_and_element_buffer_state() {
    let mut c = ctx_640x480();

    // Two VAOs from the default (0) binding.
    let vao_a = record::gen_vertex_array(&mut c);
    let vao_b = record::gen_vertex_array(&mut c);
    assert_ne!(vao_a, vao_b);
    assert!(record::is_vertex_array(&c, vao_a));
    assert!(!record::is_vertex_array(&c, 0)); // the default VAO is not an object name

    // Configure attribute 0 + an element-buffer binding under VAO A.
    record::bind_vertex_array(&mut c, vao_a);
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(&mut c, 0);
    record::bind_buffer(&mut c, GL_ELEMENT_ARRAY_BUFFER, 7);
    assert!(c.attr[0].enabled);
    assert_eq!(c.element_buffer, 7);

    // Binding VAO B swaps in its (fresh, empty) state.
    record::bind_vertex_array(&mut c, vao_b);
    assert!(!c.attr[0].enabled, "VAO B starts with no attribute arrays");
    assert_eq!(c.element_buffer, 0, "VAO B starts with no element buffer");

    // Re-binding VAO A restores exactly what was captured.
    record::bind_vertex_array(&mut c, vao_a);
    assert!(c.attr[0].enabled);
    assert_eq!(c.attr[0].size, 2);
    assert_eq!(c.attr[0].stride, 8);
    assert_eq!(c.element_buffer, 7);

    // Deleting the bound VAO reverts to the default and drops the name.
    assert!(record::delete_vertex_array(&mut c, vao_a));
    assert!(!record::is_vertex_array(&c, vao_a));
    assert_eq!(c.cur_vao, 0);
}

// ---------------------------------------------------------------------------------------------------
// instanced draw — the recorded instance count lowers into the IR Draw
// ---------------------------------------------------------------------------------------------------

#[test]
fn instanced_draw_records_the_instance_count_into_the_ir_draw() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut c);
    // Replace the trailing single-instance draw with an instanced one + a per-instance attribute.
    c.draws.clear();
    record::vertex_attrib_divisor(&mut c, 0, 1);
    record::draw_arrays_instanced(&mut c, GL_TRIANGLES, 0, 6, 4);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let ops = submit_ops(batch);
    let draw = ops
        .iter()
        .find(|e| matches!(e, Enc::Draw { .. }))
        .expect("a Draw op");
    match draw {
        Enc::Draw { vertex_count, instance_count, .. } => {
            assert_eq!(*vertex_count, 6);
            assert_eq!(*instance_count, 4, "the 4 instances are lowered into the Draw");
        }
        _ => unreachable!(),
    }
    // The per-instance divisor marks the vertex-buffer slot instance-stepped.
    let pipe = batch.iter().find_map(|c| match c {
        Cmd::CreateRenderPipeline(_, d) => Some(d),
        _ => None,
    });
    let pipe = pipe.expect("CreateRenderPipeline");
    assert!(
        pipe.vertex_buffers.iter().any(|vl| vl.step_mode == 1),
        "a non-zero glVertexAttribDivisor sets the slot step_mode to per-instance"
    );
}

#[test]
fn negative_instance_count_is_rejected_and_records_no_draw() {
    let mut c = ctx_640x480();
    record::draw_arrays_instanced(&mut c, GL_TRIANGLES, 0, 6, -1);
    assert!(c.draws.is_empty(), "a negative instance count records nothing");
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}

// GskGpu-style vertex-pulling instanced draw: position comes from `gl_VertexID` (no per-vertex
// attribute) and the real data is PER-INSTANCE, drawn out of one big frame VBO. With the GL
// `base-instance` feature unavailable GskGpu BAKES the per-instance region base (`first_instance *
// stride`) into the `glVertexAttribPointer` offset, so an attribute's GL offset can be far larger than
// one stride (here instance 542, stride 48 → offset 26016 for `in_rect`, 26032 for `in_color`). wgpu
// rejects a pipeline whose attribute offset exceeds the vertex-buffer `array_stride`; the lowering must
// hoist the whole-stride base into the vertex-buffer BIND offset, leaving each attribute's in-stride
// offset in `[0, stride)`. Before the fix the layout emitted the raw 26016/26032 offset (NACK); after,
// the stride is 48 with attribute offsets 0/16 and the base rides `SetVertexBuffer { offset }`.
#[test]
fn gsk_vertex_pulling_instance_offset_is_hoisted_into_the_bind_offset() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();

    // One big instance VBO: 546 instances * 48 bytes/instance (>= (542 + 4) instances the draw fetches).
    const STRIDE: i32 = 48;
    const BASE_INSTANCE: i32 = 542;
    const BASE_OFF: i32 = BASE_INSTANCE * STRIDE; // 26016 — the baked region base for the first attribute
    let vbo = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &vec![0u8; ((BASE_INSTANCE + 4) * STRIDE) as usize], 0x88E4);

    // in_rect @ field 0, in_color @ field 16 — both per-instance (divisor 1), offsets baked with the base.
    record::vertex_attrib_pointer(&mut c, 0, 4, GL_FLOAT, false, STRIDE, BASE_OFF as usize);
    record::vertex_attrib_divisor(&mut c, 0, 1);
    record::enable_vertex_attrib(&mut c, 0);
    record::vertex_attrib_pointer(&mut c, 1, 4, GL_FLOAT, false, STRIDE, (BASE_OFF + 16) as usize);
    record::vertex_attrib_divisor(&mut c, 1, 1);
    record::enable_vertex_attrib(&mut c, 1);

    // A minimal linked program so the draw lowers (the layout comes from the recorded attrib state).
    let vs = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(
        &mut c,
        vs,
        "attribute vec4 in_rect;\nattribute vec4 in_color;\nvarying vec4 vc;\n\
         void main(){ vc = in_color; gl_Position = in_rect; }\n",
    );
    record::compile_shader(&mut c, vs);
    let fs = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, fs, "precision mediump float;\nvarying vec4 vc;\nvoid main(){ gl_FragColor = vc; }\n");
    record::compile_shader(&mut c, fs);
    let prog = record::create_program(&mut c);
    record::attach_shader(&mut c, prog, vs);
    record::attach_shader(&mut c, prog, fs);
    assert!(record::link_program(&mut c, prog));
    record::use_program(&mut c, prog);

    record::viewport(&mut c, [0, 0, 640, 480]);
    record::draw_arrays_instanced(&mut c, GL_TRIANGLE_STRIP, 0, 4, 4);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];

    // The instanced VBO slot: stride 48, per-instance step, EVERY attribute offset within the stride.
    let pipe = batch
        .iter()
        .find_map(|c| match c {
            Cmd::CreateRenderPipeline(_, d) => Some(d),
            _ => None,
        })
        .expect("CreateRenderPipeline");
    let vl = &pipe.vertex_buffers[0];
    assert_eq!(vl.stride, STRIDE as u32, "the instance stride is the packed instance size, not the region base");
    assert_eq!(vl.step_mode, 1, "a divisor-1 attribute makes the slot per-instance");
    for a in &vl.attrs {
        assert!(
            a.offset < vl.stride,
            "attribute at location {} offset {} must lie within the stride {} (wgpu NACKs otherwise)",
            a.location,
            a.offset,
            vl.stride,
        );
    }
    // The hoisted-out field offsets are exactly the in-struct offsets 0 (in_rect) and 16 (in_color).
    let mut offs: Vec<u32> = vl.attrs.iter().map(|a| a.offset).collect();
    offs.sort_unstable();
    assert_eq!(offs, vec![0, 16], "field offsets are recovered relative to the instance region base");

    // The whole-stride region base rides the vertex-buffer bind offset instead.
    let ops = submit_ops(batch);
    let bind_off = ops.iter().find_map(|e| match e {
        Enc::SetVertexBuffer { slot: 0, offset, .. } => Some(*offset),
        _ => None,
    });
    assert_eq!(bind_off, Some(BASE_OFF as u64), "the baked first_instance*stride base is hoisted to the bind offset");
}

// ---------------------------------------------------------------------------------------------------
// adapter/glsl — GLSL-ES → naga-acceptable desktop GLSL (forwarded, host-compiled)
// ---------------------------------------------------------------------------------------------------

#[test]
fn glsl_translate_forwards_desktop_glsl_per_stage() {
    let (vs, fs) = glsl::translate_render(VS, FS);

    // Vertex: a desktop `#version`, the attribute regenerated as a `layout(location) in`, the varying as a
    // `layout(location) out`, and the body (incl. gl_Position) carried through verbatim.
    assert!(vs.contains("#version 460"), "desktop version pinned");
    assert!(vs.contains("layout(location = 0) in vec2 aPos;"), "attribute -> desktop in: {vs}");
    assert!(vs.contains("layout(location = 0) out vec2 vUV;"), "varying -> desktop out: {vs}");
    assert!(vs.contains("gl_Position ="), "vertex body carried through");

    // Fragment: the varying as an `in`, the sampler SPLIT into a `texture2D` (binding 1) + `sampler`
    // (binding 2) — naga rejects a combined `uniform sampler2D` — a synthesized `out vec4`, ES
    // `gl_FragColor` rewritten onto it, and the ES `texture2D(` call lowered to a desktop `texture(` over
    // the `sampler2D(tex, samp)` constructor.
    assert!(fs.contains("#version 460"));
    assert!(fs.contains("layout(location = 0) in vec2 vUV;"), "varying -> desktop in: {fs}");
    assert!(fs.contains("layout(binding = 1) uniform texture2D uTex_hltex;"), "sampler texture decl: {fs}");
    assert!(fs.contains("layout(binding = 2) uniform sampler uTex_hlsmp;"), "sampler decl: {fs}");
    assert!(fs.contains("out vec4 hl_FragColor;"), "synthesized fragment output: {fs}");
    assert!(fs.contains("hl_FragColor = texture(sampler2D(uTex_hltex, uTex_hlsmp), vUV)"), "gl_FragColor + texture2D lowered: {fs}");
    assert!(!fs.contains("gl_FragColor"), "the ES gl_FragColor builtin is gone: {fs}");
    assert!(!fs.contains("texture2D("), "the ES texture2D( call is gone: {fs}");
}

#[test]
fn glsl_collects_vertex_attrs_and_samplers() {
    let attrs = glsl::collect_vertex_attrs(VS);
    assert_eq!(attrs.len(), 1);
    assert_eq!(attrs[0].name, "aPos");
    assert_eq!(attrs[0].ty, "vec2");
    assert_eq!(glsl::program_samplers(VS, FS), vec!["uTex".to_string()]);
}
