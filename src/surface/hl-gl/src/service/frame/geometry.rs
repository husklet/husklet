use super::*;

fn survivors(draws: &[DrawCall]) -> ([f32; 4], &[DrawCall]) {
    let (clear, start) = RenderPasses::effective_clear(draws);
    (clear, &draws[start..])
}

pub(super) fn build_clear_frame_color(
    ctx: &mut GlContext,
    fbo: u32,
    clear: [f32; 4],
    cmds: Vec<Cmd>,
) -> Frame {
    let snapshot = ctx
        .local
        .recording
        .draws
        .iter()
        .rev()
        .find(|d| d.fbo == fbo)
        .and_then(|d| d.target);
    build_clear_frame_snapshot(
        ctx,
        fbo,
        snapshot,
        clear,
        cmds,
        ctx.local.recording.draws.len(),
    )
}

fn build_clear_frame_snapshot(
    ctx: &mut GlContext,
    fbo: u32,
    snapshot: Option<crate::model::program::TargetSnapshot>,
    clear: [f32; 4],
    mut cmds: Vec<Cmd>,
    draw_count: usize,
) -> Frame {
    let (surface, texture, w, h, fmt) = resolve_target(ctx, fbo, snapshot, &mut cmds);
    let ops = vec![
        Enc::BeginRenderPass {
            color: vec![ColorAttachment {
                texture,
                load: LoadOp::Clear,
                clear,
                store: true,
            }],
            depth: None,
        },
        Enc::EndRenderPass,
    ];
    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: ops,
        signal: None,
    }));
    log_frame(w, h, draw_count, 1, cmds.len(), Frame::upload_bytes(&cmds));
    Frame {
        cmds,
        present: (surface, texture),
        target_width: w,
        target_height: h,
        target_format: fmt,
        color_attachments: Vec::new(),
        targets: frame_target(ctx, fbo, snapshot, texture, w, h, fmt)
            .into_iter()
            .collect(),
    }
}

/// The per-draw lowering result: the texture staging copies (hoisted before `BeginRenderPass`) and the
/// in-pass encoder ops (`SetPipeline` … `Draw`) for one geometry draw.
pub(super) struct DrawCommands {
    pub(super) copies: Vec<Enc>,
    pub(super) ops: Vec<Enc>,
}

/// Geometry frame: clear once, then replay every geometry draw into a single render pass over the target.
/// Handles single-draw, multi-draw, and clear-then-draw, against the default surface or an offscreen FBO.
impl Frame {
    pub(super) fn build_geometry(ctx: &mut GlContext) -> Option<Frame> {
        // Lower against an owned draw list kept outside `ctx`: lowering mutates residency and allocation
        // state, so borrowing `ctx.local.recording.draws` directly is impossible. Moving it avoids the former two deep
        // DrawCall clones while preserving the recorded list for retry/cleanup after this attempt.
        let draws = std::mem::take(&mut ctx.local.recording.draws);
        let frame = Self::build_geometry_from(ctx, &draws);
        ctx.local.recording.draws = draws;
        frame
    }

