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
