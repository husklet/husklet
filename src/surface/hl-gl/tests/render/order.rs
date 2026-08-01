use super::*;
use hl_gpu::protocol::model::descriptor::Mirror;

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

/// A MIRRORED `glBlitFramebuffer` reaches the IR as a mirror, and it takes the resampling path.
///
/// GL flips a blit by inverting a rect: `srcX1 < srcX0` reverses the destination's column order. The
/// lowering already normalises both rects with a min/max — it has to, since the IR's origin and extent are
/// unsigned — and it threw the comparison away, so a mirrored blit produced an UNMIRRORED image with no
/// error anywhere. It also has to leave the exact-copy path, because `CopyTextureToTexture` moves bytes
/// and cannot reflect them: equal extents and matching formats are no longer sufficient for a copy.
///
/// The y axis is the subtle one and is asserted rather than assumed: both rects get the same bottom-left
/// to top-left reflection, so that reflection cancels out of the NET flip and an ordinary (uninverted)
/// blit must still record `Mirror::NONE`.
#[test]
fn a_mirrored_blit_lowers_to_a_mirrored_blit_texture() {
    // `(src rect, dst rect)` in GL window coordinates, and the net mirror each must produce.
    let cases: [([i32; 4], [i32; 4], Mirror); 5] = [
        ([0, 0, 16, 16], [0, 0, 16, 16], Mirror::NONE),
        ([16, 0, 0, 16], [0, 0, 16, 16], Mirror { x: true, y: false }),
        ([0, 16, 16, 0], [0, 0, 16, 16], Mirror { x: false, y: true }),
        ([16, 16, 0, 0], [0, 0, 16, 16], Mirror { x: true, y: true }),
        // Both sides inverted on x: two reflections are the identity, so this is NOT a mirror.
        ([16, 0, 0, 16], [16, 0, 0, 16], Mirror::NONE),
    ];

    for (src_rect, dst_rect, want) in cases {
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
            src_rect[0],
            src_rect[1],
            src_rect[2],
            src_rect[3],
            dst_rect[0],
            dst_rect[1],
            dst_rect[2],
            dst_rect[3],
            GL_COLOR_BUFFER_BIT,
            GL_NEAREST,
        );

        let frame = hl_gl::service::frame::Frame::build(&mut context).expect("the frame lowers");
        let transfers: Vec<&Enc> = frame
            .cmds
            .iter()
            .filter_map(|command| match command {
                Cmd::Submit(buffer) => Some(buffer.encoder.iter()),
                _ => None,
            })
            .flatten()
            .filter(|op| {
                matches!(
                    op,
                    Enc::CopyTextureToTexture { .. } | Enc::BlitTexture { .. }
                )
            })
            .collect();
        assert_eq!(
            transfers.len(),
            1,
            "{src_rect:?} -> {dst_rect:?} must lower to exactly one transfer, got {transfers:?}"
        );

        if want == Mirror::NONE {
            // Equal extents, matching formats, no net flip: still the exact byte copy it always was.
            assert!(
                matches!(transfers[0], Enc::CopyTextureToTexture { .. }),
                "{src_rect:?} -> {dst_rect:?} carries no net flip and must stay an exact copy"
            );
            continue;
        }
        let Enc::BlitTexture { mirror, .. } = transfers[0] else {
            panic!(
                "{src_rect:?} -> {dst_rect:?} is mirrored, so it must take the resampling path (an \
                 exact copy cannot reflect), got {:?}",
                transfers[0]
            );
        };
        assert_eq!(
            *mirror, want,
            "{src_rect:?} -> {dst_rect:?} must carry the net per-axis flip"
        );
    }
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
    // An imported external image is the ONE target stored in GL texel order (row 0 = the framebuffer's
    // bottom), because the guest owns what the foreign consumer reads out of that memory. So it — and only
    // it — carries the clip reflection and the reversed winding that implies.
    let vertex = frame
        .cmds
        .iter()
        .filter_map(|command| match command {
            Cmd::CreateShader { spirv, .. } => {
                hl_gpu::protocol::model::kernel::GlslDescriptor::from_words(spirv)
                    .and_then(Result::ok)
                    .filter(|shader| {
                        shader.stage == hl_gpu::protocol::model::kernel::glsl_stage::VERTEX
                    })
            }
            _ => None,
        })
        .next()
        .expect("external target vertex shader");
    assert_eq!(
        vertex
            .source
            .matches("gl_Position.y = -gl_Position.y")
            .count(),
        1,
        "an imported external image receives exactly one row-origin conversion"
    );
    let pipeline = frame
        .cmds
        .iter()
        .find_map(|command| match command {
            Cmd::CreateRenderPipeline(_, descriptor) => Some(descriptor),
            _ => None,
        })
        .expect("external target pipeline");
    assert_eq!(
        pipeline.front_face, 1,
        "the clip reflection swaps GL's default CCW winding"
    );
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

// ---------------------------------------------------------------------------------------------------
// A frame whose only recorded work is a BLIT must still be flushed
// ---------------------------------------------------------------------------------------------------