    fn build_geometry_from(ctx: &mut GlContext, draws: &[DrawCall]) -> Option<Frame> {
        // An unscissored full-framebuffer `glClear` wipes all prior rendering, so the effective pass clear is the
        // LAST such clear's color and only draws recorded AFTER it survive (see [`effective_clear`]). Chrome's
        // window frame clears the default framebuffer to `#ff7700` (the page background) partway through its
        // draw-list, then composites tiles — replaying the wiped pre-clear draws or using the leading transparent
        // clear (the old behavior) dropped that background and read back blank.
        let (clear, survivors) = survivors(draws);
        // Only an UNSCISSORED clear may clear-load the whole attachment. With none, the frame's first render
        // pass must LOAD what is already there — clearing it would wipe the previous frame's content outside a
        // scissored clear's rect (and, with the clear color taken from that scissored clear, read as "scissor
        // ignored"). See [`RenderPasses::full_clear`].
        let has_full_clear = RenderPasses::full_clear(draws).0.is_some();
        // A SCISSORED `glClear` among the survivors fills a sub-rect with a color (GskGpu/Chrome fill the page
        // background this way: `glEnable(GL_SCISSOR_TEST); glClear(#ff7700)` over the content rect). It is a real
        // paint op, not a pass boundary — lowered below as an `Enc::ClearRect` between render-pass segments.
        // A depth/stencil clear recorded AFTER geometry is not a colour op at all: it starts a new render
        // pass whose depth attachment clear-loads, so the draws after it test against a fresh depth buffer.
        // Folded into the same segmented path as the scissored colour clear (see [`segment_boundary`]).
        let needs_segments = survivors.iter().any(segment_boundary);
        let first_geometry = survivors.iter().find(|draw| !draw.is_clear);
        // A multiple-render-target framebuffer resolves BEFORE the "nothing but clears" shortcut below: a
        // `glClearBufferfv(GL_COLOR, 1, …)` frame has no geometry at all, and the single-target shortcut
        // would clear attachment 0 with it (which is exactly "the index was ignored").
        if !needs_segments {
            let mrt_fbo = draws.first().map(|d| d.fbo).unwrap_or(0);
            if mrt_fbo != 0 && ctx.local.framebuffers.color_attachment_count(mrt_fbo) > 1 {
                // The WHOLE run, not the slot-0 survivors: which draws a clear supersedes is a per-
                // attachment question here, and the builder answers it per attachment.
                if let Some(f) = build_mrt_geometry_frame(ctx, draws, mrt_fbo) {
                    return Some(f);
                }
                // Fall through to the single-target path if the MRT attachments could not be fully resolved.
            }
        }
        if first_geometry.is_none() && !needs_segments {
            // Nothing but non-colour clears (or nothing at all) is left, so there is no colour work to present.
            if !draws.iter().any(DrawCall::clears_color) {
                return None;
            }
            // Every geometry draw was erased by a trailing full-framebuffer clear — present just that clear color.
            // All draws in this single-group path share one framebuffer, so the last draw's fbo is the target.
            let last = draws.last();
            let fbo = last.map(|d| d.fbo).unwrap_or(0);
            return Some(build_clear_frame_snapshot(
                ctx,
                fbo,
                last.and_then(|draw| draw.target),
                clear,
                Vec::new(),
                draws.len(),
            ));
        }
        // The render target follows the surviving geometry's (or the first survivor's) framebuffer binding.
        let fbo = first_geometry
            .or_else(|| survivors.first())
            .map(|d| d.fbo)
            .unwrap_or(0);

        // A `glDrawBuffers` MRT frame: the bound FBO carries 2+ contiguous color attachments, so the frame
        // renders ALL of them in ONE pass with N color targets (see [`build_mrt_geometry_frame`]). An FBO with
        // one (or zero) attachment, or the default framebuffer, stays on the byte-identical single-target path.
        // MRT never co-occurs with a scissored clear in practice, so it keeps the single-pass path.
        let mut cmds: Vec<Cmd> = Vec::new();
        let snapshot = first_geometry
            .or_else(|| survivors.first())
            .and_then(|d| d.target);
        let (surface, target_tex, tw, th, target_fmt) =
            resolve_target(ctx, fbo, snapshot, &mut cmds);
        let bottom_up = RenderPasses::stores_bottom_up_rows(ctx, snapshot);
        let no_fbo_tex = std::collections::HashMap::new();
        let mut snapshots = SnapshotTextures::new();

        if !needs_segments {
            // ---- single-pass path (byte-identical to the pre-scissored-clear builder) ----
            // The pass's shared depth(+stencil) attachment format (if any draw is depth/stencil-tested) — every
            // depth pipeline in the pass must be built at this format to match the attachment (wgpu requirement).
            let depth_fmt = RenderPasses::depth_format(survivors);
            let mut copies: Vec<Enc> = Vec::new();
            let mut draw_ops: Vec<Enc> = Vec::new();
            for d in survivors {
                if let Some(lowered) = lower_draw(
                    ctx,
                    d,
                    target_fmt,
                    depth_fmt,
                    tw,
                    th,
                    &mut cmds,
                    &no_fbo_tex,
                    &mut snapshots,
                ) {
                    copies.extend(lowered.copies);
                    draw_ops.extend(lowered.ops);
                }
            }
            // Not one geometry draw could be lowered (e.g. every program was unlinked). A `glClear` is
            // program-independent and must still reach the framebuffer, so present the clear-only pass
            // instead of dropping the frame: glmark2 does not check `GL_LINK_STATUS` and issues its draws
            // anyway, and dropping the whole frame left its window with no content at all. With no clear
            // recorded either there is genuinely nothing to show, so the frame is still skipped.
            if draw_ops.is_empty() {
                if !draws.iter().any(DrawCall::clears_color) {
                    return None;
                }
                return Some(build_clear_frame_snapshot(
                    ctx,
                    fbo,
                    snapshot,
                    clear,
                    cmds,
                    draws.len(),
                ));
            }
            let depth = depth_attachment_for(
                ctx,
                target_tex,
                tw,
                th,
                survivors,
                &mut cmds,
                depth_load(draws),
            );
            let mut ops: Vec<Enc> = copies;
            ops.push(Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: target_tex,
                    load: if has_full_clear {
                        LoadOp::Clear
                    } else {
                        LoadOp::Load
                    },
                    clear,
                    store: true,
                }],
                depth,
            });
            ops.extend(draw_ops);
            ops.push(Enc::EndRenderPass);
            cmds.push(Cmd::Submit(CommandBuffer {
                encoder: ops,
                signal: None,
            }));
            log_frame(
                tw,
                th,
                draws.len(),
                1,
                cmds.len(),
                Frame::upload_bytes(&cmds),
            );
            return Some(Frame {
                cmds,
                present: (surface, target_tex),
                target_width: tw,
                target_height: th,
                target_format: target_fmt,
                color_attachments: Vec::new(),
                targets: frame_target(ctx, fbo, snapshot, target_tex, tw, th, target_fmt)
                    .into_iter()
                    .collect(),
            });
        }

        // ---- segmented path: a scissored clear paints a rect, so the frame is a SEQUENCE of render-pass
        // segments separated by `Enc::ClearRect` fills, all writing the ONE target in draw order. The first
        // segment's pass clear-loads the target ONLY if an unscissored clear justified it (`clear`, the
        // effective full clear) — otherwise it load-preserves like the rest; every later segment
        // load-preserves what the prior segments + fills already wrote (`LoadOp::Load`). This is what renders
        // Chrome's page background: `clear(full transparent); …draws; SCISSORED clear(#ff7700 over the content
        // rect); …composite tile draws` → transparent clear, orange rect fill, tiles composited on top.
        let mut ops: Vec<Enc> = Vec::new();
        lower_segments(
            ctx,
            survivors,
            SegmentTarget {
                texture: target_tex,
                format: target_fmt,
                width: tw,
                height: th,
                bottom_up,
            },
            clear,
            if has_full_clear {
                LoadOp::Clear
            } else {
                LoadOp::Load
            },
            &mut cmds,
            &mut ops,
            &no_fbo_tex,
            &mut snapshots,
        );

        cmds.push(Cmd::Submit(CommandBuffer {
            encoder: ops,
            signal: None,
        }));
        log_frame(
            tw,
            th,
            draws.len(),
            1,
            cmds.len(),
            Frame::upload_bytes(&cmds),
        );
        Some(Frame {
            cmds,
            present: (surface, target_tex),
            target_width: tw,
            target_height: th,
            target_format: target_fmt,
            color_attachments: Vec::new(),
            targets: frame_target(ctx, fbo, snapshot, target_tex, tw, th, target_fmt)
                .into_iter()
                .collect(),
        })
    }
}

