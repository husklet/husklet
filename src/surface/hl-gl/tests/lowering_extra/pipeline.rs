use super::*;

// ---------------------------------------------------------------------------------------------------
// pipeline state lowering: blend-equation-separate, cull winding, topology
// ---------------------------------------------------------------------------------------------------

#[test]
fn blend_equation_separate_lowers_distinct_color_and_alpha_ops() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::enable(&mut c, GL_BLEND);
    record::blend_func_separate(
        &mut c,
        GL_SRC_ALPHA,
        GL_ONE_MINUS_SRC_ALPHA,
        GL_ONE,
        GL_ZERO,
    );
    record::blend_equation_separate(&mut c, GL_FUNC_SUBTRACT, GL_MIN);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let blend = pipeline_desc(&sink.batches[0]).color_targets[0]
        .blend
        .clone()
        .expect("blend state");
    // op wire: FUNC_SUBTRACT -> 1, MIN -> 3 (from frame::blend_op_wire).
    assert_eq!(blend.op_color, 1, "color equation = FUNC_SUBTRACT");
    assert_eq!(blend.op_alpha, 3, "alpha equation = MIN");
    // src_alpha factor wire: SRC_ALPHA -> 4.
    assert_eq!(blend.src_color, 4);
}

#[test]
fn constant_blend_factors_emit_the_draw_time_blend_color() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::enable(&mut c, GL_BLEND);
    record::blend_func(&mut c, 0x8001, 0x8002); // CONSTANT_COLOR, ONE_MINUS_CONSTANT_COLOR
    record::blend_color(&mut c, [-1.0, 0.25, 0.75, 2.0]);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let batch = &sink.batches[0];
    let blend = pipeline_desc(batch).color_targets[0]
        .blend
        .as_ref()
        .expect("blend state");
    assert_eq!((blend.src_color, blend.dst_color), (11, 12));
    assert!(submit_ops(batch).iter().any(|op| matches!(
        op,
        Enc::SetBlendConstant { color } if *color == [0.0, 0.25, 0.75, 1.0]
    )));
}

#[test]
fn dual_source_blend_factors_keep_their_exact_protocol_meaning() {
    use hl_gpu::protocol::model::enums::blend_factor;

    let cases = [
        (GL_SRC1_COLOR, blend_factor::SRC1_COLOR),
        (GL_ONE_MINUS_SRC1_COLOR, blend_factor::ONE_MINUS_SRC1_COLOR),
        (GL_SRC1_ALPHA, blend_factor::SRC1_ALPHA),
        (GL_ONE_MINUS_SRC1_ALPHA, blend_factor::ONE_MINUS_SRC1_ALPHA),
    ];

    for (gl_factor, expected) in cases {
        let mut c = ctx();
        let mut sink = RecordingSink::with_full_caps();
        setup_geometry(&mut c);
        record::enable(&mut c, GL_BLEND);
        record::blend_func(&mut c, gl_factor, GL_ZERO);
        record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

        swap::swap_buffers(&mut c, &mut sink).unwrap();
        let blend = pipeline_desc(&sink.batches[0]).color_targets[0]
            .blend
            .as_ref()
            .expect("blend state");
        assert_eq!(blend.src_color, expected, "GL factor {gl_factor:#x}");
        assert_eq!(blend.src_alpha, expected, "GL factor {gl_factor:#x}");
    }
}

#[test]
fn cull_and_front_face_and_topology_lower_into_the_pipeline() {
    let mut c = ctx();
    // Isolate fixed-function winding from the additional reflection used by presentation targets.
    c.set_surface_kind(hl_gl::model::context::SurfaceKind::Offscreen);
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::enable(&mut c, GL_CULL_FACE);
    record::cull_face(&mut c, GL_FRONT);
    record::front_face(&mut c, GL_CW);
    record::draw_arrays(&mut c, GL_TRIANGLE_STRIP, 0, 4);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let pipe = pipeline_desc(&sink.batches[0]);
    assert_eq!(pipe.cull, 1, "GL_FRONT cull -> 1");
    assert_eq!(pipe.front_face, 1, "GL_CW winding -> 1");
    assert_eq!(
        pipe.topology,
        Topology::TriangleStrip,
        "GL_TRIANGLE_STRIP -> TriangleStrip"
    );
}

