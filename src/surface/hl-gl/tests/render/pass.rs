use super::*;

#[test]
fn disabled_arrays_lower_current_generic_attributes() {
    const VERTEX: &str = "#version 300 es
layout(location=0) in vec2 a;
layout(location=1) in vec4 b;
layout(location=2) in vec4 c;
layout(location=3) in vec2 d;
void main() { gl_Position = vec4(a + b.xy + c.xy + d, 0.0, 1.0); }";
    const FRAGMENT: &str = "#version 300 es
precision highp float;
out vec4 color;
void main() { color = vec4(1.0); }";

    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let vertex = record::create_shader(&mut context, GL_VERTEX_SHADER);
    record::shader_source(&mut context, vertex, VERTEX);
    record::compile_shader(&mut context, vertex);
    let fragment = record::create_shader(&mut context, GL_FRAGMENT_SHADER);
    record::shader_source(&mut context, fragment, FRAGMENT);
    record::compile_shader(&mut context, fragment);
    let program = record::create_program(&mut context);
    record::attach_shader(&mut context, program, vertex);
    record::attach_shader(&mut context, program, fragment);
    assert!(record::link_program(&mut context, program));
    record::use_program(&mut context, program);
    record::vertex_attrib(&mut context, 0, [1.0, 2.0, 0.0, 1.0]);
    record::vertex_attrib(&mut context, 1, [3.0, 4.0, 5.0, 6.0]);
    record::vertex_attrib(&mut context, 2, [7.0, 8.0, 9.0, 10.0]);
    record::vertex_attrib(&mut context, 3, [11.0, 12.0, 0.0, 1.0]);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let pipeline = batch
        .iter()
        .find_map(|command| match command {
            Cmd::CreateRenderPipeline(_, descriptor) => Some(descriptor),
            _ => None,
        })
        .expect("render pipeline");
    assert_eq!(pipeline.vertex_buffers.len(), 4);
    assert_eq!(
        submit_ops(batch)
            .iter()
            .filter(|operation| matches!(operation, Enc::SetVertexBuffer { .. }))
            .count(),
        4
    );
}

#[test]
fn disabled_integer_attribute_preserves_exact_bits_and_integer_format() {
    const VERTEX: &str = "#version 300 es
layout(location=0) in ivec4 value;
void main() { gl_Position = vec4(float(value.x), 0.0, 0.0, 1.0); }";
    const FRAGMENT: &str = "#version 300 es
precision highp float;
out vec4 color;
void main() { color = vec4(1.0); }";
    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let vertex = record::create_shader(&mut context, GL_VERTEX_SHADER);
    record::shader_source(&mut context, vertex, VERTEX);
    record::compile_shader(&mut context, vertex);
    let fragment = record::create_shader(&mut context, GL_FRAGMENT_SHADER);
    record::shader_source(&mut context, fragment, FRAGMENT);
    record::compile_shader(&mut context, fragment);
    let program = record::create_program(&mut context);
    record::attach_shader(&mut context, program, vertex);
    record::attach_shader(&mut context, program, fragment);
    assert!(record::link_program(&mut context, program));
    record::use_program(&mut context, program);
    record::vertex_attrib_i(&mut context, 0, [-7, 2, i32::MIN, i32::MAX]);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let pipeline = batch
        .iter()
        .find_map(|command| match command {
            Cmd::CreateRenderPipeline(_, descriptor) => Some(descriptor),
            _ => None,
        })
        .expect("pipeline");
    assert_eq!((pipeline.vertex_buffers[0].attrs[0].format >> 8) & 0xff, 6);
    let uploaded = batch
        .iter()
        .find_map(|command| match command {
            Cmd::WriteBuffer { data, .. } if data.len() >= 16 => Some(data),
            _ => None,
        })
        .expect("constant attribute upload");
    let expected = [-7i32, 2, i32::MIN, i32::MAX]
        .into_iter()
        .flat_map(i32::to_le_bytes)
        .collect::<Vec<_>>();
    assert_eq!(&uploaded[..16], expected);
}

