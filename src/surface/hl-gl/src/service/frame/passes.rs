use super::*;

pub(super) struct RenderPasses;

impl RenderPasses {
    pub(super) fn build_ordered(ctx: &mut GlContext) -> Option<Frame> {
        use crate::model::context::FrameOp;

        let operations = ctx.local.recording.operations.clone();
        let mut cmds = Vec::new();
        let mut fbo_tex_ir = std::collections::HashMap::new();
        let mut fbo_target = std::collections::HashMap::new();
        let mut initialized = std::collections::HashSet::new();
        let mut snapshots = SnapshotTextures::new();
        let mut targets = Vec::new();
        let mut present = None;
        let mut last = None;
        let mut index = 0;

        while index < operations.len() {
            match &operations[index] {
                FrameOp::Draw(first) => {
                    let fbo = first.fbo;
                    let target = first.target;
                    let start = index;
                    index += 1;
                    while index < operations.len() {
                        match &operations[index] {
                            FrameOp::Draw(draw) if draw.fbo == fbo && draw.target == target => {
                                index += 1
                            }
                            _ => break,
                        }
                    }
                    let run = operations[start..index]
                        .iter()
                        .filter_map(|operation| match operation {
                            FrameOp::Draw(draw) => Some((**draw).clone()),
                            FrameOp::Blit(_) | FrameOp::CopyTex(_) => None,
                        })
                        .collect::<Vec<_>>();
                    let target = lower_ordered_run(
                        ctx,
                        &run,
                        &mut cmds,
                        &mut fbo_tex_ir,
                        &mut fbo_target,
                        &mut initialized,
                        &mut targets,
                        &mut snapshots,
                    )?;
                    last = Some(target);
                    if fbo == 0 {
                        present = Some(target);
                    }
                }
                FrameOp::Blit(blit) => {
                    let src = resolve_ordered_target(
                        ctx,
                        blit.read_fbo,
                        blit.read_target,
                        blit.read_ir,
                        &mut cmds,
                        &mut fbo_target,
                        &mut targets,
                    );
                    let dst = resolve_ordered_target(
                        ctx,
                        blit.draw_fbo,
                        blit.draw_target,
                        blit.draw_ir,
                        &mut cmds,
                        &mut fbo_target,
                        &mut targets,
                    );
                    if let Some(copy) = blit_copy_enc(
                        &blit.src,
                        &blit.dst,
                        src.1,
                        src.3,
                        src.4,
                        dst.1,
                        dst.3,
                        dst.4,
                        blit.filter,
                    ) {
                        cmds.push(Cmd::Submit(CommandBuffer {
                            encoder: vec![copy],
                            signal: None,
                        }));
                        initialized.insert(dst.1);
                    }
                    last = Some(dst);
                    if blit.draw_fbo == 0 {
                        present = Some(dst);
                    }
                    index += 1;
                }
                FrameOp::CopyTex(copy) => {
                    let src = resolve_ordered_target(
                        ctx, copy.read_fbo, copy.read_target, copy.read_ir, &mut cmds,
                        &mut fbo_target, &mut targets,
                    );
                    let (dst, _) = prepare_copy_destination(
                        ctx, copy.texture, copy.cube, &mut cmds,
                    )?;
                    let width = copy.extent[0].max(0) as u32;
                    let height = copy.extent[1].max(0) as u32;
                    let src_y = src.3.saturating_sub(copy.src[1]).saturating_sub(copy.extent[1]);
                    cmds.push(Cmd::Submit(CommandBuffer {
                        encoder: vec![Enc::BlitTexture {
                            src: src.1,
                            src_sub: TextureSubresource::base(),
                            src_origin: Origin3d {
                                x: copy.src[0].max(0) as u32,
                                y: src_y.max(0) as u32,
                                z: 0,
                            },
                            src_extent: Extent3d { width, height, depth: 1 },
                            dst,
                            dst_sub: TextureSubresource {
                                mip: copy.level,
                                layer: copy.face,
                                aspect: hl_gpu::protocol::model::enums::TextureAspect::All,
                            },
                            dst_origin: Origin3d {
                                x: copy.dst[0].max(0) as u32,
                                y: copy.dst[1].max(0) as u32,
                                z: 0,
                            },
                            dst_extent: Extent3d { width, height, depth: 1 },
                            filter: Filter::Nearest,
                            mirror: Mirror::NONE,
                        }],
                        signal: None,
                    }));
                    last = Some(src);
                    if copy.read_fbo == 0 {
                        present = Some(src);
                    }
                    index += 1;
                }
            }
        }

        let (surface, texture, tw, th, fmt) = present.or(last)?;
        Some(Frame {
            cmds,
            present: (surface, texture),
            target_width: tw,
            target_height: th,
            target_format: fmt,
            color_attachments: Vec::new(),
            targets,
        })
    }