#[test]
fn no_cull_when_disabled() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    swap::swap_buffers(&mut c, &mut sink).unwrap();
    assert_eq!(
        pipeline_desc(&sink.batches[0]).cull,
        0,
        "cull disabled by default"
    );
}

// ---------------------------------------------------------------------------------------------------
// glBlendEquation (non-separate) sets the SAME op for color + alpha
// ---------------------------------------------------------------------------------------------------

#[test]
fn blend_equation_lowers_same_op_for_color_and_alpha() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::enable(&mut c, GL_BLEND);
    record::blend_func(&mut c, GL_ONE, GL_ONE);
    record::blend_equation(&mut c, GL_FUNC_REVERSE_SUBTRACT);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let blend = pipeline_desc(&sink.batches[0]).color_targets[0]
        .blend
        .clone()
        .expect("blend state");
    // op wire: FUNC_REVERSE_SUBTRACT -> 2 (frame::blend_op_wire); glBlendEquation sets BOTH ops.
    assert_eq!(blend.op_color, 2, "color equation = FUNC_REVERSE_SUBTRACT");
    assert_eq!(
        blend.op_alpha, 2,
        "alpha equation = FUNC_REVERSE_SUBTRACT (same, non-separate)"
    );
}

// ---------------------------------------------------------------------------------------------------
// glCullFace(GL_FRONT_AND_BACK) discards every triangle, whichever way it winds. WebGPU's cull mode names
// one face, so the pipeline mapping cannot express it; the draw must be dropped instead of lowered as a
// back-face cull, which removed a triangle only when its winding happened to make it a back face.
// ---------------------------------------------------------------------------------------------------

/// Lower one triangle at `winding` with `GL_CULL_FACE` enabled and `cull_face`, and report whether any
/// draw reached the encoder.
fn triangle_survives_cull(cull_face: u32, winding: u32) -> bool {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::enable(&mut c, GL_CULL_FACE);
    record::cull_face(&mut c, cull_face);
    record::front_face(&mut c, winding);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    if !swap::swap_buffers(&mut c, &mut sink).unwrap() {
        return false;
    }
    sink.batches[0]
        .iter()
        .any(|cmd| matches!(cmd, Cmd::CreateRenderPipeline(..)))
}

#[test]
fn cull_front_and_back_discards_triangles_of_either_winding() {
    for winding in [GL_CCW, GL_CW] {
        assert!(
            !triangle_survives_cull(GL_FRONT_AND_BACK, winding),
            "GL_FRONT_AND_BACK must discard the triangle at winding {winding:#x}"
        );
    }
}

#[test]
fn single_face_cull_still_lowers_a_pipeline() {
    // The neighbouring modes must be untouched: GL_BACK/GL_FRONT are ordinary pipeline cull state and
    // whether a given triangle survives is the rasterizer's business, not the lowering's.
    for face in [GL_BACK, GL_FRONT] {
        assert!(
            triangle_survives_cull(face, GL_CCW),
            "cull face {face:#x} must still lower a pipeline"
        );
    }
}

#[test]
fn cull_front_and_back_leaves_points_and_lines_alone() {
    // GL culls triangles only (ES 3.0 §3.6.1); points and lines are never culled.
    for mode in [GL_POINTS, GL_LINES] {
        let mut c = ctx();
        let mut sink = RecordingSink::with_full_caps();
        setup_geometry(&mut c);
        record::enable(&mut c, GL_CULL_FACE);
        record::cull_face(&mut c, GL_FRONT_AND_BACK);
        record::draw_arrays(&mut c, mode, 0, 3);
        assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
        assert!(
            sink.batches[0]
                .iter()
                .any(|cmd| matches!(cmd, Cmd::CreateRenderPipeline(..))),
            "mode {mode:#x} is not a triangle and must survive GL_FRONT_AND_BACK"
        );
    }
}