/// `swap::flush` — the path `glFlush`, `glFinish` and `glReadPixels` all take — returned early whenever the
/// draw list was empty, without consulting the recorded BLITS. `Frame::build` guards on both
/// (`draws.is_empty() && blits.is_empty()`), so the builder was always willing; the boundary never asked it.
///
/// A `glBlitFramebuffer` followed by a `glReadPixels` of the destination therefore read the destination as
/// it was BEFORE the blit. On an offscreen or pbuffer context there is no `eglSwapBuffers` to rescue it
/// later, so the blit simply never executed.
#[test]
fn a_blit_only_frame_is_flushed() {
    let mut context = ctx_64();
    context.set_surface_kind(hl_gl::model::context::SurfaceKind::Offscreen);
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    let source = framebuffer(&mut context);
    let destination = framebuffer(&mut context);

    // Give the source real content in its own frame, so the blit below is the ONLY thing recorded.
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, source);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    let mut sink = RecordingSink::with_full_caps();
    assert!(swap::flush(&mut context, &mut sink).unwrap());
    sink.batches.clear();

    // Now a blit and nothing else — the shape a compositor's copy-then-read takes.
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
    assert_eq!(
        context.recording_counts(),
        (0, 1),
        "the recording holds one blit and no draws — the shape that was dropped"
    );

    assert!(
        swap::flush(&mut context, &mut sink).unwrap(),
        "a recorded blit is work, and the flush must report that it submitted some"
    );
    let copied = sink.batches.iter().flatten().any(|cmd| match cmd {
        Cmd::Submit(batch) => batch.encoder.iter().any(|e| {
            matches!(
                e,
                Enc::CopyTextureToTexture { .. } | Enc::BlitTexture { .. }
            )
        }),
        _ => false,
    });
    assert!(copied, "the blit must reach the encoder: {:?}", sink.batches);
}

/// The WINDOW-surface branch of `swap::flush` has the same disagreement in a different shape. It splits
/// the recording into offscreen work (flushed now) and default-framebuffer work (retained for
/// `eglSwapBuffers`), then bails out when the offscreen DRAW list is empty — without asking whether any
/// offscreen BLIT was recorded, even though the partition immediately below it routes exactly those.
#[test]
fn a_window_frame_flushes_an_offscreen_blit_with_no_offscreen_draw() {
    let mut context = ctx_64();
    context.set_surface_kind(hl_gl::model::context::SurfaceKind::Window);
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    let source = framebuffer(&mut context);
    let destination = framebuffer(&mut context);

    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, source);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    let mut sink = RecordingSink::with_full_caps();
    assert!(swap::flush(&mut context, &mut sink).unwrap());
    sink.batches.clear();

    // An offscreen-to-offscreen blit, plus a default-framebuffer draw that must STAY for the swap.
    record::bind_framebuffer(&mut context, GL_READ_FRAMEBUFFER, source);
    record::bind_framebuffer(&mut context, GL_DRAW_FRAMEBUFFER, destination);
    record::blit_framebuffer(
        &mut context, 0, 0, 16, 16, 0, 0, 16, 16, GL_COLOR_BUFFER_BIT, GL_NEAREST,
    );
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, 0);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    assert!(
        swap::flush(&mut context, &mut sink).unwrap(),
        "the offscreen blit is flushable work even with no offscreen draw beside it"
    );
    let copied = sink.batches.iter().flatten().any(|cmd| match cmd {
        Cmd::Submit(batch) => batch.encoder.iter().any(|e| {
            matches!(e, Enc::CopyTextureToTexture { .. } | Enc::BlitTexture { .. })
        }),
        _ => false,
    });
    assert!(copied, "the blit must reach the encoder: {:?}", sink.batches);
    assert_eq!(
        context.recording_counts().0,
        1,
        "and the default-framebuffer draw must be RETAINED for eglSwapBuffers, not flushed with it"
    );
}

/// A surfaceless EGL context has no default framebuffer, but its user FBOs are fully renderable. The
/// guard says so in its own comment — "reject only work that actually targets framebuffer 0" — and then
/// rejects the WHOLE FRAME the moment any single draw targets it, discarding every offscreen draw beside
/// it.
///
/// That is the exact failure the comment was written to fix, still present one level up: a surfaceless
/// Chrome-shaped context whose command buffer happens to contain one default-framebuffer draw loses all
/// of its FBO work and acknowledges the flush as though it had run.
#[test]
fn a_surfaceless_frame_keeps_its_offscreen_work_beside_a_default_framebuffer_draw() {
    let mut context = ctx_64();
    context.set_surface(hl_gl::model::context::GlSurface {
        have: false,
        width: 0,
        height: 0,
    });
    context.set_surface_kind(hl_gl::model::context::SurfaceKind::Offscreen);
    flat_program(&mut context);
    tri_vbo(&mut context, 8);
    let offscreen = framebuffer(&mut context);

    // Offscreen work, which is renderable, and one default-framebuffer draw, which is not.
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, offscreen);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);
    record::bind_framebuffer(&mut context, GL_FRAMEBUFFER, 0);
    record::draw_arrays(&mut context, GL_TRIANGLES, 0, 3);

    let mut sink = RecordingSink::with_full_caps();
    assert!(
        swap::flush(&mut context, &mut sink).unwrap(),
        "the offscreen draw is renderable and must be submitted"
    );
    let drew = sink.batches.iter().flatten().any(|cmd| match cmd {
        Cmd::Submit(batch) => batch
            .encoder
            .iter()
            .any(|e| matches!(e, Enc::Draw { .. } | Enc::DrawIndexed { .. })),
        _ => false,
    });
    assert!(drew, "the offscreen draw must reach the encoder: {:?}", sink.batches);
}
