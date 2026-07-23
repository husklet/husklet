use super::*;

// ---------------------------------------------------------------------------------------------------
// indirect draws: the args are read from the bound GL_DRAW_INDIRECT_BUFFER and lowered
// ---------------------------------------------------------------------------------------------------

fn set_indirect_buffer(c: &mut GlContext, words: &[u32]) {
    let ind = c.buffers.gen();
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
    let draw = ops
        .iter()
        .find(|e| matches!(e, Enc::Draw { .. }))
        .expect("a Draw");
    match draw {
        Enc::Draw {
            vertex_count,
            instance_count,
            ..
        } => {
            assert_eq!(*vertex_count, 3, "count read from the indirect buffer");
            assert_eq!(
                *instance_count, 4,
                "instanceCount read from the indirect buffer"
            );
        }
        _ => unreachable!(),
    }
}

#[test]
fn draw_elements_indirect_reads_indexed_args_and_lowers_draw_indexed() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    let ebo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ELEMENT_ARRAY_BUFFER, ebo);
    record::buffer_data(&mut c, GL_ELEMENT_ARRAY_BUFFER, &[0u8; 48], 0x88E4);
    // {count=6, instanceCount=2, firstIndex=0, baseVertex=5, baseInstance=0}
    set_indirect_buffer(&mut c, &[6, 2, 0, 5, 0]);
    record::draw_elements_indirect(&mut c, GL_TRIANGLES, GL_UNSIGNED_INT, 0);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let ops = submit_ops(&sink.batches[0]);
    match ops
        .iter()
        .find(|e| matches!(e, Enc::DrawIndexed { .. }))
        .expect("DrawIndexed")
    {
        Enc::DrawIndexed {
            index_count,
            instance_count,
            base_vertex,
            ..
        } => {
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
    let ebo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ELEMENT_ARRAY_BUFFER, ebo);
    record::buffer_data(&mut c, GL_ELEMENT_ARRAY_BUFFER, &[0u8; 24], 0x88E4);
    record::draw_elements_instanced_base_vertex(&mut c, GL_TRIANGLES, 3, GL_UNSIGNED_INT, 0, 8, 2);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let ops = submit_ops(&sink.batches[0]);
    match ops
        .iter()
        .find(|e| matches!(e, Enc::DrawIndexed { .. }))
        .expect("DrawIndexed")
    {
        Enc::DrawIndexed {
            index_count,
            instance_count,
            base_vertex,
            ..
        } => {
            assert_eq!(*index_count, 3);
            assert_eq!(*instance_count, 8, "instance count is lowered");
            assert_eq!(*base_vertex, 2, "base vertex is lowered");
        }
        _ => unreachable!(),
    }
}
