use super::*;

pub(super) struct RenderPasses;

impl RenderPasses {
    pub(super) fn groups(draws: &[DrawCall]) -> Vec<(u32, usize, usize)> {
        let mut groups: Vec<(u32, usize, usize)> = Vec::new();
        for (i, d) in draws.iter().enumerate() {
            match groups.last_mut() {
                Some((fbo, start, end)) if *fbo == d.fbo && draws[*start].target == d.target => {
                    *end = i + 1
                }
                _ => groups.push((d.fbo, i, i + 1)),
            }
        }
        groups
    }

    /// Lower a multi-framebuffer frame as a SEQUENCE of render passes, one per contiguous `fbo` run (see
    /// [`fbo_groups`]). Each offscreen run renders into its FBO's color-attachment texture; a later run that
    /// samples that attachment binds the render-target texture directly (see [`lower_draw`]'s cross-pass path),
    /// so the atlas/offscreen → window composite works. The DEFAULT framebuffer (fbo `0`) run renders into the
    /// window color target — and THAT target is what the frame presents and a `glReadPixels` reads back, at
    /// window dimensions (not an offscreen atlas). Falls back to the last run's target for a frame that never
    /// bound fbo `0` (a pure render-to-offscreen frame). Returns `None` if nothing could be lowered.
    pub(super) fn build_multi(
        ctx: &mut GlContext,
        groups: &[(u32, usize, usize)],
    ) -> Option<Frame> {
        let draws = ctx.draws.clone();
        let blits = ctx.blits.clone();
        let mut cmds: Vec<Cmd> = Vec::new();
        // GL texture name of an FBO color attachment → the render-target texture IR a prior pass rendered into,
        // so a later pass sampling that attachment reads the rendered pixels rather than re-uploading its CPU
        // storage (an FBO attachment allocated via glTexImage2D(…, NULL) carries a zeroed plane, not the render).
        let mut fbo_tex_ir: std::collections::HashMap<(u32, u64), u32> =
            std::collections::HashMap::new();
        // FBO name → its resolved render target `(surface, texture, w, h, fmt)`, so a recorded
        // `glBlitFramebuffer` can find the source + destination attachment textures after the passes are built.
        let mut fbo_target: std::collections::HashMap<u32, (u32, u32, i32, i32, TextureFormat)> =
            std::collections::HashMap::new();
        // The default-framebuffer (window) target to present + read back; the last run's target is the fallback.
        let mut present: Option<(u32, u32, i32, i32, TextureFormat)> = None;
        let mut last: Option<(u32, u32, i32, i32, TextureFormat)> = None;

        for &(fbo, start, end) in groups {
            let run = &draws[start..end];
            let (surface, target_tex, tw, th, fmt) =
                resolve_target(ctx, fbo, run.first().and_then(|d| d.target), &mut cmds);
            fbo_target.insert(fbo, (surface, target_tex, tw, th, fmt));
            // Register this run's offscreen attachment so a later run can sample its rendered pixels. Mirror
            // resolve_target's offscreen condition (a sized attachment) so `target_tex` is the offscreen target.
            if let Some(target) = run.first().and_then(|d| d.target) {
                fbo_tex_ir.insert((target.texture, target.generation), target_tex);
            }
            // An unscissored full-framebuffer clear wipes the run's target, so the pass clear-loads with the LAST
            // such clear's color and replays only the draws recorded AFTER it (see [`effective_clear`]); with no
            // full clear this is the run's first-draw clear + replay-all (byte-identical to before).
            let (clear, rstart) = Self::effective_clear(run);
            let survivors = &run[rstart..];

            let depth_fmt = Self::depth_format(survivors);
            let mut copies: Vec<Enc> = Vec::new();
            let mut draw_ops: Vec<Enc> = Vec::new();
            for d in survivors.iter().filter(|d| !d.is_clear) {
                if let Some(l) = lower_draw(ctx, d, fmt, depth_fmt, tw, th, &mut cmds, &fbo_tex_ir)
                {
                    copies.extend(l.copies);
                    draw_ops.extend(l.ops);
                }
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

            last = Some((surface, target_tex, tw, th, fmt));
            if fbo == 0 {
                present = Some((surface, target_tex, tw, th, fmt));
            }
        }

        // Apply the recorded `glBlitFramebuffer` copies AFTER the render passes. Each copies a sub-rect from the
        // read FBO's resolved render target into the draw FBO's, lowered to `Enc::CopyTextureToTexture` for the
        // equal-size (non-scaling) case. The last blit's destination becomes the frame's present/read-back
        // target so a `glReadPixels` after the blit observes the copied result.
        for b in &blits {
            // Resolve each side's render target (rendered/cleared by a pass above, or created on demand).
            let src = match fbo_target.get(&b.read_fbo).copied() {
                Some(t) => t,
                None => {
                    let t = resolve_target(ctx, b.read_fbo, None, &mut cmds);
                    fbo_target.insert(b.read_fbo, t);
                    t
                }
            };
            let dstt = match fbo_target.get(&b.draw_fbo).copied() {
                Some(t) => t,
                None => {
                    let t = resolve_target(ctx, b.draw_fbo, None, &mut cmds);
                    fbo_target.insert(b.draw_fbo, t);
                    t
                }
            };
            if let Some(copy) = blit_copy_enc(
                &b.src, &b.dst, src.1, src.3, src.4, dstt.1, dstt.3, dstt.4, b.filter,
            ) {
                cmds.push(Cmd::Submit(CommandBuffer {
                    encoder: vec![copy],
                    signal: None,
                }));
            }
            present = Some(dstt);
            last = Some(dstt);
        }

        let (surface, texture, tw, th, fmt) = present.or(last)?;
        log_frame(
            tw,
            th,
            draws.len(),
            groups.len(),
            cmds.len(),
            Frame::upload_bytes(&cmds),
        );
        Some(Frame {
            cmds,
            present: (surface, texture),
            target_width: tw,
            target_height: th,
            target_format: fmt,
            color_attachments: Vec::new(),
        })
    }
}

/// The render target + presentable surface for a frame whose draws target framebuffer `fbo`. Mints the
/// target's `CreateTexture` + `CreateSurface` (once, cached in the context) and pushes them into `cmds`.
/// Returns `(surface_ir, texture_ir, width, height, format)`.
///
/// * `fbo == 0` (or an FBO with no usable color attachment) → the default window target: `Bgra8Unorm`,
///   sized to the window surface.
/// * a non-default `fbo` with a sized color-attachment texture → an offscreen render target sized to and
///   formatted as that attachment (the "render to a texture instead of the default surface" path).
pub(super) fn resolve_target(
    ctx: &mut GlContext,
    fbo: u32,
    snapshot: Option<crate::model::program::TargetSnapshot>,
    cmds: &mut Vec<Cmd>,
) -> (u32, u32, i32, i32, TextureFormat) {
    // Try the FBO's color attachment; fall back to the default target if it is missing/unsized.
    if fbo != 0 {
        let target = snapshot.or_else(|| {
            let texture = ctx.framebuffers.color_attachment(fbo);
            ctx.textures
                .get(texture)
                .filter(|t| t.w > 0 && t.h > 0)
                .map(|t| crate::model::program::TargetSnapshot {
                    texture,
                    generation: t.gen,
                    width: t.w,
                    height: t.h,
                    format: t.ir_format,
                })
        });
        if let Some(target) = target {
            let (w, h, fmt) = (target.width, target.height, target.format);
            let (surface, texture, needs_create) =
                ctx.fbo_target(target.texture, target.generation);
            if needs_create {
                // Offscreen targets add SAMPLED: a later default-framebuffer pass samples them (the
                // atlas/offscreen → window composite), which the CPU oracle's bind-group check requires.
                push_target_creates(cmds, surface, texture, w, h, fmt, "offscreen-fbo", true);
            }
            return (surface, texture, w, h, fmt);
        }
    }
    let (w, h) = ctx.target_wh();
    let fmt = TextureFormat::Bgra8Unorm;
    // Pass the current window size so a resize retires the stale-sized cached target and mints a fresh one
    // (a stale default target read back at the new size shears the whole composited frame).
    let (surface, texture, needs_create) = ctx.default_target(w, h);
    if needs_create {
        push_target_creates(cmds, surface, texture, w, h, fmt, "default-fbo", false);
    }
    (surface, texture, w, h, fmt)
}

/// Emit the `CreateTexture(RENDER_TARGET | PRESENT)` + matching `CreateSurface` for a render target. When
/// `sampled` a `SAMPLED` usage bit is added so a later render pass may bind this target as a texture (an
/// offscreen FBO sampled by the default-framebuffer composite); the default window target is never sampled.
// These fields are the wire-level CreateTexture/CreateSurface tuple; grouping them would only duplicate
// TextureDesc and SurfaceDesc without creating a domain value.
#[allow(clippy::too_many_arguments)]
pub(super) fn push_target_creates(
    cmds: &mut Vec<Cmd>,
    surface: u32,
    texture: u32,
    w: i32,
    h: i32,
    fmt: TextureFormat,
    label: &str,
    sampled: bool,
) {
    let (w, h) = (w.max(1) as u32, h.max(1) as u32);
    // Offscreen FBO targets (`sampled`) additionally take COPY_DST so a `glBlitFramebuffer` can copy into
    // them (`Enc::CopyTextureToTexture` requires COPY_DST on its destination); the default window target
    // never needs it. This only adds a usage bit — the render/present/copy-src behavior is unchanged.
    let extra = if sampled {
        texture_usage::SAMPLED | texture_usage::COPY_DST
    } else {
        0
    };
    cmds.push(Cmd::CreateTexture(
        texture,
        TextureDesc {
            width: w,
            height: h,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: fmt,
            // COPY_SRC so a `glReadPixels` can copy the rendered target back to a host-readable buffer
            // (the CPU executor requires COPY_SRC on a `CopyTextureToBuffer` source).
            usage: texture_usage::RENDER_TARGET
                | texture_usage::PRESENT
                | texture_usage::COPY_SRC
                | extra,
            label: label.into(),
        },
    ));
    cmds.push(Cmd::CreateSurface(
        surface,
        SurfaceDesc {
            width: w,
            height: h,
            format: fmt,
            hlp_surface: 0,
        },
    ));
}

/// The depth(+stencil) attachment FORMAT a render pass needs, if any of `run`'s geometry draws is depth-
/// or stencil-tested. A stencil-testing draw requires a stencil aspect, so the pass upgrades to
/// `Depth24PlusStencil8`; a purely depth-tested pass keeps the leaner `Depth32Float`. `None` when no draw
/// needs a depth attachment at all (the common 2D path). Every depth-carrying pipeline in the run MUST use
/// this ONE format — wgpu requires the pipeline's depth-stencil format to match the pass attachment — so a
/// pass that stencil-tests any draw lowers ALL its depth pipelines as `Depth24PlusStencil8`.
impl RenderPasses {
    pub(super) fn depth_format(draws: &[DrawCall]) -> Option<TextureFormat> {
        let any_stencil = draws.iter().any(|d| !d.is_clear && d.stencil);
        let any_depth = draws.iter().any(|d| !d.is_clear && d.depth);
        if any_stencil {
            Some(TextureFormat::Depth24PlusStencil8)
        } else if any_depth {
            Some(TextureFormat::Depth32Float)
        } else {
            None
        }
    }
}

/// The depth attachment for a render pass, if any of `run`'s geometry draws is depth- or stencil-tested. A
/// pipeline built with a `DepthState` (see [`lower_draw`]) MUST run in a pass carrying a matching depth
/// attachment — wgpu enforces this — so whenever a draw enables `GL_DEPTH_TEST` (or `GL_STENCIL_TEST`) the
/// pass needs a depth buffer of the format [`pass_depth_format`] chose. Mints one depth texture per
/// `(color target, format)` (cached on the context), emits its `CreateTexture` once into `cmds`, and
/// returns a `DepthAttachment` that clear-loads depth to the far plane (`1.0`, the GL `glClearDepthf`
/// default) and — for a stencil-aspect format — the stencil plane to `glClearStencil`'s value. Returns
/// `None` when no draw in the pass is depth/stencil-tested (the common 2D path), leaving the pass depth-less
/// exactly as before.
pub(super) fn depth_attachment_for(
    ctx: &mut GlContext,
    color_tex: u32,
    w: i32,
    h: i32,
    draws: &[DrawCall],
    cmds: &mut Vec<Cmd>,
) -> Option<DepthAttachment> {
    let format = RenderPasses::depth_format(draws)?;
    let with_stencil = matches!(format, TextureFormat::Depth24PlusStencil8);
    let clear_depth = ctx.clear_depth;
    // GL clears the stencil plane to `glClearStencil`'s value (default 0), masked to the 8-bit buffer.
    let clear_stencil = (ctx.clear_stencil as u32) & 0xff;
    let (depth_tex, needs_create) = ctx.depth_target(color_tex, with_stencil);
    if needs_create {
        cmds.push(Cmd::CreateTexture(
            depth_tex,
            TextureDesc {
                width: w.max(1) as u32,
                height: h.max(1) as u32,
                depth: 1,
                mip_levels: 1,
                sample_count: 1,
                dim: TextureDim::D2,
                format,
                usage: texture_usage::RENDER_TARGET,
                label: if with_stencil {
                    "gl-depth-stencil".into()
                } else {
                    "gl-depth".into()
                },
            },
        ));
    }
    Some(DepthAttachment {
        texture: depth_tex,
        load: LoadOp::Clear,
        clear_depth,
        clear_stencil,
    })
}

/// The effective `LoadOp::Clear` color for a framebuffer run and the index in `run` from which draws still
/// survive. GL semantics: an UNSCISSORED `glClear(GL_COLOR_BUFFER_BIT)` overwrites the ENTIRE color buffer,
/// discarding every prior draw + clear to that framebuffer. So the pass must clear to the color of the LAST
/// unscissored clear in the run and replay ONLY the draws recorded AFTER it — the earlier draws were wiped.
///
/// This is what makes Chrome's window composite render: its default-framebuffer frame interleaves several
/// full clears with geometry (`clear(transparent); …; clear(white); …; clear(orange); …draw tiles`), where
/// the LAST full clear (`#ff7700`, the page background) plus the draws after it are the visible frame. The
/// old "use the first draw's clear + replay all geometry" folded the leading transparent clear onto the pass
/// and kept the wiped pre-clear draws, so the page background (orange) was dropped and the frame read blank.
///
/// A scissored clear only touches its rect, so it does NOT wipe — it is not a boundary here. With no
/// unscissored clear at all the run falls back to `(first-draw clear, 0)` = the prior leading-clear behavior,
/// so an ordinary "clear then draw" frame lowers byte-identically.
impl RenderPasses {
    pub(super) fn effective_clear(run: &[DrawCall]) -> ([f32; 4], usize) {
        let mut last_full: Option<usize> = None;
        for (i, d) in run.iter().enumerate() {
            if d.is_clear && !d.scissor_enabled {
                last_full = Some(i);
            }
        }
        match last_full {
            Some(i) => (run[i].clear, i + 1),
            None => (run.first().map(|d| d.clear).unwrap_or([0.0; 4]), 0),
        }
    }
}

/// Clear-only frame: a render pass over the target that clears it (`LoadOp::Clear`), honoring the LAST
/// unscissored clear's color (see [`effective_clear`]).
impl Frame {
    pub(super) fn build_clear(ctx: &mut GlContext) -> Frame {
        let cmds: Vec<Cmd> = Vec::new();
        let fbo = ctx.draws.last().map(|d| d.fbo).unwrap_or(0);
        let clear = RenderPasses::effective_clear(&ctx.draws).0;
        build_clear_frame_color(ctx, fbo, clear, cmds)
    }
}

// Geometry frame assembly continues in `geometry`.
