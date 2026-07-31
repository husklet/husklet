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
        let has_scissored_clear = survivors.iter().any(|d| d.is_clear && d.scissor_enabled);
        let first_geometry = survivors.iter().find(|draw| !draw.is_clear);
        if first_geometry.is_none() && !has_scissored_clear {
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
        if !has_scissored_clear
            && fbo != 0
            && ctx.local.framebuffers.color_attachment_count(fbo) > 1
        {
            if let Some(f) = build_mrt_geometry_frame(ctx, survivors, fbo, clear) {
                return Some(f);
            }
            // Fall through to the single-target path if the MRT attachments could not be fully resolved.
        }

        let mut cmds: Vec<Cmd> = Vec::new();
        let snapshot = first_geometry
            .or_else(|| survivors.first())
            .and_then(|d| d.target);
        let (surface, target_tex, tw, th, target_fmt) =
            resolve_target(ctx, fbo, snapshot, &mut cmds);
        let bottom_up = RenderPasses::stores_bottom_up_rows(ctx, snapshot);
        let no_fbo_tex = std::collections::HashMap::new();
        let mut snapshots = SnapshotTextures::new();

        if !has_scissored_clear {
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
                if !draws.iter().any(|draw| draw.is_clear) {
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
            let depth = depth_attachment_for(ctx, target_tex, tw, th, survivors, &mut cmds);
            let mut ops: Vec<Enc> = copies;
            ops.push(Enc::BeginRenderPass {
                color: vec![ColorAttachment {
                    texture: target_tex,
                    load: LoadOp::Clear,
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
        let mut first_pass = true;
        let mut segment_start = 0;
        for (index, d) in survivors.iter().enumerate() {
            if d.is_clear && d.scissor_enabled {
                emit_segment_pass(
                    ctx,
                    &mut cmds,
                    &mut ops,
                    &survivors[segment_start..index],
                    target_tex,
                    target_fmt,
                    tw,
                    th,
                    clear,
                    if first_pass && has_full_clear {
                        LoadOp::Clear
                    } else {
                        LoadOp::Load
                    },
                    &no_fbo_tex,
                    &mut snapshots,
                );
                first_pass = false;
                segment_start = index + 1;
                if let Some(cr) = scissored_clear_rect_enc(d, target_tex, tw, th, bottom_up) {
                    ops.push(cr);
                }
            }
            // A non-scissored clear cannot appear after `start` (that index is past the last full clear).
        }
        emit_segment_pass(
            ctx,
            &mut cmds,
            &mut ops,
            &survivors[segment_start..],
            target_tex,
            target_fmt,
            tw,
            th,
            clear,
            if first_pass && has_full_clear {
                LoadOp::Clear
            } else {
                LoadOp::Load
            },
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

        let frame = Frame::build_clear(&mut ctx);

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

        let frame = Frame::build_clear(&mut ctx);

        assert_eq!(color_loads(&frame).first(), Some(&LoadOp::Clear));
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
    let depth = depth_attachment_for(ctx, target_tex, tw, th, seg, cmds);
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
    clear: [f32; 4],
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

    // Lower each draw with N color targets so the pipeline writes every attachment.
    let no_fbo_tex = std::collections::HashMap::new();
    let mut snapshots = SnapshotTextures::new();
    let mut copies: Vec<Enc> = Vec::new();
    let mut draw_ops: Vec<Enc> = Vec::new();
    for d in geom {
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
    if draw_ops.is_empty() {
        return None;
    }

    let color: Vec<ColorAttachment> = targets
        .iter()
        .map(|&texture| ColorAttachment {
            texture,
            load: LoadOp::Clear,
            clear,
            store: true,
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
