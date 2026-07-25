use super::*;

#[test]
fn gl_recording_submits_nothing() {
    let mut c = ctx_640x480();
    let sink = RecordingSink::with_full_caps();

    let vbo = c.buffers.gen();
    record::bind_buffer(&mut c, GL_ARRAY_BUFFER, vbo);
    record::buffer_data(&mut c, GL_ARRAY_BUFFER, &[1u8; 48], 0x88E4);

    // Not one command was submitted — GL only emits IR at swap.
    assert!(sink.batches.is_empty());
    assert!(c.buffers.has_data(vbo));
}

#[test]
fn empty_swap_presents_nothing() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    // No draws recorded → swap is a no-op, submits nothing, returns false.
    assert!(!swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert!(sink.batches.is_empty());
}

// ---------------------------------------------------------------------------------------------------
// clear-only frame
// ---------------------------------------------------------------------------------------------------

#[test]
fn clear_only_frame_lowers_to_clear_pass_and_present() {
    let mut c = ctx_640x480();
    let mut sink = RecordingSink::with_full_caps();
    record::clear_color(&mut c, [0.1, 0.2, 0.3, 1.0]);
    record::clear(&mut c);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    assert_eq!(sink.batches.len(), 1);
    let batch = &sink.batches[0];

    // default render target + surface created once.
    assert!(matches!(batch[0], Cmd::CreateTexture(1, _)));
    assert!(matches!(batch[1], Cmd::CreateSurface(1, _)));

    // the render pass clears the default target to the recorded color.
    let ops = submit_ops(batch);
    match &ops[0] {
        Enc::BeginRenderPass { color, depth } => {
            assert!(depth.is_none());
            assert_eq!(color.len(), 1);
            assert_eq!(color[0].texture, 1);
            assert_eq!(color[0].load, LoadOp::Clear);
            assert_eq!(color[0].clear, [0.1, 0.2, 0.3, 1.0]);
        }
        other => panic!("expected BeginRenderPass, got {other:?}"),
    }
    assert!(matches!(ops[1], Enc::EndRenderPass));

    // present the rendered target through its surface.
    assert_eq!(
        *batch.last().unwrap(),
        Cmd::Present {
            surface: 1,
            texture: 1
        }
    );
}

// ---------------------------------------------------------------------------------------------------
// single textured-quad draw — the full core path
// ---------------------------------------------------------------------------------------------------
