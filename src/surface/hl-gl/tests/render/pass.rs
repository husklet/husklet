use super::*;

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
