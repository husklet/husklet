use super::*;

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
        Enc::Draw {
            vertex_count,
            instance_count,
            ..
        } => {
            assert_eq!(*vertex_count, 6);
            assert_eq!(
                *instance_count, 4,
                "the 4 instances are lowered into the Draw"
            );
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
    assert!(
        c.draws.is_empty(),
        "a negative instance count records nothing"
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}
