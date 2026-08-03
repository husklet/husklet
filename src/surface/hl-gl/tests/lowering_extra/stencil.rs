use super::*;

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
    // The pass clear values come from the recorded `glClear`, not from live `glClearDepthf`/`glClearStencil`
    // state at lowering time: an app moves those between clears, and reading them late gave every depth
    // clear in a frame the frame's LAST value. Without a clear the pass keeps GL's initial 1.0 / 0.
    record::clear_buffers(&mut c, GL_DEPTH_BUFFER_BIT | GL_STENCIL_BUFFER_BIT);
    record::enable(&mut c, GL_STENCIL_TEST);
    // Compare EQUAL, ref 0x12, masks; on pass REPLACE, on stencil-fail KEEP, on depth-fail INCR.
    record::stencil_func(&mut c, GL_EQUAL, 0x12, 0xf0);
    record::stencil_op(&mut c, GL_KEEP, GL_INCR, GL_REPLACE);
    record::stencil_mask(&mut c, 0x0f);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let depth = pipeline_desc(&sink.batches[0])
        .depth
        .as_ref()
        .expect("a stencil-tested draw carries a DepthState");
    // Wire codes: compare::EQUAL = 2; stencil_op KEEP=0, INCREMENT_CLAMP=3, REPLACE=2.
    assert_eq!(
        depth.stencil_front.compare, 2,
        "GL_EQUAL -> compare::EQUAL (2)"
    );
    assert_eq!(
        depth.stencil_front.fail_op, 0,
        "GL_KEEP -> stencil_op::KEEP (0)"
    );
    assert_eq!(
        depth.stencil_front.depth_fail_op, 3,
        "GL_INCR -> INCREMENT_CLAMP (3)"
    );
    assert_eq!(
        depth.stencil_front.pass_op, 2,
        "GL_REPLACE -> stencil_op::REPLACE (2)"
    );
    assert_eq!(
        depth.stencil_back, depth.stencil_front,
        "glStencilOp/Func set BOTH faces identically"
    );
    assert_eq!(
        depth.stencil_read_mask, 0xf0,
        "glStencilFunc mask is the read mask"
    );
    assert_eq!(
        depth.stencil_write_mask, 0x0f,
        "glStencilMask is the write mask"
    );

    let ops = submit_ops(&sink.batches[0]);
    assert!(
        ops.iter()
            .any(|e| matches!(e, Enc::SetStencilReference { reference: 0x12 })),
        "the stencil reference is emitted dynamically: {ops:?}"
    );
    let (clear_depth, clear_stencil) = begin_pass_depth_clear(ops);
    assert_eq!(
        clear_stencil, 0x7,
        "glClearStencil sets the pass stencil clear value"
    );
    assert_eq!(
        clear_depth, 0.25,
        "glClearDepthf sets the pass depth clear value"
    );
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
    let depth = pipeline_desc(&sink.batches[0])
        .depth
        .as_ref()
        .expect("DepthState");
    // Front compares EQUAL(2) + REPLACE(2) pass op; back compares ALWAYS(7) + INCREMENT_CLAMP(3) pass op.
    assert_eq!(depth.stencil_front.compare, 2, "front face GL_EQUAL");
    assert_eq!(depth.stencil_front.pass_op, 2, "front face pass op REPLACE");
    assert_eq!(depth.stencil_back.compare, 7, "back face GL_ALWAYS");
    assert_eq!(depth.stencil_back.pass_op, 3, "back face pass op INCR");
    assert_ne!(
        depth.stencil_front, depth.stencil_back,
        "separate faces lower distinctly"
    );
    assert_eq!(
        depth.stencil_write_mask, 0x3c,
        "glStencilMaskSeparate sets the write mask"
    );
}