#[test]
fn current_program_supplies_constant_attributes_when_draw_program_is_zero() {
    const VERTEX: &str = "#version 300 es
layout(location=0) in vec2 position;
void main() { gl_Position = vec4(position, 0.0, 1.0); }";
    const FRAGMENT: &str = "#version 300 es
precision highp float;
out vec4 color;
void main() { color = vec4(1.0); }";

    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let vertex = record::create_shader(&mut context, GL_VERTEX_SHADER);
    record::shader_source(&mut context, vertex, VERTEX);
    record::compile_shader(&mut context, vertex);
    let fragment = record::create_shader(&mut context, GL_FRAGMENT_SHADER);
    record::shader_source(&mut context, fragment, FRAGMENT);
    record::compile_shader(&mut context, fragment);
    let program = record::create_program(&mut context);
    record::attach_shader(&mut context, program, vertex);
    record::attach_shader(&mut context, program, fragment);
    assert!(record::link_program(&mut context, program));
    record::use_program(&mut context, program);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(context.replace_last_recorded_program(0));

    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let pipeline = sink.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateRenderPipeline(_, descriptor) => Some(descriptor),
            _ => None,
        })
        .expect("render pipeline");

    assert_eq!(pipeline.vertex_buffers.len(), 1);
    assert_eq!(pipeline.vertex_buffers[0].attrs[0].location, 0);
}

#[test]
fn draw_uses_captured_vertex_buffer_after_live_buffer_is_deleted() {
    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut context);
    let buffer = tri_vbo(&mut context, 8);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    assert!(context.delete_buffer(buffer));

    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let pipeline = sink.batches[0]
        .iter()
        .find_map(|command| match command {
            Cmd::CreateRenderPipeline(_, descriptor) => Some(descriptor),
            _ => None,
        })
        .expect("render pipeline");

    assert_eq!(pipeline.vertex_buffers.len(), 1);
    assert!(submit_ops(&sink.batches[0])
        .iter()
        .any(|operation| matches!(operation, Enc::SetVertexBuffer { .. })));
}

#[test]
fn separate_vertex_bindings_resolve_each_attribute_at_draw_time() {
    let mut context = ctx_64();
    for (name, bytes) in [(41, vec![1; 128]), (42, vec![2; 256])] {
        record::bind_buffer(&mut context, GL_ARRAY_BUFFER, name);
        record::buffer_data(&mut context, GL_ARRAY_BUFFER, &bytes, 0x88E4);
    }

    record::vertex_attrib_format(&mut context, 0, 2, GL_FLOAT, false, false, 4);
    record::vertex_attrib_binding(&mut context, 0, 3);
    record::bind_vertex_buffer(&mut context, 3, 41, 16, 24);
    record::vertex_binding_divisor(&mut context, 3, 1);
    record::enable_vertex_attrib(&mut context, 0);

    record::vertex_attrib_format(&mut context, 1, 4, GL_FLOAT, false, false, 8);
    record::vertex_attrib_binding(&mut context, 1, 5);
    record::bind_vertex_buffer(&mut context, 5, 42, 32, 40);
    record::enable_vertex_attrib(&mut context, 1);

    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    let draw = context.draws().last().expect("recorded draw");
    assert_eq!(draw.attrs[0].buffer, 41);
    assert_eq!(draw.attrs[0].offset, 20);
    assert_eq!(draw.attrs[0].stride, 24);
    assert_eq!(draw.attrs[0].divisor, 1);
    assert_eq!(draw.attrs[1].buffer, 42);
    assert_eq!(draw.attrs[1].offset, 40);
    assert_eq!(draw.attrs[1].stride, 40);
    assert_eq!(draw.attrs[1].divisor, 0);
}

