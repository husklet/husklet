use super::*;

// ---------------------------------------------------------------------------------------------------
// Multiple render targets: which attachment a scoped clear reaches, and which ones a draw may write.
//
// Both defects here presented as "the index is ignored": `glClearBufferfv(GL_COLOR, 1, …)` cleared
// attachment 0 (or nothing), and a `GL_NONE` entry in `glDrawBuffers` still received the draw.
// ---------------------------------------------------------------------------------------------------

/// A bound FBO with `n` sized RGBA colour attachments, `GL_COLOR_ATTACHMENT0..n`.
fn bind_mrt(c: &mut GlContext, n: u32) -> u32 {
    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(c, GL_FRAMEBUFFER, fbo);
    for index in 0..n {
        let tex = c.textures.gen();
        record::bind_texture(c, GL_TEXTURE_2D, tex);
        record::tex_image_2d(c, 32, 32, &[]);
        record::framebuffer_texture_2d(
            c,
            GL_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0 + index,
            GL_TEXTURE_2D,
            tex,
            0,
        );
    }
    fbo
}

fn pass_color(batch: &[Cmd]) -> Vec<(hl_gpu::protocol::model::enums::LoadOp, [f64; 4])> {
    submit_ops(batch)
        .iter()
        .find_map(|e| match e {
            Enc::BeginRenderPass { color, .. } => {
                Some(color.iter().map(|a| (a.load, a.clear)).collect())
            }
            _ => None,
        })
        .expect("a BeginRenderPass")
}

#[test]
fn clear_buffer_fv_clears_the_attachment_it_names_and_no_other() {
    use hl_gpu::protocol::model::enums::LoadOp;
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    bind_mrt(&mut c, 3);
    record::draw_buffers(
        &mut c,
        &[
            GL_COLOR_ATTACHMENT0,
            GL_COLOR_ATTACHMENT0 + 1,
            GL_COLOR_ATTACHMENT0 + 2,
        ],
    );
    record::clear_buffer_color(&mut c, 0, [0.25, 0.5, 0.75, 1.0]);
    record::clear_buffer_color(&mut c, 1, [1.0, 0.0, 0.0, 1.0]);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let color = pass_color(&sink.batches[0]);
    assert_eq!(color.len(), 3, "three attachments in one pass");
    assert_eq!(color[0], (LoadOp::Clear, [0.25, 0.5, 0.75, 1.0]));
    assert_eq!(color[1], (LoadOp::Clear, [1.0, 0.0, 0.0, 1.0]));
    assert_eq!(
        color[2].0,
        LoadOp::Load,
        "attachment 2 was never named by a clear and must keep its contents"
    );
}

#[test]
fn draw_buffers_none_gives_that_slot_a_zero_write_mask() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    bind_mrt(&mut c, 3);
    record::draw_buffers(
        &mut c,
        &[GL_COLOR_ATTACHMENT0, GL_NONE, GL_COLOR_ATTACHMENT0 + 2],
    );
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    let targets = &pipeline_desc(&sink.batches[0]).color_targets;
    assert_eq!(targets.len(), 3);
    assert_eq!(targets[0].write_mask, 0xf, "slot 0 is selected");
    assert_eq!(targets[1].write_mask, 0, "GL_NONE writes nothing");
    assert_eq!(targets[2].write_mask, 0xf, "slot 2 is selected");
}

#[test]
fn every_slot_writes_when_draw_buffers_was_never_called() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    bind_mrt(&mut c, 2);
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    swap::swap_buffers(&mut c, &mut sink).unwrap();
    for target in &pipeline_desc(&sink.batches[0]).color_targets {
        assert_eq!(target.write_mask, 0xf, "the initial selection writes every slot");
    }
}