    /// Whether this framebuffer's rows are stored in GL texel order — row 0 is the framebuffer's BOTTOM.
    ///
    /// This is the authoritative row-order contract for every lowered render target:
    ///
    /// * INTERNAL targets — the default framebuffer and ordinary FBO textures — store rows top-down as the
    ///   framebuffer is viewed (row 0 = its top). GL clip space and WebGPU clip space already agree on which
    ///   edge is up, so nothing is reflected; instead every crossing back into GL WINDOW or TEXEL coordinates
    ///   converts `h-1-y`: [`emit_viewport`], [`emit_scissor`], [`clear_rect_enc`],
    ///   `adapter::glsl::fragment_coordinates`, `blit_copy_enc`, the rendered-FBO sampler `flip_y`, and
    ///   `readpixels::pack_region`. A window frame therefore reaches its IOSurface or `wl_shm` buffer — both
    ///   top-down — upright, with NO flip anywhere on the present path.
    /// * An IMPORTED EXTERNAL image is the one exception. The guest, not this driver, owns what the foreign
    ///   consumer reads out of that memory (Chrome attaches an imported EGLImage to a non-zero FBO and
    ///   publishes it at `glFlush`), so the driver must materialize true GL FBO semantics, in which texel row
    ///   0 is the framebuffer's BOTTOM. Those draws get the clip reflection, the reversed triangle winding it
    ///   implies, and un-converted window rows.
    ///
    /// Reflecting the DEFAULT framebuffer as well — which this predicate used to do — mirrored every
    /// presented frame of an ordinary GL application while leaving Chrome upright, because Chrome already
    /// pre-flips its own projection for a top-left scanout image and the two flips cancelled.
    pub(super) fn stores_bottom_up_rows(
        ctx: &GlContext,
        target: Option<crate::model::program::TargetSnapshot>,
    ) -> bool {
        target.is_some_and(|target| {
            ctx.external_target(target.texture, target.generation)
                .is_some()
        })
    }

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
        let draws = ctx.local.recording.draws.clone();
        let blits = ctx.local.recording.blits.clone();
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
        let mut targets: Vec<FrameTarget> = Vec::new();
        let mut snapshots = SnapshotTextures::new();
        // Render targets already written by an earlier pass of THIS frame; a later pass over one of them
        // load-preserves instead of clearing (see [`lower_ordered_run`]).
        let mut initialized: std::collections::HashSet<u32> = std::collections::HashSet::new();

        for &(fbo, start, end) in groups {
            let run = &draws[start..end];
            let ((surface, target_tex, tw, th, fmt), resolved_attachment) =
                resolve_target_with_identity(
                    ctx,
                    fbo,
                    run.first().and_then(|d| d.target),
                    &mut cmds,
                );
            fbo_target.insert(fbo, (surface, target_tex, tw, th, fmt));
            if let Some(target) = frame_target(
                ctx,
                fbo,
                run.first().and_then(|draw| draw.target),
                target_tex,
                tw,
                th,
                fmt,
            ) {
                push_final_target(&mut targets, target);
            }
            // Register the attachment resolve_target ACTUALLY selected. Its live-attachment fallback is
            // intentionally wider than the first draw's optional snapshot; re-deriving from that snapshot
            // left a freshly minted target invisible to a later sampler in the same frame.
            register_resolved_target(&mut fbo_tex_ir, resolved_attachment, target_tex);
            // Only an unscissored COLOUR clear wipes the run's target, so the pass clear-loads with the LAST
            // such clear's colour and replays only the draws recorded AFTER it (see [`full_clear`]). With no
            // such clear the pass load-preserves — unless this target has not been written yet in this frame,
            // which mirrors [`lower_ordered_run`]. Scissored colour clears and depth/stencil clears split the
            // run into segments exactly as the single-target builder does; a depth-only `glClear` therefore no
            // longer repaints the colour attachment here either.
            let (full, rstart) = RenderPasses::full_clear(run);
            let clear = full.unwrap_or_else(|| RenderPasses::fallback_clear(run));
            let survivors = &run[rstart..];
            let load = if full.is_some() || !initialized.contains(&target_tex) {
                LoadOp::Clear
            } else {
                LoadOp::Load
            };
            let mut ops: Vec<Enc> = Vec::new();
            lower_segments(
                ctx,
                survivors,
                depth_load(&run[..rstart]),
                SegmentTarget {
                    fbo,
                    texture: target_tex,
                    format: fmt,
                    width: tw,
                    height: th,
                    bottom_up: RenderPasses::stores_bottom_up_rows(
                        ctx,
                        run.first().and_then(|d| d.target),
                    ),
                },
                clear,
                load,
                &mut cmds,
                &mut ops,
                &fbo_tex_ir,
                &mut snapshots,
            );
            if !ops.is_empty() {
                cmds.push(Cmd::Submit(CommandBuffer {
                    encoder: ops,
                    signal: None,
                }));
                initialized.insert(target_tex);
            }

            last = Some((surface, target_tex, tw, th, fmt));
            if fbo == 0 {
                present = Some((surface, target_tex, tw, th, fmt));
            }
        }

        // Apply the recorded `glBlitFramebuffer` copies AFTER the render passes. Each copies a sub-rect from the
        // read FBO's resolved render target into the draw FBO's, lowered to `Enc::CopyTextureToTexture` for the
        // equal-size (non-scaling) case. A blit into framebuffer 0 updates the window target. Offscreen
        // destinations must not replace a window target selected by an earlier pass; they become the fallback
        // only for a frame that contains no default-framebuffer work.
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
            if let Some(target) =
                frame_target(ctx, b.draw_fbo, None, dstt.1, dstt.2, dstt.3, dstt.4)
            {
                push_final_target(&mut targets, target);
            }
            if let Some(copy) = blit_copy_enc(
                &b.src, &b.dst, src.1, src.3, src.4, dstt.1, dstt.3, dstt.4, b.filter,
            ) {
                cmds.push(Cmd::Submit(CommandBuffer {
                    encoder: vec![copy],
                    signal: None,
                }));
            }
            if b.draw_fbo == 0 {
                present = Some(dstt);
            }
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
            targets,
        })
    }
}

