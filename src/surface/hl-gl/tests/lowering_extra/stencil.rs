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
