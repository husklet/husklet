//! Extra lowering coverage for op paths the existing suites don't pin: indexed/base-vertex draws, the
//! index-buffer format + offset, blend-equation-separate / cull-winding / primitive-topology pipeline
//! lowering, `glClearBufferfv`, and the honest "unlinked program presents nothing" path. Drives the real
//! recording ops + swap against a `RecordingSink` and asserts the exact emitted `Cmd`/`Enc`.

use hl_gl::model::context::{GlContext, GlSurface};
use hl_gl::model::glconst::*;
use hl_gl::service::{record, swap};

use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::RenderPipelineDesc;
use hl_gpu::protocol::model::enums::{IndexFormat, Topology};
use hl_gpu::{Cmd, RecordingSink};

const VS: &str = "attribute vec2 aPos;\nvoid main(){ gl_Position = vec4(aPos, 0.0, 1.0); }\n";
const FS: &str = "void main(){ gl_FragColor = vec4(1.0); }\n";

fn ctx() -> GlContext {
    let mut c = GlContext::new();
    c.surf = GlSurface { have: true, width: 256, height: 256 };
    c
}

/// Link the flat program (no textures / uniforms), returning its GL name, WITHOUT binding it.
fn flat_program(c: &mut GlContext) -> u32 {
    let v = record::create_shader(c, GL_VERTEX_SHADER);
    record::shader_source(c, v, VS);
    let f = record::create_shader(c, GL_FRAGMENT_SHADER);
    record::shader_source(c, f, FS);
    let p = record::create_program(c);
    record::attach_shader(c, p, v);
    record::attach_shader(c, p, f);
    assert!(record::link_program(c, p));
    p
}

/// A bound program + a VBO of 4 vec2 verts with attribute 0 enabled.
fn setup_geometry(c: &mut GlContext) {
    let p = flat_program(c);
    record::use_program(c, p);
    let vbo = record::gen_buffer(c);
    record::bind_buffer(c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(c, GL_ARRAY_BUFFER, &[0u8; 32], 0x88E4);
    record::vertex_attrib_pointer(c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(c, 0);
    record::viewport(c, [0, 0, 256, 256]);
}

fn submit_ops(batch: &[Cmd]) -> &[Enc] {
    for cmd in batch {
        if let Cmd::Submit(cb) = cmd {
            return &cb.encoder;
        }
    }
    panic!("no Submit in batch");
}

fn pipeline_desc(batch: &[Cmd]) -> &RenderPipelineDesc {
    batch
        .iter()
        .find_map(|c| match c {
            Cmd::CreateRenderPipeline(_, d) => Some(d),
            _ => None,
        })
        .expect("a CreateRenderPipeline")
}

// ---------------------------------------------------------------------------------------------------
// indexed draws → DrawIndexed + the index-buffer format/offset
// ---------------------------------------------------------------------------------------------------

#[test]
fn indexed_draw_lowers_to_set_index_buffer_and_draw_indexed_u16() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    // element buffer of 6 u16 indices.
    let ebo = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_ELEMENT_ARRAY_BUFFER, ebo);
    record::buffer_data(&mut c, GL_ELEMENT_ARRAY_BUFFER, &[0u8; 12], 0x88E4);
    record::draw_elements(&mut c, GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, 4);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let ops = submit_ops(&sink.batches[0]);
    let sib = ops.iter().find(|e| matches!(e, Enc::SetIndexBuffer { .. })).expect("SetIndexBuffer");
    match sib {
        Enc::SetIndexBuffer { offset, format, .. } => {
            assert_eq!(*offset, 4, "byte offset carried through");
            assert_eq!(*format, IndexFormat::U16);
        }
        _ => unreachable!(),
    }
    let di = ops.iter().find(|e| matches!(e, Enc::DrawIndexed { .. })).expect("DrawIndexed");
    match di {
        Enc::DrawIndexed { index_count, instance_count, base_vertex, .. } => {
            assert_eq!(*index_count, 6);
            assert_eq!(*instance_count, 1);
            assert_eq!(*base_vertex, 0);
        }
        _ => unreachable!(),
    }
}