#[cfg(test)]
mod clone_tests {
    use super::*;
    use crate::model::program::BufferSnapshot;
    use std::sync::Arc;

    #[test]
    fn large_draw_selection_borrows_snapshots_without_cloning() {
        let storage = Arc::new(vec![7; 4096]);
        let draws = (0..20_000)
            .map(|_| {
                let mut draw = DrawCall::default();
                draw.buffers.push(BufferSnapshot {
                    name: 1,
                    generation: 1,
                    data: Arc::clone(&storage),
                });
                draw.ubo_bytes = vec![3; 256];
                draw
            })
            .collect::<Vec<_>>();
        let refs_before = Arc::strong_count(&storage);
        let first = draws.as_ptr();

        let (_, selected) = survivors(&draws);

        assert_eq!(selected.len(), 20_000);
        assert_eq!(selected.as_ptr(), first);
        assert_eq!(
            Arc::strong_count(&storage),
            refs_before,
            "selecting a Chrome-sized draw list must not clone per-draw snapshots"
        );
    }

    fn clear_draw(color: [f32; 4], scissor: Option<[i32; 4]>) -> DrawCall {
        let mut draw = DrawCall::default();
        draw.is_clear = true;
        draw.clear = color;
        draw.scissor_enabled = scissor.is_some();
        draw.scissor = scissor.unwrap_or_default();
        draw
    }

