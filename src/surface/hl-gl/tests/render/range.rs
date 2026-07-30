use super::*;

#[test]
fn nonzero_first_vertex_stays_within_the_emitted_vertex_buffer() {
    const FIRST: i32 = 257;
    const COUNT: i32 = 3;
    const STRIDE: i32 = 16;
    const ATTRIBUTE_BYTES: u64 = 8;

    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut context);

    let final_vertex = FIRST + COUNT - 1;
    let source_size = (final_vertex as usize * STRIDE as usize) + ATTRIBUTE_BYTES as usize;
    let buffer = context.buffers.gen();
    record::bind_buffer(&mut context, GL_ARRAY_BUFFER, buffer);
    record::buffer_data(&mut context, GL_ARRAY_BUFFER, &vec![0; source_size], 0x88E4);
    record::vertex_attrib_pointer(&mut context, 0, 2, GL_FLOAT, false, STRIDE, 0);
    record::enable_vertex_attrib(&mut context, 0);
    record::draw_arrays(&mut context, GL_TRIANGLES, FIRST, COUNT);

    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let (vertex_buffer, emitted_size) = batch
        .iter()
        .find_map(|command| match command {
            Cmd::CreateBuffer(id, descriptor)
                if descriptor.usage == hl_gpu::protocol::model::enums::buffer_usage::VERTEX =>
            {
                Some((*id, descriptor.size))
            }
            _ => None,
        })
        .expect("vertex-buffer creation");

    let operations = submit_ops(batch);
    let layout = batch
        .iter()
        .find_map(|command| match command {
            Cmd::CreateRenderPipeline(_, descriptor) => descriptor.vertex_buffers.first(),
            _ => None,
        })
        .expect("vertex-buffer layout");
    let attribute = layout.attrs.first().expect("position attribute");
    let bind_offset = operations
        .iter()
        .find_map(|operation| match operation {
            Enc::SetVertexBuffer {
                slot: 0,
                buffer,
                offset,
            } if *buffer == vertex_buffer => Some(*offset),
            _ => None,
        })
        .expect("vertex-buffer binding");
    let (first_vertex, vertex_count) = operations
        .iter()
        .find_map(|operation| match operation {
            Enc::Draw {
                first_vertex,
                vertex_count,
                ..
            } => Some((*first_vertex, *vertex_count)),
            _ => None,
        })
        .expect("non-indexed draw");

    assert_eq!(emitted_size, source_size as u64);
    assert_eq!(layout.stride, STRIDE as u32);
    assert_eq!(attribute.location, 0);
    assert_eq!(first_vertex, FIRST as u32);
    assert_eq!(vertex_count, COUNT as u32);
    let final_byte = bind_offset
        + u64::from(first_vertex + vertex_count - 1) * u64::from(layout.stride)
        + u64::from(attribute.offset)
        + ATTRIBUTE_BYTES;
    assert!(
        final_byte <= emitted_size,
        "draw reads through byte {final_byte}, beyond emitted buffer size {emitted_size}"
    );
}