#[test]
fn separate_vertex_bindings_are_vao_state() {
    let mut context = ctx_64();
    let first = record::gen_vertex_array(&mut context);
    let second = record::gen_vertex_array(&mut context);

    record::bind_vertex_array(&mut context, first);
    record::vertex_attrib_format(&mut context, 2, 2, GL_FLOAT, false, false, 12);
    record::vertex_attrib_binding(&mut context, 2, 4);
    record::bind_vertex_buffer(&mut context, 4, 77, 64, 32);
    record::enable_vertex_attrib(&mut context, 2);

    record::bind_vertex_array(&mut context, second);
    record::bind_vertex_array(&mut context, first);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    let attr = context.draws().last().expect("recorded draw").attrs[2];
    assert!(attr.enabled);
    assert_eq!(attr.buffer, 77);
    assert_eq!(attr.offset, 76);
    assert_eq!(attr.stride, 32);
}

#[test]
fn vertex_id_draw_declares_and_binds_no_vertex_buffer() {
    const VERTEX_ID: &str = "#version 300 es
out vec2 uv;
void main() {
    vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
    uv = p;
    gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}";
    const COLOR: &str = "#version 300 es
precision highp float;
in vec2 uv;
out vec4 color;
void main() { color = vec4(uv, 0.0, 1.0); }";

    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let vertex = record::create_shader(&mut context, GL_VERTEX_SHADER);
    record::shader_source(&mut context, vertex, VERTEX_ID);
    record::compile_shader(&mut context, vertex);
    let fragment = record::create_shader(&mut context, GL_FRAGMENT_SHADER);
    record::shader_source(&mut context, fragment, COLOR);
    record::compile_shader(&mut context, fragment);
    let program = record::create_program(&mut context);
    record::attach_shader(&mut context, program, vertex);
    record::attach_shader(&mut context, program, fragment);
    assert!(record::link_program(&mut context, program));
    record::use_program(&mut context, program);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let pipeline = batch
        .iter()
        .find_map(|command| match command {
            Cmd::CreateRenderPipeline(_, descriptor) => Some(descriptor),
            _ => None,
        })
        .expect("render pipeline");

    assert!(pipeline.vertex_buffers.is_empty());
    assert!(!submit_ops(batch)
        .iter()
        .any(|operation| matches!(operation, Enc::SetVertexBuffer { .. })));
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
// scissored glClear: an Enc::ClearRect over a LOAD-ing pass, never a full-target LoadOp::Clear
// ---------------------------------------------------------------------------------------------------

/// `glClear` is scissor-tested. A frame whose only clear is scissored must NOT clear-load the attachment:
/// doing so paints the whole target with the scissored clear's color (the "scissor ignored" readback) and
/// destroys the previous frame's content outside the rect. Mirrors `apps/gl-minimal` case `egl_offscreen`:
/// a 64x64 target, `glScissor(0, 0, 32, 32)`, one `glClear`.
#[test]
fn scissored_clear_alone_fills_only_its_rect_over_a_loading_pass() {
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    record::clear_color(&mut c, [0.25, 0.5, 0.75, 1.0]);
    record::enable(&mut c, GL_SCISSOR_TEST);
    record::scissor(&mut c, [0, 0, 32, 32]);
    record::clear(&mut c);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let ops = submit_ops(&sink.batches[0]);

    for op in ops {
        if let Enc::BeginRenderPass { color, .. } = op {
            assert_eq!(
                color[0].load,
                LoadOp::Load,
                "no unscissored clear justifies clearing the whole attachment"
            );
        }
    }
    assert_eq!(
        ops.iter()
            .filter(|o| matches!(
                o,
                Enc::ClearRect {
                    x: 0,
                    y: 32,
                    w: 32,
                    h: 32,
                    color,
                    ..
                } if *color == [0.25, 0.5, 0.75, 1.0]
            ))
            .count(),
        1,
        "the scissored clear must lower to exactly one ClearRect over its rect: {ops:?}"
    );
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