    fn window_ctx(w: u32, h: u32) -> GlContext {
        let mut ctx = GlContext::new();
        ctx.local.surf.have = true;
        ctx.local.surf.width = w;
        ctx.local.surf.height = h;
        ctx
    }

    fn color_loads(frame: &Frame) -> Vec<LoadOp> {
        frame
            .cmds
            .iter()
            .filter_map(|c| match c {
                Cmd::Submit(batch) => Some(batch.encoder.iter()),
                _ => None,
            })
            .flatten()
            .filter_map(|e| match e {
                Enc::BeginRenderPass { color, .. } => color.first().map(|a| a.load),
                _ => None,
            })
            .collect()
    }

    /// A scissored `glClear` must paint ONLY its rect: an `Enc::ClearRect` over a LOAD-ing pass. Promoting it
    /// to a full-target `LoadOp::Clear` is indistinguishable from ignoring `GL_SCISSOR_TEST` (and wipes the
    /// previous frame outside the rect).
    #[test]
    fn scissored_clear_lowers_to_load_plus_clear_rect() {
        let mut ctx = window_ctx(64, 64);
        ctx.local.recording.draws = vec![clear_draw([1.0, 0.0, 0.0, 1.0], Some([0, 0, 32, 32]))];

        let frame = Frame::build_clear(&mut ctx).expect("a color clear frame");

        assert!(
            color_loads(&frame).iter().all(|l| *l == LoadOp::Load),
            "a frame with no unscissored clear must not clear-load the whole attachment: {:?}",
            color_loads(&frame)
        );
        let rects = frame
            .cmds
            .iter()
            .filter_map(|c| match c {
                Cmd::Submit(batch) => Some(batch.encoder.iter()),
                _ => None,
            })
            .flatten()
            .filter(|e| matches!(e, Enc::ClearRect { .. }))
            .count();
        assert_eq!(rects, 1, "the scissored clear must lower to one ClearRect");
    }

    /// An UNSCISSORED clear still clear-loads the pass at its color — the scissored rect fill lands on top.
    #[test]
    fn full_clear_before_a_scissored_clear_still_clear_loads() {
        let mut ctx = window_ctx(64, 64);
        ctx.local.recording.draws = vec![
            clear_draw([0.0, 0.0, 1.0, 1.0], None),
            clear_draw([1.0, 0.0, 0.0, 1.0], Some([0, 0, 32, 32])),
        ];

        let frame = Frame::build_clear(&mut ctx).expect("a color clear frame");

        assert_eq!(color_loads(&frame).first(), Some(&LoadOp::Clear));
    }

    /// A depth- or stencil-only `glClear` is not a color clear: it must neither justify a full-target
    /// `LoadOp::Clear` nor supply the clear color, and a frame made only of such clears has no color work.
    #[test]
    fn depth_and_stencil_clears_are_not_color_clears() {
        let red = [1.0, 0.0, 0.0, 1.0];
        for mask in [
            crate::model::glconst::GL_DEPTH_BUFFER_BIT,
            crate::model::glconst::GL_STENCIL_BUFFER_BIT,
        ] {
            let mut draw = clear_draw(red, None);
            draw.clear_mask = mask;
            let draws = vec![draw];

            assert_eq!(
                RenderPasses::full_clear(&draws).0,
                None,
                "mask {mask:#x} names no color plane"
            );
            assert_ne!(RenderPasses::effective_clear(&draws).0, red);

            let mut ctx = window_ctx(64, 64);
            ctx.local.recording.draws = draws;
            assert!(
                Frame::build_clear(&mut ctx).is_none(),
                "a clear frame with no color clear must present nothing (mask {mask:#x})"
            );
        }
    }

