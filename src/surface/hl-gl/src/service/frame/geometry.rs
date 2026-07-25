use super::*;

pub(super) fn build_clear_frame_color(
    ctx: &mut GlContext,
    fbo: u32,
    clear: [f32; 4],
    mut cmds: Vec<Cmd>,
) -> Frame {
    let snapshot = ctx
        .draws
        .iter()
        .rev()
        .find(|d| d.fbo == fbo)
        .and_then(|d| d.target);
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
    log_frame(
        w,
        h,
        ctx.draws.len(),
        1,
        cmds.len(),
        Frame::upload_bytes(&cmds),
    );
    Frame {
        cmds,
        present: (surface, texture),
        target_width: w,
        target_height: h,
        target_format: fmt,
        color_attachments: Vec::new(),
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
        // An unscissored full-framebuffer `glClear` wipes all prior rendering, so the effective pass clear is the
        // LAST such clear's color and only draws recorded AFTER it survive (see [`effective_clear`]). Chrome's
        // window frame clears the default framebuffer to `#ff7700` (the page background) partway through its
        // draw-list, then composites tiles — replaying the wiped pre-clear draws or using the leading transparent
        // clear (the old behavior) dropped that background and read back blank.
        let (clear, start) = RenderPasses::effective_clear(&ctx.draws);
        let survivors: Vec<DrawCall> = ctx.draws[start..].to_vec();
        let geom: Vec<DrawCall> = survivors.iter().filter(|d| !d.is_clear).cloned().collect();
        // A SCISSORED `glClear` among the survivors fills a sub-rect with a color (GskGpu/Chrome fill the page
        // background this way: `glEnable(GL_SCISSOR_TEST); glClear(#ff7700)` over the content rect). It is a real
        // paint op, not a pass boundary — lowered below as an `Enc::ClearRect` between render-pass segments.
        let has_scissored_clear = survivors.iter().any(|d| d.is_clear && d.scissor_enabled);
        if geom.is_empty() && !has_scissored_clear {
            // Every geometry draw was erased by a trailing full-framebuffer clear — present just that clear color.
            // All draws in this single-group path share one framebuffer, so the last draw's fbo is the target.
            let fbo = ctx.draws.last().map(|d| d.fbo).unwrap_or(0);
            return Some(build_clear_frame_color(ctx, fbo, clear, Vec::new()));
        }
        // The render target follows the surviving geometry's (or the first survivor's) framebuffer binding.
        let fbo = geom
            .first()
            .or_else(|| survivors.first())
            .map(|d| d.fbo)
            .unwrap_or(0);

        // A `glDrawBuffers` MRT frame: the bound FBO carries 2+ contiguous color attachments, so the frame
        // renders ALL of them in ONE pass with N color targets (see [`build_mrt_geometry_frame`]). An FBO with
        // one (or zero) attachment, or the default framebuffer, stays on the byte-identical single-target path.
        // MRT never co-occurs with a scissored clear in practice, so it keeps the single-pass path.
        if !has_scissored_clear && fbo != 0 && ctx.framebuffers.color_attachment_count(fbo) > 1 {
            if let Some(f) = build_mrt_geometry_frame(ctx, &geom, fbo, clear) {
                return Some(f);
            }
            // Fall through to the single-target path if the MRT attachments could not be fully resolved.
        }

        let mut cmds: Vec<Cmd> = Vec::new();
        let snapshot = geom
            .first()
            .or_else(|| survivors.first())
            .and_then(|d| d.target);
        let (surface, target_tex, tw, th, target_fmt) =
            resolve_target(ctx, fbo, snapshot, &mut cmds);
        let no_fbo_tex = std::collections::HashMap::new();

        if !has_scissored_clear {
            // ---- single-pass path (byte-identical to the pre-scissored-clear builder) ----
            // The pass's shared depth(+stencil) attachment format (if any draw is depth/stencil-tested) — every
            // depth pipeline in the pass must be built at this format to match the attachment (wgpu requirement).
            let depth_fmt = RenderPasses::depth_format(&geom);
            let mut copies: Vec<Enc> = Vec::new();
            let mut draw_ops: Vec<Enc> = Vec::new();
            for d in &geom {
                if let Some(lowered) = lower_draw(
                    ctx,
                    d,
                    target_fmt,
                    depth_fmt,
                    tw,
                    th,
                    &mut cmds,
                    &no_fbo_tex,
                ) {
                    copies.extend(lowered.copies);
                    draw_ops.extend(lowered.ops);
                }
            }
            // Not one geometry draw could be lowered (e.g. every program was unlinked) → present nothing.
            if draw_ops.is_empty() {
                return None;
            }
            let depth = depth_attachment_for(ctx, target_tex, tw, th, &geom, &mut cmds);
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
                ctx.draws.len(),
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
            });
        }

        // ---- segmented path: a scissored clear paints a rect, so the frame is a SEQUENCE of render-pass
        // segments separated by `Enc::ClearRect` fills, all writing the ONE target in draw order. The first
        // segment's pass clear-loads the target (`clear`, the effective full clear); every later segment
        // load-preserves what the prior segments + fills already wrote (`LoadOp::Load`). This is what renders
        // Chrome's page background: `clear(full transparent); …draws; SCISSORED clear(#ff7700 over the content
        // rect); …composite tile draws` → transparent clear, orange rect fill, tiles composited on top.
        let mut ops: Vec<Enc> = Vec::new();
        let mut seg: Vec<DrawCall> = Vec::new();
        let mut first_pass = true;
        for d in &survivors {
            if d.is_clear {
                if d.scissor_enabled {
                    emit_segment_pass(
                        ctx,
                        &mut cmds,
                        &mut ops,
                        &seg,
                        target_tex,
                        target_fmt,
                        tw,
                        th,
                        clear,
                        first_pass,
                        &no_fbo_tex,
                    );
                    first_pass = false;
                    seg.clear();
                    if let Some(cr) = scissored_clear_rect_enc(d, target_tex, tw, th) {
                        ops.push(cr);
                    }
                }
                // A non-scissored clear cannot appear after `start` (that index is past the last full clear).
            } else {
                seg.push(d.clone());
            }
        }
        emit_segment_pass(
            ctx,
            &mut cmds,
            &mut ops,
            &seg,
            target_tex,
            target_fmt,
            tw,
            th,
            clear,
            first_pass,
            &no_fbo_tex,
        );

        cmds.push(Cmd::Submit(CommandBuffer {
            encoder: ops,
            signal: None,
        }));
        log_frame(
            tw,
            th,
            ctx.draws.len(),
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
        })
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
    first_pass: bool,
    no_fbo_tex: &std::collections::HashMap<(u32, u64), u32>,
) {
    let depth_fmt = RenderPasses::depth_format(seg);
    let mut copies: Vec<Enc> = Vec::new();
    let mut draw_ops: Vec<Enc> = Vec::new();
    for d in seg {
        if let Some(lowered) = lower_draw(ctx, d, target_fmt, depth_fmt, tw, th, cmds, no_fbo_tex) {
            copies.extend(lowered.copies);
            draw_ops.extend(lowered.ops);
        }
    }
    let depth = depth_attachment_for(ctx, target_tex, tw, th, seg, cmds);
    ops.extend(copies);
    let load = if first_pass {
        LoadOp::Clear
    } else {
        LoadOp::Load
    };
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

/// Lower a SCISSORED `glClear` to an `Enc::ClearRect` fill of `target_tex`, flipping the GL bottom-left
/// scissor rect into the render target's top-left texel origin and clamping to the target. `None` for a
/// degenerate (empty) rect.
pub(super) fn scissored_clear_rect_enc(
    d: &DrawCall,
    target_tex: u32,
    tw: i32,
    th: i32,
) -> Option<Enc> {
    let [sx, sy, sw, sh] = d.scissor;
    if sw <= 0 || sh <= 0 {
        return None;
    }
    let x = sx.clamp(0, tw);
    let y_top = (th - sy - sh).max(0);
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
    let n = ctx.framebuffers.color_attachment_count(fbo) as usize;
    // Resolve every attachment's render-target texture (all must share the pass dimensions, as wgpu
    // requires). Attachment 0 sets the pass size/format.
    let mut cmds: Vec<Cmd> = Vec::new();
    let mut targets: Vec<u32> = Vec::with_capacity(n);
    let mut dims: Option<(i32, i32)> = None;
    let mut fmt0 = TextureFormat::Rgba8Unorm;
    for idx in 0..n {
        let gl_tex = ctx.framebuffers.color_attachment_index(fbo, idx as u32);
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
        let (surface, texture, needs_create) = ctx.fbo_target(gl_tex, generation);
        if needs_create {
            push_target_creates(&mut cmds, surface, texture, w, h, fmt, "mrt-fbo", true);
        }
        targets.push(texture);
    }
    let (tw, th) = dims?;

    // Lower each draw with N color targets so the pipeline writes every attachment.
    let no_fbo_tex = std::collections::HashMap::new();
    let mut copies: Vec<Enc> = Vec::new();
    let mut draw_ops: Vec<Enc> = Vec::new();
    for d in geom {
        // MRT passes carry no depth/stencil attachment in this model, so no depth pipeline format.
        if let Some(lowered) = lower_draw_n(ctx, d, fmt0, None, n, tw, th, &mut cmds, &no_fbo_tex) {
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
    })
}

// Draw lowering continues in `lower`.
