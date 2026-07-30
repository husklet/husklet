use super::*;

fn mixed_frame() -> (GlContext, [u32; 2]) {
    let mut context = ctx_64();
    context.set_surface_kind(hl_gl::model::context::SurfaceKind::Offscreen);
    flat_program(&mut context);
    tri_vbo(&mut context, 8);

    let mut targets = [0; 2];
    let mut framebuffers = [0; 2];
    for (target, framebuffer) in targets.iter_mut().zip(&mut framebuffers) {
        *target = context.textures.gen();
        context.active_texture(GL_TEXTURE0);
        record::bind_texture(&mut context, GL_TEXTURE_2D, *target);
        record::tex_image_2d_format(&mut context, 16, 16, &[], TextureFormat::Rgba8Unorm);
        *framebuffer = context.gen_framebuffer();
        record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, *framebuffer);
        record::framebuffer_texture_2d(
            &mut context,
            GL_FRAMEBUFFER,
            GL_COLOR_ATTACHMENT0,
            GL_TEXTURE_2D,
            *target,
            0,
        );
    }

    for framebuffer in [framebuffers[0], framebuffers[1], framebuffers[0]] {
        record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
        record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    }
    (context, targets)
}

fn framebuffer(context: &mut GlContext) -> u32 {
    let texture = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(context, GL_TEXTURE_2D, texture);
    record::tex_image_2d_format(context, 16, 16, &[], TextureFormat::Rgba8Unorm);
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        texture,
        0,
    );
    framebuffer
}

#[test]
fn draw_blit_draw_preserves_exact_gl_call_order() {
    let mut context = ctx_64();
    context.set_surface_kind(hl_gl::model::context::SurfaceKind::Offscreen);
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    let source = framebuffer(&mut context);
    let destination = framebuffer(&mut context);

    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, source);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    record::bind_framebuffer(&mut context, GL_READ_FRAMEBUFFER, source);
    record::bind_framebuffer(&mut context, GL_DRAW_FRAMEBUFFER, destination);
    record::blit_framebuffer(
        &mut context,
        0,
        0,
        16,
        16,
        0,
        0,
        16,
        16,
        GL_COLOR_BUFFER_BIT,
        GL_NEAREST,
    );
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    let frame = hl_gl::service::frame::Frame::build(&mut context).expect("ordered frame lowers");
    let kinds = frame
        .cmds
        .iter()
        .filter_map(|command| match command {
            Cmd::Submit(buffer) => Some(buffer.encoder.iter()),
            _ => None,
        })
        .flatten()
        .filter_map(|operation| match operation {
            Enc::CopyTextureToTexture { .. } | Enc::BlitTexture { .. } => Some("blit"),
            Enc::BeginRenderPass { .. } => Some("draw"),
            _ => None,
        })
        .collect::<Vec<_>>();

    assert_eq!(kinds, ["draw", "blit", "draw"]);
}

#[test]
fn blit_uses_attachment_present_when_call_was_recorded() {
    let mut context = ctx_64();
    context.set_surface_kind(hl_gl::model::context::SurfaceKind::Offscreen);
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    let source = framebuffer(&mut context);
    let destination = framebuffer(&mut context);

    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, source);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    record::bind_framebuffer(&mut context, GL_READ_FRAMEBUFFER, source);
    record::bind_framebuffer(&mut context, GL_DRAW_FRAMEBUFFER, destination);
    record::blit_framebuffer(
        &mut context,
        0,
        0,
        16,
        16,
        0,
        0,
        16,
        16,
        GL_COLOR_BUFFER_BIT,
        GL_NEAREST,
    );

    let replacement = context.textures.gen();
    record::bind_texture(&mut context, GL_TEXTURE_2D, replacement);
    record::tex_image_2d_format(&mut context, 16, 16, &[], TextureFormat::Rgba8Unorm);
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, source);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        replacement,
        0,
    );

    let frame = hl_gl::service::frame::Frame::build(&mut context).expect("ordered frame lowers");
    let draw_target = frame
        .cmds
        .iter()
        .find_map(|command| match command {
            Cmd::Submit(buffer) => buffer.encoder.iter().find_map(|operation| match operation {
                Enc::BeginRenderPass { color, .. } => Some(color[0].texture),
                _ => None,
            }),
            _ => None,
        })
        .expect("source draw target");
    let copied_source = frame
        .cmds
        .iter()
        .find_map(|command| match command {
            Cmd::Submit(buffer) => buffer.encoder.iter().find_map(|operation| match operation {
                Enc::CopyTextureToTexture { src, .. } | Enc::BlitTexture { src, .. } => Some(*src),
                _ => None,
            }),
            _ => None,
        })
        .expect("blit source");

    assert_eq!(copied_source, draw_target);
}