    /// A partially `glColorMask`ed clear cannot be expressed by a four-channel clear, so it clears nothing.
    #[test]
    fn channel_masked_color_clear_is_not_a_color_clear() {
        let mut draw = clear_draw([1.0, 0.0, 0.0, 1.0], None);
        draw.color_mask = 0x8; // alpha only — Skia's `glColorMask(0,0,0,1); glClear(...)`
        assert!(!draw.clears_color());
        assert!(draw.color_clear_is_partial());
        assert_eq!(RenderPasses::full_clear(&[draw]).0, None);
    }

    /// A scissored clear's color must never become the full-target clear color.
    #[test]
    fn scissored_clear_color_is_not_the_full_target_clear() {
        let red = [1.0, 0.0, 0.0, 1.0];
        let draws = vec![clear_draw(red, Some([0, 0, 32, 32]))];

        assert_eq!(RenderPasses::full_clear(&draws).0, None);
        assert_ne!(
            RenderPasses::effective_clear(&draws).0,
            red,
            "a scissored clear paints its rect only — its color must not clear the attachment"
        );

        // …and it must not be picked up as the fallback ahead of a later geometry draw either.
        let mut geometry = DrawCall::default();
        geometry.clear = [0.0, 0.0, 1.0, 1.0];
        let mixed = vec![clear_draw(red, Some([0, 0, 32, 32])), geometry];
        assert_ne!(RenderPasses::effective_clear(&mixed).0, red);
    }
}


/// The one render target a run of draws lowers into.
#[derive(Clone, Copy)]
pub(super) struct SegmentTarget {
    pub(super) texture: u32,
    pub(super) format: TextureFormat,
    pub(super) width: i32,
    pub(super) height: i32,
    /// Whether the target stores GL texel rows bottom-up (see [`RenderPasses::stores_bottom_up_rows`]).
    pub(super) bottom_up: bool,
}

/// A recorded `glClear` that cannot be folded into the run's leading pass clear, and therefore ends the
/// current render-pass segment:
/// * a SCISSORED color clear — it paints a sub-rect, lowered to an `Enc::ClearRect` between segments;
/// * a DEPTH or STENCIL clear — the only place this IR can clear those planes is a pass's `DepthAttachment`
///   load op, so the draws recorded after it must run in a NEW pass that clear-loads depth. Folding it into
///   the current pass would leave the following draws testing against the previous draws' depth.
///
/// A clear that names no writable plane at all never reaches recording (see `record_clear_buffers`).
pub(super) fn segment_boundary(d: &DrawCall) -> bool {
    (d.clears_color() && d.scissor_enabled) || d.clears_depth() || d.clears_stencil()
}

/// The depth-attachment load op for a pass covering `draws`, together with the values the clear that armed
/// it carried: `Clear` when one of them is a depth or stencil clear, otherwise `Load` (GL keeps the depth
/// buffer between frames unless the app clears it). A depth texture created this frame always clear-loads
/// regardless — see [`depth_attachment_for`].
///
/// The values come from the LAST depth (resp. stencil) clear in the run rather than from live context
/// state, because `glClearDepthf` is ordinary state an app moves between clears: reading it at lowering
/// time gave every depth clear in a frame the frame's final `glClearDepthf` value.
#[derive(Clone, Copy)]
pub(super) struct DepthClear {
    pub(super) load: LoadOp,
    pub(super) depth: f32,
    pub(super) stencil: i32,
}

impl DepthClear {
    /// The GL initial clear values, used when nothing in the run recorded a depth/stencil clear (a depth
    /// texture minted this frame still has to clear-load; see [`depth_attachment_for`]).
    pub(super) fn preserving() -> Self {
        Self {
            load: LoadOp::Load,
            depth: 1.0,
            stencil: 0,
        }
    }
}

pub(super) fn depth_load(draws: &[DrawCall]) -> DepthClear {
    let mut clear = DepthClear::preserving();
    for d in draws {
        if d.clears_depth() {
            clear.load = LoadOp::Clear;
            clear.depth = d.clear_depth;
        }
        if d.clears_stencil() {
            clear.load = LoadOp::Clear;
            clear.stencil = d.clear_stencil;
        }
    }
    clear
}