#[test]
fn client_array_with_nonzero_first_captures_the_full_addressed_range() {
    const FIRST: i32 = 50;
    const COUNT: i32 = 40;
    const STRIDE: u64 = 8;

    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut context);

    let vertices = vec![0.0f32; (FIRST + COUNT) as usize * 2];
    record::vertex_attrib_pointer(
        &mut context,
        0,
        2,
        GL_FLOAT,
        false,
        0,
        vertices.as_ptr() as usize,
    );
    record::enable_vertex_attrib(&mut context, 0);
    record::draw_arrays(&mut context, GL_TRIANGLES, FIRST, COUNT);

    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let (vertex_buffer, emitted_size) = batch
        .iter()
        .find_map(|command| match command {
            Cmd::CreateBuffer(id, descriptor)
                if descriptor.usage == hl_gpu::protocol::model::enums::buffer_usage::VERTEX =>
            {
                Some((*id, descriptor.size))
            }
            _ => None,
        })
        .expect("transient client vertex-buffer creation");

    let operations = submit_ops(batch);
    let bind_offset = operations
        .iter()
        .find_map(|operation| match operation {
            Enc::SetVertexBuffer {
                slot: 0,
                buffer,
                offset,
            } if *buffer == vertex_buffer => Some(*offset),
            _ => None,
        })
        .expect("transient client vertex-buffer binding");
    let (first_vertex, vertex_count) = operations
        .iter()
        .find_map(|operation| match operation {
            Enc::Draw {
                first_vertex,
                vertex_count,
                ..
            } => Some((*first_vertex, *vertex_count)),
            _ => None,
        })
        .expect("non-indexed client-array draw");

    assert_eq!(bind_offset, 0);
    assert_eq!(first_vertex, FIRST as u32);
    assert_eq!(vertex_count, COUNT as u32);
    assert_eq!(
        emitted_size,
        u64::from((FIRST + COUNT) as u32) * STRIDE,
        "capture must include skipped vertices because Draw preserves first_vertex"
    );
    let final_byte = bind_offset + u64::from(first_vertex + vertex_count - 1) * STRIDE + STRIDE;
    assert!(
        final_byte <= emitted_size,
        "draw reads through byte {final_byte}, beyond transient buffer size {emitted_size}"
    );
}

#[test]
fn indexed_draw_uses_captured_element_buffer_after_live_buffer_is_deleted() {
    const INDEX_COUNT: i32 = 90;
    const VERTEX_COUNT: usize = 40;
    const STRIDE: i32 = 8;

    let mut context = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    flat_program(&mut context);

    let vertex_buffer = context.buffers.gen();
    record::bind_buffer(&mut context, GL_ARRAY_BUFFER, vertex_buffer);
    record::buffer_data(
        &mut context,
        GL_ARRAY_BUFFER,
        &vec![0; VERTEX_COUNT * STRIDE as usize],
        0x88E4,
    );
    record::vertex_attrib_pointer(&mut context, 0, 2, GL_FLOAT, false, STRIDE, 0);
    record::enable_vertex_attrib(&mut context, 0);

    let element_buffer = context.buffers.gen();
    record::bind_buffer(&mut context, GL_ELEMENT_ARRAY_BUFFER, element_buffer);
    let indices = (0..INDEX_COUNT)
        .flat_map(|index| ((index as u16) % VERTEX_COUNT as u16).to_le_bytes())
        .collect::<Vec<_>>();
    record::buffer_data(&mut context, GL_ELEMENT_ARRAY_BUFFER, &indices, 0x88E4);
    record::draw_elements(
        &mut context,
        GL_TRIANGLES,
        INDEX_COUNT,
        GL_UNSIGNED_SHORT,
        0,
    );

    assert!(context.delete_buffer(element_buffer));
    assert!(swap::swap_buffers(&mut context, &mut sink).unwrap());
    let batch = &sink.batches[0];
    let index_buffer = batch
        .iter()
        .find_map(|command| match command {
            Cmd::CreateBuffer(id, descriptor)
                if descriptor.usage == hl_gpu::protocol::model::enums::buffer_usage::INDEX =>
            {
                Some(*id)
            }
            _ => None,
        })
        .expect("captured element buffer creation");
    assert!(batch.iter().any(|command| matches!(
        command,
        Cmd::WriteBuffer { id, data, .. } if *id == index_buffer && data == &indices
    )));

    let operations = submit_ops(batch);
    assert!(operations.iter().any(|operation| matches!(
        operation,
        Enc::DrawIndexed {
            index_count,
            first_index: 0,
            ..
        } if *index_count == INDEX_COUNT as u32
    )));
    assert!(
        !operations
            .iter()
            .any(|operation| matches!(operation, Enc::Draw { .. })),
        "an indexed draw must never fall through to a non-indexed draw"
    );
}