#[test]
fn distinct_face_masks_lower_to_face_culled_pipeline_draws() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::enable(&mut c, GL_STENCIL_TEST);
    record::stencil_func_separate(&mut c, GL_FRONT, GL_ALWAYS, 260, 0xf0);
    record::stencil_func_separate(&mut c, GL_BACK, GL_ALWAYS, -3, 0x0f);
    record::stencil_mask_separate(&mut c, GL_FRONT, 0xcc);
    record::stencil_mask_separate(&mut c, GL_BACK, 0x33);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let pipelines = sink.batches[0]
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateRenderPipeline(_, descriptor) => Some(descriptor),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(pipelines.len(), 2, "one culling pipeline per stencil face");
    let masks = pipelines
        .iter()
        .map(|pipeline| {
            let depth = pipeline.depth.as_ref().unwrap();
            (depth.stencil_read_mask, depth.stencil_write_mask)
        })
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(masks, [(0x0f, 0x33), (0xf0, 0xcc)].into_iter().collect());
    assert_eq!(
        submit_ops(&sink.batches[0])
            .iter()
            .filter(|op| matches!(op, Enc::Draw { .. } | Enc::DrawIndexed { .. }))
            .count(),
        2
    );
    let references = submit_ops(&sink.batches[0])
        .iter()
        .filter_map(|op| match op {
            Enc::SetStencilReference { reference } => Some(*reference),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(references, [255, 0]);
}

// ---------------------------------------------------------------------------------------------------
// A colour clear must not discard draws that wrote the stencil or depth plane
// ---------------------------------------------------------------------------------------------------

/// An unscissored `glClear(GL_COLOR_BUFFER_BIT)` folds into the pass load op and DISCARDS the draws before
/// it, because they can no longer be seen. That is only sound when those draws affected nothing but
/// colour: a `GL_INCR` draw leaves a stencil value the colour clear does not erase, and a later
/// `GL_EQUAL` draw tests against it.
///
/// The differential harness caught this as three `GL_INCR` draws, a `glClear(GL_COLOR)`, and a
/// `glStencilFunc(GL_EQUAL, 3, 0x0F)` draw reading black where the reference reads red — the three
/// increments had been dropped, so the stencil never reached 3. The clear must instead become a
/// full-target `Enc::ClearRect` between two passes, which paints colour only.
#[test]
fn a_colour_clear_does_not_discard_earlier_stencil_writing_draws() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::enable(&mut c, GL_STENCIL_TEST);
    record::stencil_func(&mut c, GL_ALWAYS, 0, 0xff);
    record::stencil_op(&mut c, GL_KEEP, GL_KEEP, GL_INCR);
    record::stencil_mask(&mut c, 0x0f);
    for _ in 0..3 {
        record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    }
    record::stencil_func(&mut c, GL_EQUAL, 3, 0x0f);
    record::stencil_op(&mut c, GL_KEEP, GL_KEEP, GL_KEEP);
    record::clear_color(&mut c, [0.0, 0.0, 0.0, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let ops = submit_ops(&sink.batches[0]);
    let draws = ops
        .iter()
        .filter(|e| matches!(e, Enc::Draw { .. } | Enc::DrawIndexed { .. }))
        .count();
    assert_eq!(
        draws, 4,
        "all three increments and the test draw must reach the encoder: {ops:?}"
    );
    assert!(
        ops.iter().any(|e| matches!(e, Enc::ClearRect { .. })),
        "the colour clear is painted as a full-target rect, not folded into a load op: {ops:?}"
    );
    // The load-bearing property: the pass AFTER the colour clear must load-preserve the depth/stencil
    // plane, or the three increments are lost anyway. (The FIRST pass legitimately clear-loads it: the
    // depth texture is minted this frame and has no prior contents — see `depth_attachment_for`.)
    use hl_gpu::protocol::model::enums::LoadOp;
    let depth_loads = ops
        .iter()
        .filter_map(|e| match e {
            Enc::BeginRenderPass { depth, .. } => depth.as_ref().map(|d| d.load),
            _ => false.then_some(LoadOp::Load),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        depth_loads,
        vec![LoadOp::Clear, LoadOp::Load],
        "the freshly minted plane clear-loads once, then survives the colour clear: {ops:?}"
    );
}

/// The ordinary shape must keep its single-pass lowering: a leading colour clear with no depth/stencil
/// side effect before it still folds into the pass load op and emits no `ClearRect` at all.
#[test]
fn an_ordinary_clear_then_draw_frame_still_folds_into_one_pass() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::clear_color(&mut c, [0.25, 0.5, 0.75, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let ops = submit_ops(&sink.batches[0]);
    assert!(
        !ops.iter().any(|e| matches!(e, Enc::ClearRect { .. })),
        "no rect fill: the clear is the pass load op"
    );
    assert_eq!(
        ops.iter()
            .filter(|e| matches!(e, Enc::BeginRenderPass { .. }))
            .count(),
        1,
        "one pass"
    );
}

/// And a colour clear after DEPTH-writing draws is the same rule: the depth results survive it.
#[test]
fn a_colour_clear_does_not_discard_earlier_depth_writing_draws() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::enable(&mut c, GL_DEPTH_TEST);
    record::depth_mask(&mut c, true);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    record::clear_color(&mut c, [0.0, 0.0, 0.0, 1.0]);
    record::clear_buffers(&mut c, GL_COLOR_BUFFER_BIT);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let ops = submit_ops(&sink.batches[0]);
    assert_eq!(
        ops.iter()
            .filter(|e| matches!(e, Enc::Draw { .. } | Enc::DrawIndexed { .. }))
            .count(),
        2,
        "the depth-writing draw is not discarded: {ops:?}"
    );
}
