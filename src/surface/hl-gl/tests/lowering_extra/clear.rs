use super::*;

// ---------------------------------------------------------------------------------------------------
// glClearBufferfv → a scoped clear pass
// ---------------------------------------------------------------------------------------------------

#[test]
fn clear_buffer_color_lowers_to_a_clear_pass() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    record::clear_buffer_color(&mut c, 0, [0.25, 0.5, 0.75, 1.0]);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let ops = submit_ops(&sink.batches[0]);
    match &ops[0] {
        Enc::BeginRenderPass { color, .. } => {
            assert_eq!(color[0].clear, [0.25, 0.5, 0.75, 1.0]);
        }
        other => panic!("expected a clear pass, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------------------------------
// The depth/stencil clear VALUE belongs to the recorded clear, and glDepthMask gates it
// ---------------------------------------------------------------------------------------------------

fn pass_depth_clear(batch: &[Cmd]) -> Option<(f32, u32)> {
    submit_ops(batch).iter().find_map(|e| match e {
        Enc::BeginRenderPass { depth, .. } => {
            depth.as_ref().map(|d| (d.clear_depth, d.clear_stencil))
        }
        _ => None,
    })
}

#[test]
fn depth_clear_uses_the_value_in_force_at_the_clear_not_at_lowering() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::depth_mask(&mut c, true);
    record::clear_depth(&mut c, 0.5);
    record::clear_buffers(&mut c, GL_DEPTH_BUFFER_BIT);
    // glClearDepthf is ordinary state; moving it AFTER the clear must not retroactively change the clear.
    record::clear_depth(&mut c, 1.0);
    record::enable(&mut c, GL_DEPTH_TEST);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let (clear_depth, _) = pass_depth_clear(&sink.batches[0]).expect("a depth attachment");
    assert_eq!(clear_depth, 0.5, "the clear recorded at 0.5 must clear to 0.5");
}

#[test]
fn a_masked_off_depth_clear_leaves_the_plane_at_the_earlier_clears_value() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    record::depth_mask(&mut c, true);
    record::clear_depth(&mut c, 0.5);
    record::clear_buffers(&mut c, GL_DEPTH_BUFFER_BIT);
    // glDepthMask(GL_FALSE) makes a depth clear a no-op (ES 3.0 §4.2.3), so the 1.0 never lands.
    record::depth_mask(&mut c, false);
    record::clear_depth(&mut c, 1.0);
    record::clear_buffers(&mut c, GL_DEPTH_BUFFER_BIT);
    record::depth_mask(&mut c, true);
    record::enable(&mut c, GL_DEPTH_TEST);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let (clear_depth, _) = pass_depth_clear(&sink.batches[0]).expect("a depth attachment");
    assert_eq!(
        clear_depth, 0.5,
        "the masked-off clear must not overwrite the 0.5 the open-masked clear left"
    );
}

// ---------------------------------------------------------------------------------------------------
// An FBO with no depth attachment: every depth test passes and nothing is written (ES 3.0 §4.1.5)
// ---------------------------------------------------------------------------------------------------

/// Bind a fresh FBO with one sized RGBA colour texture and, when asked, a depth renderbuffer.
fn bind_offscreen(c: &mut GlContext, with_depth: bool) {
    let tex = c.textures.gen();
    record::bind_texture(c, GL_TEXTURE_2D, tex);
    record::tex_image_2d(c, 64, 64, &[]);
    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(c, GL_FRAMEBUFFER, fbo);
    record::framebuffer_texture_2d(c, GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
    if with_depth {
        let rbo = c.gen_renderbuffer();
        record::bind_renderbuffer(c, GL_RENDERBUFFER, rbo);
        record::renderbuffer_storage(c, GL_RENDERBUFFER, GL_DEPTH_COMPONENT16, 64, 64);
        record::framebuffer_renderbuffer(c, GL_FRAMEBUFFER, GL_DEPTH_ATTACHMENT, GL_RENDERBUFFER, rbo);
    }
}

fn depth_state_present(with_depth: bool) -> bool {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    bind_offscreen(&mut c, with_depth);
    record::enable(&mut c, GL_DEPTH_TEST);
    record::depth_func(&mut c, GL_LESS);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    swap::swap_buffers(&mut c, &mut sink).unwrap();
    pipeline_desc(&sink.batches[0]).depth.is_some()
}

#[test]
fn depth_test_on_a_depthless_fbo_carries_no_depth_state() {
    assert!(
        !depth_state_present(false),
        "with no GL_DEPTH_ATTACHMENT the depth test must always pass, so no depth state is lowered"
    );
}

#[test]
fn depth_test_on_an_fbo_with_a_depth_attachment_still_tests() {
    assert!(
        depth_state_present(true),
        "an attached depth renderbuffer must keep the depth test armed"
    );
}