/// Lower one framebuffer run as a SEQUENCE of render-pass segments split at every [`segment_boundary`],
/// appending the encoder ops to `ops` (the caller submits them). `first_load` is the color load op of the
/// FIRST segment — `LoadOp::Clear` only when an unscissored color clear justifies wiping the whole
/// attachment; every later segment load-preserves what the earlier segments and rect fills wrote.
///
/// Depth is carried by each segment's `DepthAttachment`: the first segment clear-loads it when the run
/// contains a depth/stencil clear at all, and every segment that FOLLOWS such a clear clear-loads it again.
#[allow(clippy::too_many_arguments)]
pub(super) fn lower_segments(
    ctx: &mut GlContext,
    run: &[DrawCall],
    target: SegmentTarget,
    clear: [f32; 4],
    first_load: LoadOp,
    cmds: &mut Vec<Cmd>,
    ops: &mut Vec<Enc>,
    fbo_tex_ir: &std::collections::HashMap<(u32, u64), u32>,
    snapshots: &mut SnapshotTextures,
) {
    let mut load = first_load;
    // The run's leading depth clear (if any) applies to the first segment; later depth clears re-arm it.
    let mut dload = depth_load(run);
    let mut segment: Vec<DrawCall> = Vec::new();
    let mut emit = |ctx: &mut GlContext,
                    cmds: &mut Vec<Cmd>,
                    ops: &mut Vec<Enc>,
                    segment: &mut Vec<DrawCall>,
                    load: &mut LoadOp,
                    dload: &mut DepthClear| {
        emit_segment_pass(
            ctx,
            cmds,
            ops,
            segment,
            target.texture,
            target.format,
            target.width,
            target.height,
            clear,
            *load,
            *dload,
            fbo_tex_ir,
            snapshots,
        );
        segment.clear();
        *load = LoadOp::Load;
        *dload = DepthClear::preserving();
    };

    for d in run {
        if !d.is_clear {
            segment.push(d.clone());
            continue;
        }
        if !segment_boundary(d) {
            continue; // an unscissored color clear inside the run is already folded into `clear`.
        }
        // Everything recorded before this boundary must reach the target first.
        if !segment.is_empty() || matches!(load, LoadOp::Clear) {
            emit(ctx, cmds, ops, &mut segment, &mut load, &mut dload);
        }
        if d.clears_color() && d.scissor_enabled {
            if let Some(rect) = scissored_clear_rect_enc(
                d,
                target.texture,
                target.width,
                target.height,
                target.bottom_up,
            ) {
                ops.push(rect);
            }
        }
        if d.clears_depth() {
            dload.load = LoadOp::Clear;
            dload.depth = d.clear_depth;
        }
        if d.clears_stencil() {
            dload.load = LoadOp::Clear;
            dload.stencil = d.clear_stencil;
        }
    }
    emit(ctx, cmds, ops, &mut segment, &mut load, &mut dload);
}

/// Emit one render-pass SEGMENT of the scissored-clear-split geometry frame (see [`build_geometry_frame`]):
/// lower `seg`'s draws, hoist their staging copies ahead of the pass, then a `BeginRenderPass` that
/// clear-loads (`clear`) on the FIRST segment or load-preserves (`LoadOp::Load`) on later ones, the draws,
/// and `EndRenderPass`. An empty `seg` still emits its pass so a leading scissored clear's full-target clear
/// (or a load boundary) is established.
#[allow(clippy::too_many_arguments)]
pub(super) fn emit_segment_pass(
    ctx: &mut GlContext,
    cmds: &mut Vec<Cmd>,
    ops: &mut Vec<Enc>,
    seg: &[DrawCall],
    target_tex: u32,
    target_fmt: TextureFormat,
    tw: i32,
    th: i32,
    clear: [f32; 4],
    load: LoadOp,
    depth_load: DepthClear,
    no_fbo_tex: &std::collections::HashMap<(u32, u64), u32>,
    snapshots: &mut SnapshotTextures,
) {
    let depth_fmt = RenderPasses::depth_format(seg);
    let mut copies: Vec<Enc> = Vec::new();
    let mut draw_ops: Vec<Enc> = Vec::new();
    for d in seg {
        if let Some(lowered) = lower_draw(
            ctx, d, target_fmt, depth_fmt, tw, th, cmds, no_fbo_tex, snapshots,
        ) {
            copies.extend(lowered.copies);
            draw_ops.extend(lowered.ops);
        }
    }
    let depth = depth_attachment_for(ctx, target_tex, tw, th, seg, cmds, depth_load);
    ops.extend(copies);
    ops.push(Enc::BeginRenderPass {
        color: vec![ColorAttachment {
            texture: target_tex,
            load,
            clear,
            store: true,
        }],
        depth,
    });
    ops.extend(draw_ops);
    ops.push(Enc::EndRenderPass);
}

