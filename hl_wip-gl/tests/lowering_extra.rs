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

// ---------------------------------------------------------------------------------------------------
// stencil test → the pipeline DepthState front/back faces + SetStencilReference + a Depth24PlusStencil8
// pass whose stencil plane clears to glClearStencil's value
// ---------------------------------------------------------------------------------------------------

fn begin_pass_depth_clear(ops: &[Enc]) -> (f32, u32) {
    ops.iter()
        .find_map(|e| match e {
            Enc::BeginRenderPass { depth, .. } => {
                let d = depth.as_ref().expect("a depth attachment");
                Some((d.clear_depth, d.clear_stencil))
            }
            _ => None,
        })
        .expect("a BeginRenderPass")
}

#[test]
fn stencil_test_lowers_to_pipeline_stencil_faces_and_reference_and_clear() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::clear_stencil(&mut c, 0x7);
    record::clear_depth(&mut c, 0.25);
    record::enable(&mut c, GL_STENCIL_TEST);
    // Compare EQUAL, ref 0x12, masks; on pass REPLACE, on stencil-fail KEEP, on depth-fail INCR.
    record::stencil_func(&mut c, GL_EQUAL, 0x12, 0xf0);
    record::stencil_op(&mut c, GL_KEEP, GL_INCR, GL_REPLACE);
    record::stencil_mask(&mut c, 0x0f);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let depth = pipeline_desc(&sink.batches[0]).depth.as_ref().expect("a stencil-tested draw carries a DepthState");
    // Wire codes: compare::EQUAL = 2; stencil_op KEEP=0, INCREMENT_CLAMP=3, REPLACE=2.
    assert_eq!(depth.stencil_front.compare, 2, "GL_EQUAL -> compare::EQUAL (2)");
    assert_eq!(depth.stencil_front.fail_op, 0, "GL_KEEP -> stencil_op::KEEP (0)");
    assert_eq!(depth.stencil_front.depth_fail_op, 3, "GL_INCR -> INCREMENT_CLAMP (3)");
    assert_eq!(depth.stencil_front.pass_op, 2, "GL_REPLACE -> stencil_op::REPLACE (2)");
    assert_eq!(depth.stencil_back, depth.stencil_front, "glStencilOp/Func set BOTH faces identically");
    assert_eq!(depth.stencil_read_mask, 0xf0, "glStencilFunc mask is the read mask");
    assert_eq!(depth.stencil_write_mask, 0x0f, "glStencilMask is the write mask");

    let ops = submit_ops(&sink.batches[0]);
    assert!(
        ops.iter().any(|e| matches!(e, Enc::SetStencilReference { reference: 0x12 })),
        "the stencil reference is emitted dynamically: {ops:?}"
    );
    let (clear_depth, clear_stencil) = begin_pass_depth_clear(ops);
    assert_eq!(clear_stencil, 0x7, "glClearStencil sets the pass stencil clear value");
    assert_eq!(clear_depth, 0.25, "glClearDepthf sets the pass depth clear value");
}

#[test]
fn stencil_op_separate_lowers_distinct_front_and_back_faces() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::enable(&mut c, GL_STENCIL_TEST);
    record::stencil_func_separate(&mut c, GL_FRONT, GL_EQUAL, 1, 0xff);
    record::stencil_func_separate(&mut c, GL_BACK, GL_ALWAYS, 1, 0xff);
    record::stencil_op_separate(&mut c, GL_FRONT, GL_KEEP, GL_KEEP, GL_REPLACE);
    record::stencil_op_separate(&mut c, GL_BACK, GL_KEEP, GL_KEEP, GL_INCR);
    record::stencil_mask_separate(&mut c, GL_FRONT_AND_BACK, 0x3c);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let depth = pipeline_desc(&sink.batches[0]).depth.as_ref().expect("DepthState");
    // Front compares EQUAL(2) + REPLACE(2) pass op; back compares ALWAYS(7) + INCREMENT_CLAMP(3) pass op.
    assert_eq!(depth.stencil_front.compare, 2, "front face GL_EQUAL");
    assert_eq!(depth.stencil_front.pass_op, 2, "front face pass op REPLACE");
    assert_eq!(depth.stencil_back.compare, 7, "back face GL_ALWAYS");
    assert_eq!(depth.stencil_back.pass_op, 3, "back face pass op INCR");
    assert_ne!(depth.stencil_front, depth.stencil_back, "separate faces lower distinctly");
    assert_eq!(depth.stencil_write_mask, 0x3c, "glStencilMaskSeparate sets the write mask");
}