#[test]
fn multi_framebuffer_scissored_clear_remains_an_ordered_fill() {
    let mut context = ctx_64();
    context.set_surface_kind(hl_gl::model::context::SurfaceKind::Offscreen);
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    let first = framebuffer(&mut context);
    let second = framebuffer(&mut context);

    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, first);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, second);
    record::enable(&mut context, GL_SCISSOR_TEST);
    record::scissor(&mut context, [2, 3, 7, 5]);
    record::clear_color(&mut context, [0.25, 0.5, 0.75, 1.0]);
    record::clear(&mut context);

    let frame = hl_gl::service::frame::Frame::build(&mut context).expect("ordered frame lowers");
    assert!(frame.cmds.iter().any(|command| match command {
        Cmd::Submit(buffer) => buffer.encoder.iter().any(|operation| {
            matches!(
                operation,
                Enc::ClearRect {
                    x: 2,
                    y: 8,
                    w: 7,
                    h: 5,
                    color,
                    ..
                } if *color == [0.25, 0.5, 0.75, 1.0]
            )
        }),
        _ => false,
    }));
}

#[test]
fn external_present_target_uses_the_present_row_origin_for_scissor_and_clear() {
    let mut context = ctx_64();
    context.set_surface_kind(hl_gl::model::context::SurfaceKind::Offscreen);
    flat_program(&mut context);
    tri_vbo(&mut context, 8);

    let texture = context.textures.gen();
    context.active_texture(GL_TEXTURE0);
    record::bind_texture(&mut context, GL_TEXTURE_2D, texture);
    record::tex_image_2d_format(&mut context, 16, 16, &[], TextureFormat::Bgra8Unorm);
    let generation = context.textures.get(texture).expect("external texture").gen;
    context.bind_external_target(
        texture,
        generation,
        hl_gpu::protocol::model::descriptor::SurfaceToken::new(19).unwrap(),
    );
    let framebuffer = context.gen_framebuffer();
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, framebuffer);
    record::framebuffer_texture_2d(
        &mut context,
        GL_FRAMEBUFFER,
        GL_COLOR_ATTACHMENT0,
        GL_TEXTURE_2D,
        texture,
        0,
    );
    record::enable(&mut context, GL_SCISSOR_TEST);
    record::scissor(&mut context, [2, 3, 7, 5]);
    record::clear_color(&mut context, [0.25, 0.5, 0.75, 1.0]);
    record::clear(&mut context);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    let frame = hl_gl::service::frame::Frame::build(&mut context).expect("external frame lowers");
    let operations = frame
        .cmds
        .iter()
        .find_map(|command| match command {
            Cmd::Submit(buffer) => Some(buffer.encoder.as_slice()),
            _ => None,
        })
        .expect("external frame submission");
    assert!(operations.iter().any(|operation| {
        matches!(
            operation,
            Enc::ClearRect {
                x: 2,
                y: 3,
                w: 7,
                h: 5,
                ..
            }
        )
    }));
    assert!(operations.iter().any(|operation| {
        matches!(
            operation,
            Enc::SetScissor {
                x: 2,
                y: 3,
                w: 7,
                h: 5,
            }
        )
    }));
}

