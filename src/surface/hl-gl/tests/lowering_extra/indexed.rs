use super::*;

// ---------------------------------------------------------------------------------------------------
// indexed draws → DrawIndexed + the index-buffer format/offset
// ---------------------------------------------------------------------------------------------------

#[test]
fn indexed_draw_lowers_to_set_index_buffer_and_draw_indexed_u16() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    // element buffer of 6 u16 indices.
    let ebo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ELEMENT_ARRAY_BUFFER, ebo);
    record::buffer_data(&mut c, GL_ELEMENT_ARRAY_BUFFER, &[0u8; 12], 0x88E4);
    record::draw_elements(&mut c, GL_TRIANGLES, 6, GL_UNSIGNED_SHORT, 4);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let ops = submit_ops(&sink.batches[0]);
    let sib = ops
        .iter()
        .find(|e| matches!(e, Enc::SetIndexBuffer { .. }))
        .expect("SetIndexBuffer");
    match sib {
        Enc::SetIndexBuffer { offset, format, .. } => {
            assert_eq!(*offset, 4, "byte offset carried through");
            assert_eq!(*format, IndexFormat::U16);
        }
        _ => unreachable!(),
    }
    let di = ops
        .iter()
        .find(|e| matches!(e, Enc::DrawIndexed { .. }))
        .expect("DrawIndexed");
    match di {
        Enc::DrawIndexed {
            index_count,
            instance_count,
            base_vertex,
            ..
        } => {
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
    let ebo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ELEMENT_ARRAY_BUFFER, ebo);
    record::buffer_data(&mut c, GL_ELEMENT_ARRAY_BUFFER, &[0u8; 24], 0x88E4);
    record::draw_elements_base_vertex(&mut c, GL_TRIANGLES, 3, GL_UNSIGNED_INT, 0, 7);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let ops = submit_ops(&sink.batches[0]);
    match ops
        .iter()
        .find(|e| matches!(e, Enc::SetIndexBuffer { .. }))
        .unwrap()
    {
        Enc::SetIndexBuffer { format, .. } => assert_eq!(*format, IndexFormat::U32),
        _ => unreachable!(),
    }
    match ops
        .iter()
        .find(|e| matches!(e, Enc::DrawIndexed { .. }))
        .unwrap()
    {
        Enc::DrawIndexed {
            base_vertex,
            index_count,
            ..
        } => {
            assert_eq!(
                *base_vertex, 7,
                "glDrawElementsBaseVertex base offset is lowered"
            );
            assert_eq!(*index_count, 3);
        }
        _ => unreachable!(),
    }
}

#[test]
fn indexed_draw_pads_the_final_partial_vertex_stride() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    let p = flat_program(&mut c);
    record::use_program(&mut c, p);

    let vbo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    let vertices = vec![0x5a; 24];
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &vertices, 0x88E4);
    record::vertex_attrib_pointer(&mut c, 0, 4, GL_UNSIGNED_SHORT, true, 16, 0);
    record::enable_vertex_attrib(&mut c, 0);

    let ebo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ELEMENT_ARRAY_BUFFER, ebo);
    record::buffer_data(
        &mut c,
        GL_ELEMENT_ARRAY_BUFFER,
        &[0, 0, 0, 0, 1, 0, 0, 0],
        0x88E4,
    );
    record::draw_elements(&mut c, GL_LINE_STRIP, 2, GL_UNSIGNED_INT, 0);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let padded = sink.batches[0]
        .windows(2)
        .find_map(|commands| match commands {
            [Cmd::CreateBuffer(id, descriptor), Cmd::WriteBuffer { id: write, data, .. }]
                if id == write
                    && descriptor.usage
                        == hl_gpu::protocol::model::enums::buffer_usage::VERTEX =>
            {
                Some((descriptor.size, data))
            }
            _ => None,
        })
        .expect("vertex upload");

    assert_eq!(padded.0, 32, "the final indexed stride must be addressable");
    assert_eq!(&padded.1[..24], vertices.as_slice());
    assert_eq!(&padded.1[24..], &[0; 8]);
}
