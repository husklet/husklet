use super::*;
use hl_gpu::protocol::model::enums::TextureFormat;

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

fn bind_mrt_formats(c: &mut GlContext, formats: &[TextureFormat]) -> u32 {
    let fbo = c.gen_framebuffer();
    record::bind_framebuffer(c, GL_FRAMEBUFFER, fbo);
    for (index, &format) in formats.iter().enumerate() {
        let tex = c.textures.gen();
        record::bind_texture(c, GL_TEXTURE_2D, tex);
        record::tex_image_2d_format(c, 32, 32, &[], format);
        record::framebuffer_texture_2d(
            c,
            GL_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0 + index as u32,
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

#[test]
fn mrt_pipeline_preserves_each_attachment_format() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    let formats = [
        TextureFormat::Rgba8Unorm,
        TextureFormat::Rgba16Float,
        TextureFormat::Rgba8Srgb,
        TextureFormat::Rgba32Float,
    ];
    bind_mrt_formats(&mut c, &formats);
    record::draw_buffers(
        &mut c,
        &[
            GL_COLOR_ATTACHMENT0,
            GL_COLOR_ATTACHMENT0 + 1,
            GL_COLOR_ATTACHMENT0 + 2,
            GL_COLOR_ATTACHMENT0 + 3,
        ],
    );
    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);

    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let actual: Vec<_> = pipeline_desc(&sink.batches[0])
        .color_targets
        .iter()
        .map(|target| target.format)
        .collect();
    assert_eq!(actual, formats);
}

#[test]
fn four_draw_buffers_keep_independent_blend_mask_equation_and_factor_state() {
    let mut c = ctx();
    let mut sink = RecordingSink::with_full_caps();
    setup_geometry(&mut c);
    bind_mrt(&mut c, 4);
    record::draw_buffers(
        &mut c,
        &[
            GL_COLOR_ATTACHMENT0,
            GL_COLOR_ATTACHMENT0 + 1,
            GL_COLOR_ATTACHMENT0 + 2,
            GL_COLOR_ATTACHMENT0 + 3,
        ],
    );

    let enabled = [false, true, false, true];
    let masks = [0x1u32, 0x3, 0x5, 0xf];
    let src_rgb = [GL_ZERO, GL_ONE, GL_SRC_ALPHA, GL_DST_ALPHA];
    let dst_rgb = [GL_ONE, GL_ZERO, GL_ONE_MINUS_SRC_ALPHA, GL_ONE_MINUS_DST_ALPHA];
    let src_alpha = [GL_ONE, GL_ZERO, GL_DST_ALPHA, GL_SRC_ALPHA];
    let dst_alpha = [GL_ZERO, GL_ONE, GL_ONE_MINUS_DST_ALPHA, GL_ONE_MINUS_SRC_ALPHA];
    let eq_rgb = [GL_FUNC_ADD, GL_FUNC_SUBTRACT, GL_FUNC_REVERSE_SUBTRACT, GL_MIN];
    let eq_alpha = [GL_MAX, GL_FUNC_REVERSE_SUBTRACT, GL_FUNC_SUBTRACT, GL_FUNC_ADD];
    for index in 0..4usize {
        record::set_cap_indexed(&mut c, GL_BLEND, index as u32, enabled[index]);
        record::color_mask_indexed(
            &mut c,
            index as u32,
            masks[index] & 1 != 0,
            masks[index] & 2 != 0,
            masks[index] & 4 != 0,
            masks[index] & 8 != 0,
        );
        record::blend_func_separate_indexed(
            &mut c,
            index as u32,
            src_rgb[index],
            dst_rgb[index],
            src_alpha[index],
            dst_alpha[index],
        );
        record::blend_equation_separate_indexed(
            &mut c,
            index as u32,
            eq_rgb[index],
            eq_alpha[index],
        );
        assert_eq!(
            hl_gl::service::query::is_enabled_indexed(&c, GL_BLEND, index as u32),
            Some(enabled[index])
        );
        let mut mask = [0; 4];
        assert_eq!(
            hl_gl::service::query::get_boolean_indexed(
                &c,
                GL_COLOR_WRITEMASK,
                index as u32,
                &mut mask,
            ),
            4
        );
        assert_eq!(
            mask,
            std::array::from_fn(|channel| u8::from(masks[index] & (1 << channel) != 0))
        );
        for (pname, expected) in [
            (GL_BLEND_SRC_RGB, src_rgb[index]),
            (GL_BLEND_DST_RGB, dst_rgb[index]),
            (GL_BLEND_SRC_ALPHA_STATE, src_alpha[index]),
            (GL_BLEND_DST_ALPHA, dst_alpha[index]),
            (GL_BLEND_EQUATION_RGB, eq_rgb[index]),
            (GL_BLEND_EQUATION_ALPHA, eq_alpha[index]),
        ] {
            assert_eq!(
                hl_gl::service::query::get_integer_indexed(&c, pname, index as u32),
                i64::from(expected)
            );
        }
    }

    record::draw_arrays(&mut c, GL_TRIANGLES, 0, 3);
    assert!(swap::swap_buffers(&mut c, &mut sink).unwrap());
    let targets = &pipeline_desc(&sink.batches[0]).color_targets;
    assert_eq!(targets.len(), 4);
    let src_rgb_wire = [0, 1, 4, 8];
    let dst_rgb_wire = [1, 0, 5, 9];
    let src_alpha_wire = [1, 0, 8, 4];
    let dst_alpha_wire = [0, 1, 9, 5];
    let eq_rgb_wire = [0, 1, 2, 3];
    let eq_alpha_wire = [4, 2, 1, 0];
    for index in 0..4 {
        assert_eq!(targets[index].blend.is_some(), enabled[index]);
        assert_eq!(targets[index].write_mask, masks[index]);
        if let Some(blend) = &targets[index].blend {
            assert_eq!(blend.src_color, src_rgb_wire[index]);
            assert_eq!(blend.dst_color, dst_rgb_wire[index]);
            assert_eq!(blend.src_alpha, src_alpha_wire[index]);
            assert_eq!(blend.dst_alpha, dst_alpha_wire[index]);
            assert_eq!(blend.op_color, eq_rgb_wire[index]);
            assert_eq!(blend.op_alpha, eq_alpha_wire[index]);
        }
    }
}

#[test]
fn indexed_draw_buffer_state_rejects_bad_targets_and_indices_without_mutation() {
    let mut c = ctx();
    record::color_mask_indexed(&mut c, 4, false, false, false, false);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
    let mut mask = [0; 4];
    assert_eq!(
        hl_gl::service::query::get_boolean_indexed(&c, GL_COLOR_WRITEMASK, 0, &mut mask),
        4
    );
    assert_eq!(mask, [1, 1, 1, 1]);

    record::set_cap_indexed(&mut c, GL_DEPTH_TEST, 0, true);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    assert_eq!(
        hl_gl::service::query::is_enabled_indexed(&c, GL_BLEND, 0),
        Some(false)
    );
    assert_eq!(
        hl_gl::service::query::is_enabled_indexed(&c, GL_BLEND, 4),
        None
    );

    record::blend_func_indexed(&mut c, 0, 0xdead_beef, GL_ONE);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    assert_eq!(
        hl_gl::service::query::get_integer_indexed(&c, GL_BLEND_SRC_RGB, 0),
        i64::from(GL_ONE)
    );
    record::blend_func_indexed(&mut c, 0, GL_ONE, GL_SRC_ALPHA_SATURATE);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    assert_eq!(
        hl_gl::service::query::get_integer_indexed(&c, GL_BLEND_DST_RGB, 0),
        i64::from(GL_ZERO)
    );
    record::blend_equation_separate_indexed(&mut c, 0, GL_FUNC_SUBTRACT, 0xdead_beef);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    assert_eq!(
        hl_gl::service::query::get_integer_indexed(&c, GL_BLEND_EQUATION_RGB, 0),
        i64::from(GL_FUNC_ADD)
    );

    // Enumerants are validated before the index, matching the extension command's error precedence.
    record::blend_func_indexed(&mut c, 4, 0xdead_beef, GL_ONE);
    assert_eq!(c.take_gl_error(), GL_INVALID_ENUM);
    record::blend_func_indexed(&mut c, 4, GL_ONE, GL_ZERO);
    assert_eq!(c.take_gl_error(), GL_INVALID_VALUE);
}

#[test]
fn global_draw_buffer_setters_broadcast_over_independent_state() {
    let mut c = ctx();
    for index in 0..4 {
        record::set_cap_indexed(&mut c, GL_BLEND, index, index % 2 == 0);
        record::color_mask_indexed(&mut c, index, false, false, false, false);
        record::blend_func_indexed(&mut c, index, GL_ZERO, GL_ONE);
        record::blend_equation_indexed(&mut c, index, GL_FUNC_SUBTRACT);
    }

    record::enable(&mut c, GL_BLEND);
    record::color_mask(&mut c, true, false, true, false);
    record::blend_func(&mut c, GL_SRC_ALPHA, GL_ONE_MINUS_SRC_ALPHA);
    record::blend_equation_separate(&mut c, GL_MAX, GL_FUNC_REVERSE_SUBTRACT);

    for index in 0..4 {
        assert_eq!(
            hl_gl::service::query::is_enabled_indexed(&c, GL_BLEND, index),
            Some(true)
        );
        let mut mask = [0; 4];
        assert_eq!(
            hl_gl::service::query::get_boolean_indexed(
                &c,
                GL_COLOR_WRITEMASK,
                index,
                &mut mask,
            ),
            4
        );
        assert_eq!(mask, [1, 0, 1, 0]);
        for (pname, expected) in [
            (GL_BLEND_SRC_RGB, GL_SRC_ALPHA),
            (GL_BLEND_DST_RGB, GL_ONE_MINUS_SRC_ALPHA),
            (GL_BLEND_SRC_ALPHA_STATE, GL_SRC_ALPHA),
            (GL_BLEND_DST_ALPHA, GL_ONE_MINUS_SRC_ALPHA),
            (GL_BLEND_EQUATION_RGB, GL_MAX),
            (GL_BLEND_EQUATION_ALPHA, GL_FUNC_REVERSE_SUBTRACT),
        ] {
            assert_eq!(
                hl_gl::service::query::get_integer_indexed(&c, pname, index),
                i64::from(expected)
            );
        }
    }
}