/// Lower a SCISSORED `glClear` to an `Enc::ClearRect` fill of `target_tex`, clamped to the target.
///
/// Ordinary FBOs convert GL's bottom-left rows into texture rows. Present targets already receive the
/// host-surface coordinate specialization used by their vertex shaders, so their rows remain unchanged.
/// Returns `None` for a degenerate rect.
pub(super) fn scissored_clear_rect_enc(
    d: &DrawCall,
    target_tex: u32,
    tw: i32,
    th: i32,
    bottom_up: bool,
) -> Option<Enc> {
    let [sx, sy, sw, sh] = d.scissor;
    if sw <= 0 || sh <= 0 {
        return None;
    }
    let x = sx.clamp(0, tw);
    let y_top = if bottom_up {
        sy.max(0)
    } else {
        (th - sy - sh).max(0)
    };
    let mut w = sw;
    let mut h = sh;
    if x + w > tw {
        w = tw - x;
    }
    if y_top + h > th {
        h = th - y_top;
    }
    if w <= 0 || h <= 0 {
        return None;
    }
    Some(Enc::ClearRect {
        texture: target_tex,
        x: x as u32,
        y: y_top as u32,
        w: w as u32,
        h: h as u32,
        color: d.clear,
    })
}

/// Multiple-render-target geometry frame (`glDrawBuffers` MRT): the bound `fbo` has N ≥ 2 contiguous color
/// attachments (`GL_COLOR_ATTACHMENT0..N`), so all N are rendered in ONE pass with N color targets. The
/// fragment shader's `layout(location = k) out` outputs land on attachment `k`. Each attachment texture is
/// materialized (cached on the context) and returned in `Frame::color_attachments` so a later
/// `glReadPixels` under a `glReadBuffer(GL_COLOR_ATTACHMENT{i})` selection reads the right one. `None` if an
/// attachment texture is missing/unsized (the caller falls back to the single-target path).
pub(super) fn build_mrt_geometry_frame(
    ctx: &mut GlContext,
    geom: &[DrawCall],
    fbo: u32,
) -> Option<Frame> {
    let n = ctx.local.framebuffers.color_attachment_count(fbo) as usize;
    // Resolve every attachment's render-target texture (all must share the pass dimensions, as wgpu
    // requires). Attachment 0 sets the pass size/format.
    let mut cmds: Vec<Cmd> = Vec::new();
    let mut targets: Vec<u32> = Vec::with_capacity(n);
    let mut frame_targets: Vec<FrameTarget> = Vec::with_capacity(n);
    let mut dims: Option<(i32, i32)> = None;
    let mut fmt0 = TextureFormat::Rgba8Unorm;
    for idx in 0..n {
        let gl_tex = ctx
            .local
            .framebuffers
            .color_attachment_index(fbo, idx as u32);
        let (w, h, fmt) = ctx
            .textures
            .get(gl_tex)
            .filter(|t| t.w > 0 && t.h > 0)
            .map(|t| (t.w, t.h, t.ir_format))?;
        match dims {
            None => {
                dims = Some((w, h));
                fmt0 = fmt;
            }
            Some((dw, dh)) if dw == w && dh == h => {}
            Some(_) => return None, // mismatched attachment sizes → not a lowerable MRT pass here
        }
        let generation = ctx.textures.get(gl_tex).map(|t| t.gen).unwrap_or(0);
        let (surface, texture, needs_create, ephemeral) =
            ctx.recorded_fbo_target(gl_tex, generation).ok()?;
        if needs_create {
            push_target_creates(
                &mut cmds,
                surface,
                texture,
                w,
                h,
                fmt,
                if ephemeral {
                    "gl-retired-fbo"
                } else {
                    "mrt-fbo"
                },
                true,
                ctx.external_target(gl_tex, generation),
            );
        }
        targets.push(texture);
        frame_targets.push(FrameTarget {
            name: gl_tex,
            generation,
            shared_storage: ctx
                .textures
                .get(gl_tex)
                .and_then(crate::model::texture::GlTexture::shared_storage),
            shared_revision: ctx
                .textures
                .get(gl_tex)
                .and_then(crate::model::texture::GlTexture::shared_current_identity)
                .map(|(_, revision)| revision),
            surface,
            texture,
            width: w,
            height: h,
            format: fmt,
            token: ctx.external_target(gl_tex, generation),
        });
    }
    let (tw, th) = dims?;

    // An UNSCOPED unscissored `glClear` wipes every selected attachment, so the draws recorded before it
    // are gone and the pass starts there. A `glClearBuffer*` is scoped to one attachment and supersedes
    // nothing, which is why it does not move this boundary.
    let start = geom
        .iter()
        .rposition(|d| d.clears_color() && !d.scissor_enabled && d.clear_draw_buffer.is_none())
        .unwrap_or(0);
    let geom = &geom[start..];

    // Lower each draw with N color targets so the pipeline writes every attachment.
    let no_fbo_tex = std::collections::HashMap::new();
    let mut snapshots = SnapshotTextures::new();
    let mut copies: Vec<Enc> = Vec::new();
    let mut draw_ops: Vec<Enc> = Vec::new();
    for d in geom.iter().filter(|d| !d.is_clear) {
        // MRT passes carry no depth/stencil attachment in this model, so no depth pipeline format.
        if let Some(lowered) = lower_draw_n(
            ctx,
            d,
            fmt0,
            None,
            n,
            tw,
            th,
            &mut cmds,
            &no_fbo_tex,
            &mut snapshots,
        ) {
            copies.extend(lowered.copies);
            draw_ops.extend(lowered.ops);
        }
    }
    // A frame of nothing but `glClearBufferfv`s is still a frame: its pass carries the attachment loads and
    // no draws. Only a frame that neither draws nor clears any attachment has nothing to lower.
    if draw_ops.is_empty()
        && !(0..n as u32).any(|slot| geom.iter().any(|d| d.clears_color_slot(slot)))
    {
        return None;
    }

    // Each attachment clear-loads with the LAST clear that names IT (see [`RenderPasses::full_clear_slot`])
    // and load-preserves when nothing cleared it — which is what makes a scoped clear of attachment 1 leave
    // attachments 0 and 2 alone.
    let color: Vec<ColorAttachment> = targets
        .iter()
        .enumerate()
        .map(|(slot, &texture)| {
            let (clear, _) = RenderPasses::full_clear_slot(geom, slot as u32);
            ColorAttachment {
                texture,
                load: if clear.is_some() {
                    LoadOp::Clear
                } else {
                    LoadOp::Load
                },
                clear: clear.unwrap_or([0.0; 4]),
                store: true,
            }
        })
        .collect();
    let mut ops: Vec<Enc> = copies;
    ops.push(Enc::BeginRenderPass { color, depth: None });
    ops.extend(draw_ops);
    ops.push(Enc::EndRenderPass);

    cmds.push(Cmd::Submit(CommandBuffer {
        encoder: ops,
        signal: None,
    }));
    log_frame(
        tw,
        th,
        geom.len(),
        1,
        cmds.len(),
        Frame::upload_bytes(&cmds),
    );
    // Present + default readback target is attachment 0 (there is no default window surface — MRT renders
    // only to the FBO textures); the full `color_attachments` list routes `glReadBuffer` selection.
    let present_tex = targets[0];
    Some(Frame {
        cmds,
        present: (0, present_tex),
        target_width: tw,
        target_height: th,
        target_format: fmt0,
        color_attachments: targets,
        targets: frame_targets,
    })
}

// Draw lowering continues in `lower`.
