use super::*;

#[test]
fn swap_resets_frame_state() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut c);
    swap::swap_buffers(&mut c, &mut sink).unwrap();
    assert!(c.draws().is_empty(), "draw-list reset after swap");
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
    assert!(c.attributes()[0].enabled);
    assert_eq!(c.buffer_for_target(GL_ELEMENT_ARRAY_BUFFER), 7);

    // Binding VAO B swaps in its (fresh, empty) state.
    record::bind_vertex_array(&mut c, vao_b);
    assert!(
        !c.attributes()[0].enabled,
        "VAO B starts with no attribute arrays"
    );
    assert_eq!(
        c.buffer_for_target(GL_ELEMENT_ARRAY_BUFFER),
        0,
        "VAO B starts with no element buffer"
    );

    // Re-binding VAO A restores exactly what was captured.
    record::bind_vertex_array(&mut c, vao_a);
    assert!(c.attributes()[0].enabled);
    assert_eq!(c.attributes()[0].size, 2);
    assert_eq!(c.attributes()[0].stride, 8);
    assert_eq!(c.buffer_for_target(GL_ELEMENT_ARRAY_BUFFER), 7);

    // Deleting the bound VAO reverts to the default and drops the name.
    assert!(record::delete_vertex_array(&mut c, vao_a));
    assert!(!record::is_vertex_array(&c, vao_a));
    assert_eq!(c.current_vertex_array(), 0);
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
    c.clear_recording();
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
fn instance_divisor_greater_than_one_repeats_each_source_element() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut c);
    c.clear_recording();
    record::vertex_attrib_divisor(&mut c, 0, 2);
    record::draw_arrays_instanced(&mut c, GL_TRIANGLES, 0, 3, 4);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let expanded = batch
        .iter()
        .find_map(|command| match command {
            Cmd::CreateBuffer(id, descriptor)
                if descriptor.label == "gl-divisor-vertex:2" =>
            {
                batch.iter().find_map(|write| match write {
                    Cmd::WriteBuffer {
                        id: write_id, data, ..
                    } if write_id == id => Some(data),
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("divisor replacement buffer");

    // The source is six tightly packed vec2 values. Four instances at divisor two must fetch source
    // elements [0, 0, 1, 1], represented as four per-instance vec2 records.
    assert_eq!(expanded.len(), 4 * 8);
    assert_eq!(&expanded[0..8], &expanded[8..16]);
    assert_eq!(&expanded[16..24], &expanded[24..32]);
    assert_ne!(&expanded[0..8], &expanded[16..24]);

    let pipeline = batch
        .iter()
        .find_map(|command| match command {
            Cmd::CreateRenderPipeline(_, descriptor) => Some(descriptor),
            _ => None,
        })
        .expect("render pipeline");
    assert!(pipeline.vertex_buffers.iter().any(|slot| {
        slot.step_mode == 1 && slot.attrs.iter().any(|attribute| attribute.location == 0)
    }));
}

#[test]
fn client_instance_divisor_captures_the_instance_span_and_repeats_it() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record_textured_quad(&mut c);
    c.clear_recording();

    let values = [[1.0f32, 2.0], [3.0, 4.0], [5.0, 6.0]];
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, 0);
    record::vertex_attrib_pointer(
        &mut c,
        0,
        2,
        GL_FLOAT,
        false,
        8,
        values.as_ptr() as usize,
    );
    record::vertex_attrib_divisor(&mut c, 0, 2);
    record::draw_arrays_instanced(&mut c, GL_TRIANGLES, 0, 3, 6);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let expanded = batch
        .iter()
        .find_map(|command| match command {
            Cmd::CreateBuffer(id, descriptor)
                if descriptor.label == "gl-divisor-client-vertex:2" =>
            {
                batch.iter().find_map(|write| match write {
                    Cmd::WriteBuffer {
                        id: write_id, data, ..
                    } if write_id == id => Some(data),
                    _ => None,
                })
            }
            _ => None,
        })
        .expect("expanded client instance stream");
    let expected = values
        .iter()
        .flat_map(|value| [value, value])
        .flatten()
        .flat_map(|component| component.to_le_bytes())
        .collect::<Vec<_>>();
    assert_eq!(expanded, &expected);
}

#[test]
fn divisor_materialization_covers_cts_rates_edges_storage_and_draw_methods() {
    fn edge_counts(divisor: u32) -> Vec<u32> {
        let mut counts = vec![
            0,
            1,
            divisor.saturating_sub(1),
            divisor,
            divisor.saturating_add(1),
        ];
        counts.sort_unstable();
        counts.dedup();
        counts
    }

    fn uploaded<'a>(batch: &'a [Cmd], label: &str) -> Option<&'a [u8]> {
        batch.iter().find_map(|command| match command {
            Cmd::CreateBuffer(id, descriptor) if descriptor.label == label => {
                batch.iter().find_map(|write| match write {
                    Cmd::WriteBuffer {
                        id: write_id, data, ..
                    } if write_id == id => Some(data.as_slice()),
                    _ => None,
                })
            }
            _ => None,
        })
    }

    let values = (0..140)
        .map(|index| [index as f32 + 1.0, -(index as f32 + 1.0)])
        .collect::<Vec<_>>();
    let value_bytes = values
        .iter()
        .flatten()
        .flat_map(|component| component.to_le_bytes())
        .collect::<Vec<_>>();

    for divisor in [0u32, 1, 3, 129] {
        for instances in edge_counts(divisor) {
            for client in [false, true] {
                for indexed in [false, true] {
                    let case = format!(
                        "divisor={divisor} instances={instances} client={client} indexed={indexed}"
                    );
                    let mut c = ctx_640x480();
                    let mut sink = RecordingSink::with_full_caps();
                    record_textured_quad(&mut c);
                    c.clear_recording();

                    if client {
                        record::bind_buffer(&mut c, GL_ARRAY_BUFFER, 0);
                        record::vertex_attrib_pointer(
                            &mut c,
                            0,
                            2,
                            GL_FLOAT,
                            false,
                            8,
                            values.as_ptr() as usize,
                        );
                    } else {
                        let vbo = c.buffers.gen();
                        record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
                        record::buffer_data(&mut c, GL_ARRAY_BUFFER, &value_bytes, 0x88E4);
                        record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 8, 0);
                    }
                    record::enable_vertex_attrib(&mut c, 0);
                    record::vertex_attrib_divisor(&mut c, 0, divisor);

                    if indexed {
                        let ebo = c.buffers.gen();
                        record::bind_buffer(&mut c, GL_ELEMENT_ARRAY_BUFFER, ebo);
                        record::buffer_data(
                            &mut c,
                            GL_ELEMENT_ARRAY_BUFFER,
                            &[0, 0, 1, 0, 2, 0],
                            0x88E4,
                        );
                        record::draw_elements_instanced(
                            &mut c,
                            GL_TRIANGLES,
                            3,
                            GL_UNSIGNED_SHORT,
                            0,
                            instances as i32,
                        );
                    } else {
                        record::draw_arrays_instanced(
                            &mut c,
                            GL_TRIANGLES,
                            0,
                            3,
                            instances as i32,
                        );
                    }

                    if instances == 0 {
                        assert!(c.draws().is_empty(), "zero-instance draw recorded: {case}");
                        continue;
                    }
                    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap(), "{case}");
                    let batch = &sink.batches[0];
                    let label = if client {
                        format!("gl-divisor-client-vertex:{divisor}")
                    } else {
                        format!("gl-divisor-vertex:{divisor}")
                    };
                    if divisor <= 1 {
                        assert!(
                            uploaded(batch, &label).is_none(),
                            "unexpected materialization: {case}"
                        );
                    } else {
                        let actual = uploaded(batch, &label)
                            .unwrap_or_else(|| panic!("missing materialization: {case}"));
                        let expected = (0..instances)
                            .flat_map(|instance| values[(instance / divisor) as usize])
                            .flat_map(|component| component.to_le_bytes())
                            .collect::<Vec<_>>();
                        assert_eq!(actual, expected, "{case}");
                    }

                    let pipeline = batch
                        .iter()
                        .find_map(|command| match command {
                            Cmd::CreateRenderPipeline(_, descriptor) => Some(descriptor),
                            _ => None,
                        })
                        .unwrap_or_else(|| panic!("missing pipeline: {case}"));
                    let step = pipeline
                        .vertex_buffers
                        .iter()
                        .find(|slot| slot.attrs.iter().any(|attribute| attribute.location == 0))
                        .unwrap_or_else(|| panic!("missing attribute layout: {case}"))
                        .step_mode;
                    assert_eq!(step, u32::from(divisor != 0), "{case}");
                }
            }
        }
    }
}

#[test]
fn negative_instance_count_is_rejected_and_records_no_draw() {
    let mut c = ctx_640x480();
    record::draw_arrays_instanced(&mut c, GL_TRIANGLES, 0, 6, -1);
    assert!(
        c.draws().is_empty(),
        "a negative instance count records nothing"
    );
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}