// ---------------------------------------------------------------------------------------------------
// glBlendEquation (non-separate) sets the SAME op for color + alpha
// ---------------------------------------------------------------------------------------------------

#[test]
fn blend_equation_lowers_same_op_for_color_and_alpha() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::enable(&mut c, GL_BLEND);
    record::blend_func(&mut c, GL_ONE, GL_ONE);
    record::blend_equation(&mut c, GL_FUNC_REVERSE_SUBTRACT);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let blend = pipeline_desc(&sink.batches[0]).color_targets[0].blend.clone().expect("blend state");
    // op wire: FUNC_REVERSE_SUBTRACT -> 2 (frame::blend_op_wire); glBlendEquation sets BOTH ops.
    assert_eq!(blend.op_color, 2, "color equation = FUNC_REVERSE_SUBTRACT");
    assert_eq!(blend.op_alpha, 2, "alpha equation = FUNC_REVERSE_SUBTRACT (same, non-separate)");
}

// ---------------------------------------------------------------------------------------------------
// indirect draws: the args are read from the bound GL_DRAW_INDIRECT_BUFFER and lowered
// ---------------------------------------------------------------------------------------------------

fn set_indirect_buffer(c: &mut GlContext, words: &[u32]) {
    let ind = record::gen_buffer(c);
    record::bind_buffer(c, GL_DRAW_INDIRECT_BUFFER, ind);
    let mut bytes = Vec::new();
    for w in words {
        bytes.extend_from_slice(&w.to_le_bytes());
    }
    record::buffer_data(c, GL_DRAW_INDIRECT_BUFFER, &bytes, 0x88E4);
}

#[test]
fn draw_arrays_indirect_reads_count_and_instances_and_lowers_a_draw() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    // {count=3, instanceCount=4, first=0, baseInstance=0}
    set_indirect_buffer(&mut c, &[3, 4, 0, 0]);
    record::draw_arrays_indirect(&mut c, GL_TRIANGLES, 0);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let ops = submit_ops(&sink.batches[0]);
    let draw = ops.iter().find(|e| matches!(e, Enc::Draw { .. })).expect("a Draw");
    match draw {
        Enc::Draw { vertex_count, instance_count, .. } => {
            assert_eq!(*vertex_count, 3, "count read from the indirect buffer");
            assert_eq!(*instance_count, 4, "instanceCount read from the indirect buffer");
        }
        _ => unreachable!(),
    }
}

#[test]
fn draw_elements_indirect_reads_indexed_args_and_lowers_draw_indexed() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    let ebo = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_ELEMENT_ARRAY_BUFFER, ebo);
    record::buffer_data(&mut c, GL_ELEMENT_ARRAY_BUFFER, &[0u8; 48], 0x88E4);
    // {count=6, instanceCount=2, firstIndex=0, baseVertex=5, baseInstance=0}
    set_indirect_buffer(&mut c, &[6, 2, 0, 5, 0]);
    record::draw_elements_indirect(&mut c, GL_TRIANGLES, GL_UNSIGNED_INT, 0);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let ops = submit_ops(&sink.batches[0]);
    match ops.iter().find(|e| matches!(e, Enc::DrawIndexed { .. })).expect("DrawIndexed") {
        Enc::DrawIndexed { index_count, instance_count, base_vertex, .. } => {
            assert_eq!(*index_count, 6);
            assert_eq!(*instance_count, 2);
            assert_eq!(*base_vertex, 5, "baseVertex read from the indirect buffer");
        }
        _ => unreachable!(),
    }
}

#[test]
fn draw_elements_instanced_base_vertex_lowers_instances_and_base_offset() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    let ebo = record::gen_buffer(&mut c);
    record::bind_buffer(&mut c, GL_ELEMENT_ARRAY_BUFFER, ebo);
    record::buffer_data(&mut c, GL_ELEMENT_ARRAY_BUFFER, &[0u8; 24], 0x88E4);
    record::draw_elements_instanced_base_vertex(&mut c, GL_TRIANGLES, 3, GL_UNSIGNED_INT, 0, 8, 2);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let ops = submit_ops(&sink.batches[0]);
    match ops.iter().find(|e| matches!(e, Enc::DrawIndexed { .. })).expect("DrawIndexed") {
        Enc::DrawIndexed { index_count, instance_count, base_vertex, .. } => {
            assert_eq!(*index_count, 3);
            assert_eq!(*instance_count, 8, "instance count is lowered");
            assert_eq!(*base_vertex, 2, "base vertex is lowered");
        }
        _ => unreachable!(),
    }
}