type ResolvedTarget = (u32, u32, i32, i32, TextureFormat);

fn resolve_ordered_target(
    ctx: &mut GlContext,
    fbo: u32,
    snapshot: Option<crate::model::program::TargetSnapshot>,
    transferred: Option<u32>,
    cmds: &mut Vec<Cmd>,
    fbo_target: &mut std::collections::HashMap<u32, ResolvedTarget>,
    targets: &mut Vec<FrameTarget>,
) -> ResolvedTarget {
    if let (Some(texture), Some(snapshot)) = (transferred, snapshot) {
        let target = (
            ctx.fbo_surface(snapshot.texture, snapshot.generation)
                .unwrap_or_default(),
            texture,
            snapshot.width,
            snapshot.height,
            snapshot.format,
        );
        fbo_target.insert(fbo, target);
        if let Some(frame_target) = frame_target(
            ctx,
            fbo,
            Some(snapshot),
            texture,
            target.2,
            target.3,
            target.4,
        ) {
            push_final_target(targets, frame_target);
        }
        return target;
    }
    if snapshot.is_none() {
        if let Some(target) = fbo_target.get(&fbo).copied() {
            return target;
        }
    }
    let target = resolve_target(ctx, fbo, snapshot, cmds);
    fbo_target.insert(fbo, target);
    if let Some(frame_target) =
        frame_target(ctx, fbo, snapshot, target.1, target.2, target.3, target.4)
    {
        push_final_target(targets, frame_target);
    }
    target
}

#[allow(clippy::too_many_arguments)]
fn lower_ordered_run(
    ctx: &mut GlContext,
    run: &[DrawCall],
    cmds: &mut Vec<Cmd>,
    fbo_tex_ir: &mut std::collections::HashMap<(u32, u64), u32>,
    fbo_target: &mut std::collections::HashMap<u32, ResolvedTarget>,
    initialized: &mut std::collections::HashSet<u32>,
    targets: &mut Vec<FrameTarget>,
    snapshots: &mut SnapshotTextures,
) -> Option<ResolvedTarget> {
    let first = run.first()?;
    let fbo = first.fbo;
    let (target, resolved_attachment) = resolve_target_with_identity(ctx, fbo, first.target, cmds);
    let bottom_up = RenderPasses::stores_bottom_up_rows(ctx, first.target);
    fbo_target.insert(fbo, target);
    if let Some(frame_target) = frame_target(
        ctx,
        fbo,
        first.target,
        target.1,
        target.2,
        target.3,
        target.4,
    ) {
        push_final_target(targets, frame_target);
    }
    register_resolved_target(fbo_tex_ir, resolved_attachment, target.1);

    // Only an UNSCISSORED color clear justifies wiping the whole attachment; with none, the first pass must
    // LOAD unless this target has not been written yet in this frame.
    let (full, start) = RenderPasses::full_clear(run);
    let clear = full.unwrap_or([0.0; 4]);
    let survivors = &run[start..];
    let load = if full.is_some() || !initialized.contains(&target.1) {
        LoadOp::Clear
    } else {
        LoadOp::Load
    };
    let mut ops = Vec::new();
    lower_segments(
        ctx,
        survivors,
        depth_load(&run[..start]),
        SegmentTarget {
            fbo,
            texture: target.1,
            format: target.4,
            width: target.2,
            height: target.3,
            bottom_up,
        },
        clear,
        load,
        cmds,
        &mut ops,
        fbo_tex_ir,
        snapshots,
    );
    if !ops.is_empty() {
        cmds.push(Cmd::Submit(CommandBuffer {
            encoder: ops,
            signal: None,
        }));
        initialized.insert(target.1);
    }
    Some(target)
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
    resolve_target_with_identity(ctx, fbo, snapshot, cmds).0
}

