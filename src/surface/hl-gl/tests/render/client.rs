use super::*;

// ---------------------------------------------------------------------------------------------------
// client-side vertex arrays (glVertexAttribPointer into CLIENT memory, NO VBO bound)
// ---------------------------------------------------------------------------------------------------

/// The `CreateBuffer(VERTEX)` id + the bytes of its immediately-following `WriteBuffer`.
fn vertex_buffer_upload(batch: &[Cmd]) -> (u32, Vec<u8>) {
    use hl_gpu::protocol::model::enums::buffer_usage;
    let pos = batch
        .iter()
        .position(|c| matches!(c, Cmd::CreateBuffer(_, d) if d.usage == buffer_usage::VERTEX))
        .expect("a VERTEX CreateBuffer");
    let id = match &batch[pos] {
        Cmd::CreateBuffer(id, _) => *id,
        _ => unreachable!(),
    };
    match &batch[pos + 1] {
        Cmd::WriteBuffer {
            id: wid,
            offset: 0,
            data,
        } if *wid == id => (id, data.clone()),
        other => panic!("expected the VERTEX buffer's WriteBuffer, got {other:?}"),
    }
}

#[test]
fn client_side_vertex_array_lowers_a_transient_vertex_buffer_and_binds_slot_0() {
    // A real client-array draw: glVertexAttribPointer points at a STACK array with NO glBindBuffer for
    // vertices (buffer 0). Before the client-array lowering this produced a pipeline needing vertex buffer
    // 0 but emitted no SetVertexBuffer → the executor rejected the draw. It must now capture the client
    // bytes into a transient VERTEX buffer and bind it to slot 0.
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let _prog = flat_program(&mut c);

    // Client-side positions (NO VBO bound): a centered triangle, tightly packed (stride 0).
    let verts: [f32; 6] = [0.0, 0.9, -0.9, -0.9, 0.9, -0.9];
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 0, verts.as_ptr() as usize);
    record::enable_vertex_attrib(&mut c, 0);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];

    // The captured client bytes were uploaded verbatim (3 verts * vec2 f32 = 24 bytes).
    let (vb_id, data) = vertex_buffer_upload(batch);
    let mut expect = Vec::new();
    for f in verts {
        expect.extend_from_slice(&f.to_le_bytes());
    }
    assert_eq!(
        data, expect,
        "the transient vertex buffer holds the captured client array"
    );

    // The pass binds that transient buffer to slot 0 and draws 3 vertices.
    let ops = submit_ops(batch);
    assert!(
        ops.iter().any(
            |o| matches!(o, Enc::SetVertexBuffer { slot: 0, buffer, offset: 0 } if *buffer == vb_id)
        ),
        "the client-array draw binds its transient buffer to vertex slot 0"
    );
    assert!(ops.iter().any(|o| matches!(
        o,
        Enc::Draw {
            vertex_count: 3,
            ..
        }
    )));

    // The pipeline declares exactly one vertex-buffer slot carrying attribute location 0.
    let desc = batch
        .iter()
        .find_map(|c| match c {
            Cmd::CreateRenderPipeline(_, d) => Some(d),
            _ => None,
        })
        .expect("a render pipeline");
    assert_eq!(desc.vertex_buffers.len(), 1, "one client-array slot");
    assert_eq!(desc.vertex_buffers[0].attrs.len(), 1);
    assert_eq!(desc.vertex_buffers[0].attrs[0].location, 0);
}

#[test]
fn client_side_index_array_lowers_a_transient_index_buffer() {
    // glDrawElements with a CLIENT index pointer (no element-array-buffer bound) + a client vertex array:
    // both must be captured into transient buffers, with the u8 indices promoted to u16.
    let mut c = ctx_64();
    let mut sink = RecordingSink::with_full_caps();
    let _prog = flat_program(&mut c);

    let verts: [f32; 8] = [-0.9, -0.9, 0.9, -0.9, 0.9, 0.9, -0.9, 0.9]; // a quad, 4 verts
    record::vertex_attrib_pointer(&mut c, 0, 2, GL_FLOAT, false, 0, verts.as_ptr() as usize);
    record::enable_vertex_attrib(&mut c, 0);
    let idx: [u8; 6] = [0, 1, 2, 0, 2, 3];
    record::draw_elements(
        &mut c,
        GL_TRIANGLES,
        6,
        GL_UNSIGNED_BYTE,
        idx.as_ptr() as usize,
    );

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let batch = &sink.batches[0];

    // A transient INDEX buffer holding the u8 indices promoted to little-endian u16.
    use hl_gpu::protocol::model::enums::buffer_usage;
    let ipos = batch
        .iter()
        .position(|c| matches!(c, Cmd::CreateBuffer(_, d) if d.usage == buffer_usage::INDEX))
        .expect("an INDEX CreateBuffer");
    let iid = match &batch[ipos] {
        Cmd::CreateBuffer(id, _) => *id,
        _ => unreachable!(),
    };
    let idata = match &batch[ipos + 1] {
        Cmd::WriteBuffer { data, .. } => data.clone(),
        other => panic!("expected the index WriteBuffer, got {other:?}"),
    };
    let mut expect_idx = Vec::new();
    for i in idx {
        expect_idx.extend_from_slice(&(i as u16).to_le_bytes());
    }
    assert_eq!(idata, expect_idx, "u8 client indices promoted to u16");

    // The vertex array captured the 4 quad verts (max index 3 → [0,4)).
    let (_vb, vdata) = vertex_buffer_upload(batch);
    assert_eq!(
        vdata.len(),
        8 * 4,
        "4 verts * vec2 f32 captured for the index range"
    );

    // The pass sets the transient index buffer (offset 0) and issues a 6-index DrawIndexed.
    let ops = submit_ops(batch);
    assert!(ops
        .iter()
        .any(|o| matches!(o, Enc::SetIndexBuffer { buffer, offset: 0, .. } if *buffer == iid)));
    assert!(ops
        .iter()
        .any(|o| matches!(o, Enc::DrawIndexed { index_count: 6, .. })));
}