#[test]
fn base_vertex_draw_lowers_u32_index_format_and_base_vertex() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    let ebo = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_ELEMENT_ARRAY_BUFFER, ebo);
    record::buffer_data(&mut c, GL_ELEMENT_ARRAY_BUFFER, &[0u8; 24], 0x88E4);
    record::draw_elements_base_vertex(&mut c, GL_TRIANGLES, 3, GL_UNSIGNED_INT, 0, 7);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let ops = submit_ops(&sink.batches[0]);
    match ops.iter().find(|e| matches!(e, Enc::SetIndexBuffer { .. })).unwrap() {
        Enc::SetIndexBuffer { format, .. } => assert_eq!(*format, IndexFormat::U32),
        _ => unreachable!(),
    }
    match ops.iter().find(|e| matches!(e, Enc::DrawIndexed { .. })).unwrap() {
        Enc::DrawIndexed { base_vertex, index_count, .. } => {
            assert_eq!(*base_vertex, 7, "glDrawElementsBaseVertex base offset is lowered");
            assert_eq!(*index_count, 3);
        }
        _ => unreachable!(),
    }
}

// ---------------------------------------------------------------------------------------------------
// pipeline state lowering: blend-equation-separate, cull winding, topology
// ---------------------------------------------------------------------------------------------------

#[test]
fn blend_equation_separate_lowers_distinct_color_and_alpha_ops() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::enable(&mut c, GL_BLEND);
    record::blend_func_separate(&mut c, GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA, GL_ONE, GL_ZERO);
    record::blend_equation_separate(&mut c, GL_FUNC_SUBTRACT, GL_MIN);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let blend = pipeline_desc(&sink.batches[0]).color_targets[0].blend.clone().expect("blend state");
    // op wire: FUNC_SUBTRACT -> 1, MIN -> 3 (from frame::blend_op_wire).
    assert_eq!(blend.op_color, 1, "color equation = FUNC_SUBTRACT");
    assert_eq!(blend.op_alpha, 3, "alpha equation = MIN");
    // src_alpha factor wire: SRC_ALPHA -> 4.
    assert_eq!(blend.src_color, 4);
}

#[test]
fn cull_and_front_face_and_topology_lower_into_the_pipeline() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::enable(&mut c, GL_CULL_FACE);
    record::cull_face(&mut c, GL_FRONT);
    record::front_face(&mut c, GL_CW);
    record::draw_arrays(&mut c, GL_TRIANGLE_STRIP, 0, 4);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let pipe = pipeline_desc(&sink.batches[0]);
    assert_eq!(pipe.cull, 1, "GL_FRONT cull -> 1");
    assert_eq!(pipe.front_face, 1, "GL_CW winding -> 1");
    assert_eq!(pipe.topology, Topology::TriangleStrip, "GL_TRIANGLE_STRIP -> TriangleStrip");
}

#[test]
fn no_cull_when_disabled() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    swap::swap_buffers(&mut c, &mut sink).unwrap();
    assert_eq!(pipeline_desc(&sink.batches[0]).cull, 0, "cull disabled by default");
}

// ---------------------------------------------------------------------------------------------------
// glClearBufferfv → a scoped clear pass
// ---------------------------------------------------------------------------------------------------

#[test]
fn clear_buffer_color_lowers_to_a_clear_pass() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    record::clear_buffer_color(&mut c, [0.25, 0.5, 0.75, 1.0]);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let ops = submit_ops(&sink.batches[0]);
    match &ops[0] {
        Enc::BeginRenderPass { color, .. } => {
            assert_eq!(color[0].clear, [0.25, 0.5, 0.75, 1.0]);
        }
        other => panic!("expected a clear pass, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------------
// honest "unlinked program presents nothing"
// ---------------------------------------------------------------------------------------------------

#[test]
fn a_draw_with_an_unlinked_program_presents_nothing() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    // A program with attached-but-unlinked shaders bound.
    let v = record::create_shader(&mut c, GL_VERTEX_SHADER);
    record::shader_source(&mut c, v, VS);
    let f = record::create_shader(&mut c, GL_FRAGMENT_SHADER);
    record::shader_source(&mut c, f, FS);
    let p = record::create_program(&mut c);
    record::attach_shader(&mut c, p, v);
    record::attach_shader(&mut c, p, f);
    // NOTE: no link_program.
    record::use_program(&mut c, p);
    let vbo = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[0u8; 32], 0x88E4);
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 8, 0);
    record::enable_vertex_attrib(&mut c, 0);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    // The unlinked program has no shader IR -> the draw can't be lowered -> nothing is presented.
    assert!(!swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert!(sink.batches.is_empty());
    // The frame state is still reset for the next frame.
    assert!(c.draws.is_empty());
}