fn resolve_target_with_identity(
    ctx: &mut GlContext,
    fbo: u32,
    snapshot: Option<crate::model::program::TargetSnapshot>,
    cmds: &mut Vec<Cmd>,
) -> (
    ResolvedTarget,
    Option<crate::model::program::TargetSnapshot>,
) {
    // Try the FBO's color attachment; fall back to the default target if it is missing/unsized.
    if fbo != 0 {
        let target = snapshot.or_else(|| {
            ctx.framebuffer_color_texture(fbo, 0)
                .filter(|(_, t)| t.w > 0 && t.h > 0)
                .map(|(texture, t)| crate::model::program::TargetSnapshot {
                    texture,
                    generation: t.gen,
                    shared_storage: t.shared_storage(),
                    shared_revision: t.shared_current_identity().map(|(_, revision)| revision),
                    width: t.w,
                    height: t.h,
                    format: t.ir_format,
                })
        });
        if let Some(target) = target {
            let (w, h, fmt) = (target.width, target.height, target.format);
            let (surface, texture, needs_create, ephemeral) = ctx
                .recorded_fbo_target(target.texture, target.generation)
                .unwrap_or_default();
            if needs_create {
                // Offscreen targets add SAMPLED: a later default-framebuffer pass samples them (the
                // atlas/offscreen → window composite), which the CPU oracle's bind-group check requires.
                push_target_creates(
                    cmds,
                    surface,
                    texture,
                    w,
                    h,
                    fmt,
                    if ephemeral {
                        "gl-retired-fbo"
                    } else {
                        "offscreen-fbo"
                    },
                    true,
                    ctx.external_target(target.texture, target.generation),
                );
            }
            return ((surface, texture, w, h, fmt), Some(target));
        }
    }
    let (w, h) = ctx.target_wh();
    let fmt = TextureFormat::Bgra8Unorm;
    // Pass the current window size so a resize retires the stale-sized cached target and mints a fresh one
    // (a stale default target read back at the new size shears the whole composited frame).
    let (surface, texture, needs_create) = ctx.default_target(w, h).unwrap_or_default();
    if needs_create {
        push_target_creates(
            cmds,
            surface,
            texture,
            w,
            h,
            fmt,
            "default-fbo",
            false,
            ctx.local.present_token,
        );
    }
    ((surface, texture, w, h, fmt), None)
}