#[test]
fn flush_preserves_a_b_a_framebuffer_order_and_target_identity() {
    let (mut context, _) = mixed_frame();
    let mut sink = RecordingSink::with_full_caps();
    assert!(swap::flush(&mut context, &mut sink).unwrap());
    let passes: Vec<_> = sink.batches[0]
        .iter()
        .filter_map(|command| match command {
            Cmd::Submit(buffer) => Some(buffer.encoder.iter()),
            _ => None,
        })
        .flatten()
        .filter_map(|operation| match operation {
            Enc::BeginRenderPass { color, .. } => Some((color[0].texture, color[0].load)),
            _ => None,
        })
        .collect();

    assert_eq!(
        passes.len(),
        3,
        "each contiguous framebuffer run remains a distinct ordered render pass"
    );
    assert_eq!(
        passes[0].0, passes[2].0,
        "returning to framebuffer A reuses A's render target"
    );
    assert_ne!(
        passes[0].0, passes[1].0,
        "framebuffer B retains an independent render target"
    );
    let loads = passes.iter().map(|pass| pass.1).collect::<Vec<_>>();
    assert_eq!(
        loads,
        [LoadOp::Clear, LoadOp::Clear, LoadOp::Load],
        "returning to A preserves its earlier pixels instead of clearing them"
    );
    assert!(
        context.draws().is_empty(),
        "successful flush consumes the frame"
    );
}

#[test]
fn mixed_frame_captures_two_targets_once_after_all_render_passes() {
    let (mut context, targets) = mixed_frame();
    let mut frame = hl_gl::service::frame::Frame::build(&mut context).expect("mixed frame lowers");
    let captures = frame
        .capture_targets(
            &mut context,
            [targets[0], targets[1], targets[0]],
            256 << 20,
        )
        .unwrap();

    assert_eq!(
        captures.len(),
        2,
        "duplicate requests for target A produce one final capture"
    );
    assert_eq!(
        captures
            .iter()
            .map(|capture| capture.target.name)
            .collect::<Vec<_>>(),
        targets,
        "both requested imported-image targets are captured"
    );

    let submissions: Vec<_> = frame
        .cmds
        .iter()
        .filter_map(|command| match command {
            Cmd::Submit(buffer) => Some(&buffer.encoder),
            _ => None,
        })
        .collect();
    assert_eq!(
        submissions.len(),
        4,
        "three ordered render submissions retain their command-buffer boundaries before capture"
    );
    let render_targets = submissions[..3]
        .iter()
        .flat_map(|submission| {
            submission.iter().filter_map(|operation| match operation {
                Enc::BeginRenderPass { color, .. } => Some(color[0].texture),
                _ => None,
            })
        })
        .collect::<Vec<_>>();
    assert_eq!(
        render_targets,
        [
            captures[0].target.texture,
            captures[1].target.texture,
            captures[0].target.texture,
        ],
        "capture assembly preserves A→B→A render order"
    );

    let copied: Vec<_> = submissions[3]
        .iter()
        .filter_map(|operation| match operation {
            Enc::CopyTextureToBuffer { src, .. } => Some(*src),
            _ => None,
        })
        .collect();
    assert_eq!(
        copied,
        captures
            .iter()
            .map(|capture| capture.target.texture)
            .collect::<Vec<_>>(),
        "one final copy is appended for each imported target"
    );
}

#[test]
fn failed_capture_plan_restores_lowering_state_for_identical_retry() {
    let (mut baseline, baseline_targets) = mixed_frame();
    let (baseline_frame, baseline_captures) =
        hl_gl::service::frame::Frame::build_captured(&mut baseline, baseline_targets, 256 << 20)
            .unwrap()
            .expect("baseline frame captures");

    let (mut retried, retry_targets) = mixed_frame();
    assert!(matches!(
        hl_gl::service::frame::Frame::build_captured(&mut retried, retry_targets, 255),
        Err(hl_gpu::GpuError::ResourceLimit(_))
    ));
    assert_eq!(
        retried.draws().len(),
        3,
        "failed capture construction keeps every recorded draw"
    );
    let (retry_frame, retry_captures) =
        hl_gl::service::frame::Frame::build_captured(&mut retried, retry_targets, 256 << 20)
            .unwrap()
            .expect("retry frame captures");

    assert_eq!(
        retry_frame.cmds, baseline_frame.cmds,
        "retry recreates exactly the allocator/cache-dependent command stream"
    );
    assert_eq!(retry_captures, baseline_captures);
}