fn register_resolved_target(
    targets: &mut std::collections::HashMap<(u32, u64), u32>,
    attachment: Option<crate::model::program::TargetSnapshot>,
    texture: u32,
) {
    if let Some(attachment) = attachment.filter(|attachment| attachment.texture != 0) {
        targets.insert((attachment.texture, attachment.generation), texture);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::context::FrameOp;
    use crate::model::glconst::{
        GL_FRAGMENT_SHADER, GL_TEXTURE_2D, GL_TRIANGLES, GL_VERTEX_SHADER,
    };
    use crate::service::record::{attach_shader, bind_texture, shader_source};

    fn fallback_then_sample() -> (GlContext, u32) {
        let mut ctx = GlContext::new();
        ctx.local.surf.have = true;
        ctx.local.surf.width = 16;
        ctx.local.surf.height = 16;

        let texture = ctx.textures.gen();
        assert!(ctx
            .textures
            .image_2d(texture, 8, 4, &[], TextureFormat::Rgba8Unorm));
        let fbo = ctx.local.framebuffers.gen();
        ctx.local.framebuffers.attach_color(fbo, texture);

        let vertex = ctx.create_shader(GL_VERTEX_SHADER);
        shader_source(
            &mut ctx,
            vertex,
            "#version 300 es\nvoid main(){gl_Position=vec4(float((gl_VertexID<<1)&2)-1.0,float(gl_VertexID&2)-1.0,0.0,1.0);}",
        );
        ctx.compile_shader(vertex);
        let fragment = ctx.create_shader(GL_FRAGMENT_SHADER);
        shader_source(
            &mut ctx,
            fragment,
            "#version 300 es\nprecision highp float; uniform sampler2D source; out vec4 color; void main(){color=texture(source,vec2(0.5));}",
        );
        ctx.compile_shader(fragment);
        let program = ctx.create_program();
        attach_shader(&mut ctx, program, vertex);
        attach_shader(&mut ctx, program, fragment);
        assert!(ctx.link_program(program));
        ctx.use_program(program);
        bind_texture(&mut ctx, GL_TEXTURE_2D, texture);

        let first = DrawCall {
            fbo,
            target: None,
            is_clear: true,
            clear_mask: crate::model::glconst::GL_COLOR_BUFFER_BIT,
            ..DrawCall::default()
        };
        crate::service::record::draw_arrays(&mut ctx, GL_TRIANGLES, 0, 3);
        let sample = ctx.local.recording.draws.pop().unwrap();
        ctx.local.recording.draws = vec![first, sample];
        (ctx, texture)
    }

    fn assert_sampler_uses_fallback_target(frame: &Frame, target_ir: u32) {
        assert!(frame.cmds.iter().any(|command| matches!(
            command,
            Cmd::CreateBindGroup(_, descriptor)
                if descriptor.entries.iter().any(|entry| matches!(
                    entry.resource,
                    BindResource::Texture { id } if id == target_ir
                ))
        )));
    }

    #[test]
    fn build_multi_later_sampler_uses_live_attachment_fallback_target() {
        let (mut ctx, texture) = fallback_then_sample();
        let generation = ctx.textures.get(texture).unwrap().gen;
        let groups = RenderPasses::groups(&ctx.local.recording.draws);

        let frame = RenderPasses::build_multi(&mut ctx, &groups).unwrap();
        let target_ir = ctx.resident_fbo_target_tex(texture, generation).unwrap();

        assert_sampler_uses_fallback_target(&frame, target_ir);
    }

    #[test]
    fn build_ordered_later_sampler_uses_live_attachment_fallback_target() {
        let (mut ctx, texture) = fallback_then_sample();
        let generation = ctx.textures.get(texture).unwrap().gen;
        ctx.local.recording.operations = ctx
            .local
            .recording
            .draws
            .iter()
            .cloned()
            .map(|draw| FrameOp::Draw(Box::new(draw)))
            .collect();

        let frame = RenderPasses::build_ordered(&mut ctx).unwrap();
        let target_ir = ctx.resident_fbo_target_tex(texture, generation).unwrap();

        assert_sampler_uses_fallback_target(&frame, target_ir);
    }

    #[test]
    fn copy_tex_sub_image_lowers_after_source_render_with_mip_and_offsets() {
        let mut ctx = GlContext::new();
        ctx.local.surf.have = true;
        ctx.local.surf.width = 64;
        ctx.local.surf.height = 48;
        let texture = ctx.textures.gen();
        ctx.local.tex_unit[ctx.local.active_texture] = texture;
        assert!(ctx.textures.image_2d(texture, 16, 8, &[0x31; 16 * 8 * 4], TextureFormat::Rgba8Unorm));
        assert!(ctx.textures.image_2d_level(texture, 1, 8, 4, &[0x52; 8 * 4 * 4]));
        ctx.record_draw(DrawCall {
            fbo: 0,
            is_clear: true,
            clear_mask: crate::model::glconst::GL_COLOR_BUFFER_BIT,
            clear: [0.2, 0.4, 0.6, 1.0],
            ..DrawCall::default()
        });

        crate::service::record::copy_tex_sub_image_2d(
            &mut ctx, GL_TEXTURE_2D, 1, 2, 1, 7, 9, 3, 2,
        );
        let frame = RenderPasses::build_ordered(&mut ctx).expect("ordered copy frame");
        let submits = frame.cmds.iter().filter_map(|command| match command {
            Cmd::Submit(buffer) => Some(buffer),
            _ => None,
        }).collect::<Vec<_>>();
        let render = submits.iter().position(|buffer| buffer.encoder.iter().any(|op| matches!(op, Enc::BeginRenderPass { .. }))).unwrap();
        let copy = submits.iter().position(|buffer| buffer.encoder.iter().any(|op| matches!(op,
            Enc::BlitTexture {
                src_origin: Origin3d { x: 7, y: 37, z: 0 },
                dst_sub: TextureSubresource { mip: 1, .. },
                dst_origin: Origin3d { x: 2, y: 1, z: 0 },
                src_extent: Extent3d { width: 3, height: 2, depth: 1 },
                ..
            }
        ))).unwrap();
        assert!(render < copy, "the source draw must execute before its framebuffer copy");
        assert!(frame.cmds.iter().any(|command| matches!(command,
            Cmd::CreateTexture(_, TextureDesc { mip_levels: 2, usage, .. })
                if usage & texture_usage::COPY_DST != 0
        )));
    }

    #[test]
    fn cube_copy_records_the_selected_face_and_level() {
        let mut ctx = GlContext::new();
        ctx.local.surf.have = true;
        ctx.local.surf.width = 32;
        ctx.local.surf.height = 32;
        let texture = ctx.textures.gen();
        ctx.local.tex_unit[ctx.local.active_texture] = texture;
        for face in 0..6 {
            assert!(ctx.textures.image_cube_face(
                texture, face, 8, 8, &[0; 8 * 8 * 4], TextureFormat::Rgba8Unorm, GL_RGBA,
            ));
            assert!(ctx.textures.image_cube_level(
                texture, face, 1, 4, 4, &[0; 4 * 4 * 4], GL_RGBA,
            ));
            assert!(ctx.textures.image_cube_level(
                texture, face, 2, 2, 2, &[0; 2 * 2 * 4], GL_RGBA,
            ));
            assert!(ctx.textures.image_cube_level(
                texture, face, 3, 1, 1, &[0; 4], GL_RGBA,
            ));
        }
        ctx.record_draw(DrawCall {
            fbo: 0,
            is_clear: true,
            clear_mask: crate::model::glconst::GL_COLOR_BUFFER_BIT,
            ..DrawCall::default()
        });
        crate::service::record::copy_tex_sub_image_2d(
            &mut ctx, crate::model::glconst::GL_TEXTURE_CUBE_MAP_NEGATIVE_Z,
            1, 1, 2, 3, 4, 2, 1,
        );
        assert!(matches!(ctx.local.recording.operations.last(),
            Some(FrameOp::CopyTex(copy)) if copy.cube && copy.face == 5 && copy.level == 1
                && copy.dst == [1, 2] && copy.src == [3, 4]
        ));
        let frame = RenderPasses::build_ordered(&mut ctx).expect("cube copy frame");
        assert!(frame.cmds.iter().any(|command| matches!(command,
            Cmd::CreateTexture(_, TextureDesc { depth: 6, mip_levels: 4, dim: TextureDim::Cube, .. })
        )));
        assert!(frame.cmds.iter().any(|command| matches!(command,
            Cmd::Submit(buffer) if buffer.encoder.iter().any(|op| matches!(op,
                Enc::BlitTexture {
                    dst_sub: TextureSubresource { mip: 1, layer: 5, .. },
                    dst_origin: Origin3d { x: 1, y: 2, z: 0 },
                    ..
                }
            ))
        )));
    }

    #[test]
    fn copy_tex_reads_the_rendered_fbo_target_not_its_cpu_shadow() {
        let mut ctx = GlContext::new();
        let source = ctx.textures.gen();
        assert!(ctx.textures.image_2d(source, 8, 8, &[], TextureFormat::Rgba8Unorm));
        let fbo = ctx.local.framebuffers.gen();
        ctx.local.framebuffers.attach_color(fbo, source);
        ctx.record_draw(DrawCall {
            fbo,
            is_clear: true,
            clear_mask: crate::model::glconst::GL_COLOR_BUFFER_BIT,
            target: Some(crate::model::program::TargetSnapshot {
                texture: source,
                generation: ctx.textures.get(source).unwrap().gen,
                shared_storage: None,
                shared_revision: None,
                width: 8,
                height: 8,
                format: TextureFormat::Rgba8Unorm,
            }),
            ..DrawCall::default()
        });
        let destination = ctx.textures.gen();
        assert!(ctx.textures.image_2d(destination, 4, 4, &[0; 4 * 4 * 4], TextureFormat::Rgba8Unorm));
        ctx.local.tex_unit[ctx.local.active_texture] = destination;
        ctx.local.read_fbo = fbo;
        crate::service::record::copy_tex_sub_image_2d(
            &mut ctx, GL_TEXTURE_2D, 0, 0, 0, 2, 1, 4, 4,
        );

        let frame = RenderPasses::build_ordered(&mut ctx).expect("FBO copy frame");
        let source_ir = ctx
            .resident_fbo_target_tex(source, ctx.textures.get(source).unwrap().gen)
            .expect("render target residency");
        assert!(frame.cmds.iter().any(|command| matches!(command,
            Cmd::Submit(buffer) if buffer.encoder.iter().any(|op| matches!(op,
                Enc::BlitTexture { src, .. } if *src == source_ir
            ))
        )));
    }

    #[test]
    fn combined_color_depth_stencil_clear_is_not_discarded_by_color_fold() {
        let clear = DrawCall {
            is_clear: true,
            clear_mask: crate::model::glconst::GL_COLOR_BUFFER_BIT
                | crate::model::glconst::GL_DEPTH_BUFFER_BIT
                | crate::model::glconst::GL_STENCIL_BUFFER_BIT,
            color_mask: 0xf,
            depth_write: true,
            stencil_write_mask_front: 0xff,
            stencil_write_mask_back: 0xff,
            clear: [0.125, 0.25, 0.5, 1.0],
            clear_stencil: 123,
            ..DrawCall::default()
        };

        let run = [clear];
        assert_eq!(
            RenderPasses::full_clear(&run),
            (Some([0.125, 0.25, 0.5, 1.0]), 1),
            "the color half remains eligible for the attachment-load fast path"
        );
        let depth = depth_load(&run);
        assert_eq!(depth.load, LoadOp::Clear);
        assert_eq!(depth.stencil, 123);
    }

    #[test]
    fn color_only_clear_still_uses_the_folded_fast_path() {
        let clear = DrawCall {
            is_clear: true,
            clear_mask: crate::model::glconst::GL_COLOR_BUFFER_BIT,
            color_mask: 0xf,
            clear: [0.125, 0.25, 0.5, 1.0],
            ..DrawCall::default()
        };

        assert_eq!(
            RenderPasses::full_clear(&[clear]),
            (Some([0.125, 0.25, 0.5, 1.0]), 1)
        );
    }

    #[test]
    fn color_depth_and_color_stencil_clears_each_keep_the_non_color_half() {
        for extra in [
            crate::model::glconst::GL_DEPTH_BUFFER_BIT,
            crate::model::glconst::GL_STENCIL_BUFFER_BIT,
        ] {
            let clear = DrawCall {
                is_clear: true,
                clear_mask: crate::model::glconst::GL_COLOR_BUFFER_BIT | extra,
                color_mask: 0xf,
                depth_write: true,
                stencil_write_mask_front: 0xff,
                stencil_write_mask_back: 0xff,
                ..DrawCall::default()
            };
            let run = [clear];
            assert!(RenderPasses::full_clear(&run).0.is_some());
            assert_eq!(depth_load(&run).load, LoadOp::Clear);
        }
    }

    #[test]
    fn clear_only_combined_clear_emits_each_attachment_effect_once() {
        let mut ctx = GlContext::new();
        ctx.local.surf.have = true;
        ctx.local.surf.width = 16;
        ctx.local.surf.height = 16;
        ctx.local.recording.draws.push(DrawCall {
            is_clear: true,
            clear_mask: crate::model::glconst::GL_COLOR_BUFFER_BIT
                | crate::model::glconst::GL_DEPTH_BUFFER_BIT
                | crate::model::glconst::GL_STENCIL_BUFFER_BIT,
            color_mask: 0xf,
            depth_write: true,
            stencil_write_mask_front: 0xff,
            stencil_write_mask_back: 0xff,
            clear: [0.125, 0.25, 0.5, 1.0],
            clear_depth: 0.25,
            clear_stencil: 123,
            ..DrawCall::default()
        });

        let frame = Frame::build_clear(&mut ctx).expect("combined clear produces a frame");
        let submits: Vec<_> = frame
            .cmds
            .iter()
            .filter_map(|cmd| match cmd {
                Cmd::Submit(buffer) => Some(buffer),
                _ => None,
            })
            .collect();
        assert_eq!(submits.len(), 1, "one clear call needs one render pass");
        let begins: Vec<_> = submits[0]
            .encoder
            .iter()
            .filter_map(|op| match op {
                Enc::BeginRenderPass { color, depth } => Some((color, depth)),
                _ => None,
            })
            .collect();
        assert_eq!(begins.len(), 1, "no duplicate attachment clear pass");
        assert_eq!(begins[0].0[0].load, LoadOp::Clear);
        let depth = begins[0].1.as_ref().expect("depth/stencil attachment");
        assert_eq!(depth.load, LoadOp::Clear);
        assert_eq!(depth.clear_depth, 0.25);
        assert_eq!(depth.clear_stencil, 123);
    }
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
    token: Option<hl_gpu::protocol::model::descriptor::SurfaceToken>,
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
    if let Some(token) = token {
        cmds.push(Cmd::CreateSurface(
            surface,
            SurfaceDesc {
                width: w,
                height: h,
                format: fmt,
                token,
            },
        ));
    }
}

/// The depth(+stencil) attachment FORMAT a render pass needs, if any of `run`'s geometry draws is depth-
/// or stencil-tested, or a clear armed the plane. A stencil-testing draw requires a stencil aspect, so the pass upgrades to
/// `Depth24PlusStencil8`; a purely depth-tested pass keeps the leaner `Depth32Float`. `None` when no draw
/// needs a depth attachment at all (the common 2D path). Every depth-carrying pipeline in the run MUST use
/// this ONE format — wgpu requires the pipeline's depth-stencil format to match the pass attachment — so a
/// pass that stencil-tests any draw lowers ALL its depth pipelines as `Depth24PlusStencil8`.
impl RenderPasses {
    /// [`Self::depth_format_with`], also honouring a depth/stencil CLEAR that armed this pass.
    ///
    /// A `glClear(GL_DEPTH_BUFFER_BIT)` in a frame where no draw happens to be depth-tested still has to
    /// land: the plane it wrote is what the NEXT frame's depth test reads. Deciding the attachment from
    /// the draws alone dropped that clear entirely — `glClearDepthf(0.5); glClear(DEPTH); draw(untested)`
    /// materialized no depth plane at all, so when a later frame first enabled `GL_DEPTH_TEST` the plane
    /// was created fresh and cleared to the GL initial 1.0, and every fragment passed `GL_LESS`.
    pub(super) fn depth_format_with(
        draws: &[DrawCall],
        clear: DepthClear,
        stencil_available: bool,
    ) -> Option<TextureFormat> {
        // Clearing or testing a missing stencil attachment has no storage to preserve. In particular, do
        // not upgrade a depth-only FBO to a different host texture merely because GL_STENCIL_BUFFER_BIT
        // was included in a combined clear.
        // A RECT clear writes the plane with a draw, so the pass must carry the attachment for it just as
        // much as a depth-tested draw or a load-op clear does.
        let any_stencil = stencil_available
            && (draws
                .iter()
                .any(|d| !d.is_clear && d.stencil || d.needs_rect_clear() && d.clears_stencil())
                || clear.stencil_armed);
        let any_depth = draws
            .iter()
            .any(|d| !d.is_clear && d.depth || d.needs_rect_clear() && d.clears_depth())
            || matches!(clear.load, LoadOp::Clear);
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
    fbo: u32,
    color_tex: u32,
    w: i32,
    h: i32,
    draws: &[DrawCall],
    cmds: &mut Vec<Cmd>,
    clear: DepthClear,
) -> Option<DepthAttachment> {
    let stencil_available = fbo == 0 || ctx.local.framebuffers.has_stencil(fbo);
    let format = RenderPasses::depth_format_with(draws, clear, stencil_available)?;
    let with_stencil = matches!(format, TextureFormat::Depth24PlusStencil8);
    let clear_depth = clear.depth;
    // GL clears the stencil plane to `glClearStencil`'s value (default 0), masked to the 8-bit buffer.
    let clear_stencil = (clear.stencil as u32) & 0xff;
    let (depth_tex, needs_create) = ctx.depth_target(fbo, color_tex, with_stencil).ok()?;
    // A depth texture minted this frame has no prior contents to preserve — and a zero-initialized depth
    // plane fails every `GL_LESS` test — so its first pass always clear-loads regardless of the caller's op.
    let load = if needs_create {
        LoadOp::Clear
    } else {
        clear.load
    };
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
        load,
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
/// A scissored clear only touches its rect, so it does NOT wipe — it is not a boundary here, and its color is
/// NEVER the full-target clear color (that is literally "scissor ignored"). With no unscissored clear at all
/// the run falls back to `(first non-scissored-clear draw's clear, 0)` = the prior leading-clear behavior, so
/// an ordinary "clear then draw" frame lowers byte-identically. Callers that decide a `LoadOp` must ask
/// [`full_clear`]: only `Some` there justifies clearing the attachment at all.
impl RenderPasses {
    pub(super) fn effective_clear(run: &[DrawCall]) -> ([f64; 4], usize) {
        let (clear, start) = Self::full_clear(run);
        (clear.unwrap_or_else(|| Self::fallback_clear(run)), start)
    }

    /// `Some(color)` only when an UNSCISSORED clear in the run justifies clearing the WHOLE target, plus the
    /// index from which draws survive. `None` means nothing wiped the target: the pass must `LoadOp::Load`,
    /// not clear, or it destroys everything outside a scissored clear's rect.
    pub(super) fn full_clear(run: &[DrawCall]) -> (Option<[f64; 4]>, usize) {
        Self::full_clear_slot(run, 0)
    }

    /// [`full_clear`] for one color-attachment `slot`. A `glClearBuffer*`-scoped clear wipes only the
    /// attachment it names (see [`DrawCall::clear_draw_buffer`]), so asking per slot is what keeps a clear
    /// of attachment 1 from wiping attachment 0 — and from discarding the draws recorded before it there.
    pub(super) fn full_clear_slot(run: &[DrawCall], slot: u32) -> (Option<[f64; 4]>, usize) {
        let mut last_full: Option<usize> = None;
        for (i, d) in run.iter().enumerate() {
            if d.clears_color_slot(slot) && !d.scissor_enabled {
                last_full = Some(i);
            }
        }
        let Some(index) = last_full else {
            return (None, 0);
        };
        // Folding the clear into the pass load op DISCARDS every call before it — which is only sound if
        // those earlier calls affected nothing but colour. A draw that wrote the stencil or depth plane
        // leaves a result the colour clear does not erase, and a later draw may test against it: dropping
        // it silently dropped the side effect. The selected clear's own depth/stencil halves are carried
        // separately by `depth_load` even though its color half is folded. Three `GL_INCR` draws followed
        // by a `glClear(GL_COLOR)` and a `GL_EQUAL` test read black for exactly this reason. Refusing the
        // fold sends the run down the segmented path, where the clear becomes a full-target `Enc::ClearRect`
        // between two passes and the stencil plane load-preserves across it.
        if run[..index].iter().any(|draw| {
            draw.writes_depth_or_stencil() || draw.clears_depth() || draw.clears_stencil()
        }) {
            return (None, 0);
        }
        (Some(run[index].clear), index + 1)
    }

    /// The full-target clear color for a run with no unscissored clear: the first draw's recorded clear color
    /// (the prior leading-clear behavior), SKIPPING scissored clears. A scissored clear paints only its rect,
    /// so promoting its color to the whole attachment is exactly "scissor ignored".
    pub(super) fn fallback_clear(run: &[DrawCall]) -> [f64; 4] {
        run.iter()
            .find(|d| !d.is_clear || (d.clears_color() && !d.scissor_enabled))
            .map(|d| d.clear)
            .unwrap_or([0.0; 4])
    }
}

/// Clear-only frame: a render pass over the target that clears it (`LoadOp::Clear`), honoring the LAST
/// unscissored clear's color (see [`effective_clear`]).
impl Frame {
    pub(super) fn build_clear(ctx: &mut GlContext) -> Option<Frame> {
        // GL specifies that `glClear` is scissor-tested. A frame whose only recorded ops are clears still
        // has to honor that: a scissored clear paints a sub-rect and must not become a full-target clear (nor
        // be dropped when a full clear precedes it, which is what a plain clear-load did). The geometry
        // builder already lowers a scissored clear to `Enc::ClearRect` between render-pass segments and
        // handles a segment list with no geometry at all, so route there and keep the full-target clear as
        // the fallback.
        let (_, start) = RenderPasses::effective_clear(&ctx.local.recording.draws);
        // A multiple-render-target framebuffer needs the per-attachment loads only the geometry builder
        // computes: this single-target path would apply a `glClearBufferfv(GL_COLOR, 1, …)` to attachment 0.
        let mrt = ctx
            .local
            .recording
            .draws
            .first()
            .map(|draw| draw.fbo)
            .is_some_and(|fbo| fbo != 0 && ctx.local.framebuffers.color_attachment_count(fbo) > 1);
        // A clear the geometry builder paints with a draw belongs there too: this single-target path can
        // only express a whole-attachment load op, which is the thing those clears are not.
        if mrt
            || ctx.local.recording.draws[start..].iter().any(|draw| {
                (draw.clears_color() && draw.scissor_enabled) || draw.needs_rect_clear()
            })
        {
            if let Some(frame) = Self::build_geometry(ctx) {
                return Some(frame);
            }
        }
        // No recorded clear writes the color plane — a depth- or stencil-only `glClear`, or one masked off
        // by `glColorMask`. The colour target must NOT be cleared here (that was the bug where
        // `glClear(GL_DEPTH_BUFFER_BIT)` repainted the colour buffer), but returning no frame at all dropped
        // the DEPTH clear just as silently: the plane was never materialized, so the next frame to enable
        // the depth test minted it fresh at the GL initial 1.0 and every `GL_LESS` fragment passed. Build
        // the pass with the colour attachment LOADing and the depth attachment carrying the clear.
        let depth = depth_load(&ctx.local.recording.draws);
        if !ctx.local.recording.draws.iter().any(DrawCall::clears_color) {
            if !matches!(depth.load, LoadOp::Clear) {
                return None;
            }
            let cmds: Vec<Cmd> = Vec::new();
            let fbo = ctx.local.recording.draws.last().map(|d| d.fbo).unwrap_or(0);
            return Some(build_clear_frame_depth(ctx, fbo, depth, cmds));
        }
        let cmds: Vec<Cmd> = Vec::new();
        let fbo = ctx.local.recording.draws.last().map(|d| d.fbo).unwrap_or(0);
        let clear = RenderPasses::effective_clear(&ctx.local.recording.draws).0;
        Some(build_clear_frame_color(ctx, fbo, clear, cmds))
    }
}

// Geometry frame assembly continues in `geometry`.
