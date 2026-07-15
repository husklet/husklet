//! Frame assembly — lower the recorded draw-list into the frame's `Cmd` stream at `eglSwapBuffers`.
//!
//! Ported (simplified to the core paths) from `hl-shim-gl/src/frame.rs` (`build_frame_ir`) +
//! `hl-shim-gl/src/lower.rs`. Unlike the C/shim path — which encoded straight to wire bytes — this
//! produces a `Vec<hl_gpu::Cmd>` so [`crate::service::swap`] can submit it through a
//! [`hl_gpu::CommandSink`] (the tested seam), exactly as cuda's services submit `Cmd`s.
//!
//! Frame shapes FULLY lowered:
//! * **clear-only** — a frame whose draw-list is all `glClear`s → a render pass that clears the target
//!   (mirrors gl_shim.c's `ClearRect`-only submit).
//! * **single-draw** — one geometry draw → the VBO/index/texture/uniform uploads + the translated shader
//!   + pipeline + bind group + the render pass.
//! * **multi-draw / clear-then-draw** — any mix of a leading `glClear` and one-or-more geometry draws →
//!   a single render pass that clears once (`LoadOp::Clear`) then replays every geometry draw's
//!   `SetPipeline`/bindings/`Draw` in order into that pass. Each draw's texture staging copies are hoisted
//!   ahead of `BeginRenderPass` (copies are illegal inside a render pass).
//! * **offscreen FBO** — geometry recorded while a non-default framebuffer is bound renders into a
//!   `CreateTexture(RENDER_TARGET)` for that FBO's color attachment (sized + formatted from the attached
//!   GL texture) instead of the default window surface; that render-target texture is what the frame
//!   presents.
//!
//! Still deferred: residency-delta upload skipping (re-uploading every bound buffer/texture each frame —
//! the `hl-shim-gl` `build_replay_frame` dirty-tracking path) and interleaved clear-between-draws within a
//! single frame (a leading clear is honored; a `glClear` recorded *after* a draw is folded into the one
//! pass-clear rather than re-clearing mid-pass).

use crate::model::context::GlContext;
use crate::model::glconst::*;
use crate::model::program::DrawCall;
use hl_gpu::protocol::model::command::Enc;
use hl_gpu::protocol::model::descriptor::{
    BindEntry, BindGroupDesc, BindResource, BlendState, BufferDesc, ColorAttachment, ColorTargetState,
    DepthState, RenderPipelineDesc, SamplerDesc, ShaderRef, SurfaceDesc, TextureDesc, VertexAttr,
    VertexLayout,
};
use hl_gpu::protocol::model::enums::{
    buffer_usage, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat, Topology,
};
use hl_gpu::{Cmd, CommandBuffer, ShaderPayloadKind};
use hl_log::{hl_add, hl_count, hl_debug, tag};

/// The assembled frame: the `Cmd` stream to submit, plus the `(surface, texture)` to `Present` at the
/// end. Returned by [`build_frame_ir`] for [`crate::service::swap`] to ship.
pub struct Frame {
    /// The resource + submit commands, in submission order (no `Present` — swap appends that).
    pub cmds: Vec<Cmd>,
    /// The default-surface + its render-target texture IR ids to `Present`.
    pub present: (u32, u32),
    /// The presented render target's pixel dimensions + texel format — what a `glReadPixels` readback of
    /// this frame ([`crate::service::readpixels`]) copies out of the render-target texture.
    pub target_width: i32,
    pub target_height: i32,
    pub target_format: TextureFormat,
}

/// Total upload bytes a lowered frame carries: the sum of every `WriteBuffer` payload (vertex / index /
/// uniform data + hoisted texture-staging copies — the CreateTexture pixels ride a staging `WriteBuffer`).
/// This is the single most valuable frame instrument: the 4.3 GiB GTK frame is exactly this number.
fn frame_upload_bytes(cmds: &[Cmd]) -> u64 {
    cmds.iter()
        .map(|c| match c {
            Cmd::WriteBuffer { data, .. } => data.len() as u64,
            _ => 0,
        })
        .sum()
}

/// Emit the per-frame observability: one scannable `key=val` debug line + the frame counters. Gated +
/// zero-cost when `tag::GL` logging / counters are off.
fn log_frame(w: i32, h: i32, draws: usize, passes: usize, cmds: usize, bytes: u64) {
    hl_debug!(tag::GL, "frame {}x{} draws={} passes={} cmds={} bytes={}", w, h, draws, passes, cmds, bytes);
    hl_add!(tag::GL, "frame_bytes", bytes);
    hl_add!(tag::GL, "frame_cmds", cmds as u64);
    hl_count!(tag::GL, "frames");
}

/// Assemble the frame's `Cmd` stream from the recorded draw-list, or `None` if there is nothing (or
/// nothing yet supported) to present. Mints the IR ids it needs from `ctx`.
pub fn build_frame_ir(ctx: &mut GlContext) -> Option<Frame> {
    if !ctx.surf.have || ctx.draws.is_empty() {
        return None;
    }
    // Partition the recorded draw-list into contiguous runs that share a bound framebuffer. A frame that
    // renders to more than one framebuffer (the GskGL / offscreen-compositor shape: a glyph atlas + offscreen
    // render targets, then a final default-framebuffer pass that SAMPLES them) lowers as a SEQUENCE of render
    // passes, one per run — not collapsed onto the first draw's FBO. A single run is the single-target fast
    // path (byte-identical to the pre-frame-graph lowering).
    let groups = fbo_groups(&ctx.draws);
    if groups.len() > 1 {
        return build_multi_pass_frame(ctx, &groups);
    }
    if ctx.draws.iter().all(|d| d.is_clear) {
        return Some(build_clear_frame(ctx));
    }
    // One framebuffer, one or more geometry draws (optionally led by a clear) → the single/multi-draw path.
    build_geometry_frame(ctx)
}

/// Partition the draw-list into maximal contiguous runs that share a bound framebuffer, in record order,
/// as `(fbo, start, end)` half-open index ranges into `draws`. Each run becomes one render pass targeting
/// that FBO's color attachment (fbo `0` = the default window framebuffer). A clear carries the FBO bound
/// when it was recorded, so a `glClear` under an offscreen FBO groups with that FBO's geometry.
fn fbo_groups(draws: &[DrawCall]) -> Vec<(u32, usize, usize)> {
    let mut groups: Vec<(u32, usize, usize)> = Vec::new();
    for (i, d) in draws.iter().enumerate() {
        match groups.last_mut() {
            Some((fbo, _, end)) if *fbo == d.fbo => *end = i + 1,
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
fn build_multi_pass_frame(ctx: &mut GlContext, groups: &[(u32, usize, usize)]) -> Option<Frame> {
    let draws = ctx.draws.clone();
    let mut cmds: Vec<Cmd> = Vec::new();
    // GL texture name of an FBO color attachment → the render-target texture IR a prior pass rendered into,
    // so a later pass sampling that attachment reads the rendered pixels rather than re-uploading its CPU
    // storage (an FBO attachment allocated via glTexImage2D(…, NULL) carries a zeroed plane, not the render).
    let mut fbo_tex_ir: std::collections::HashMap<u32, u32> = std::collections::HashMap::new();
    // The default-framebuffer (window) target to present + read back; the last run's target is the fallback.
    let mut present: Option<(u32, u32, i32, i32, TextureFormat)> = None;
    let mut last: Option<(u32, u32, i32, i32, TextureFormat)> = None;

    for &(fbo, start, end) in groups {
        let run = &draws[start..end];
        let (surface, target_tex, tw, th, fmt) = resolve_target(ctx, fbo, &mut cmds);
        // Register this run's offscreen attachment so a later run can sample its rendered pixels. Mirror
        // resolve_target's offscreen condition (a sized attachment) so `target_tex` is the offscreen target.
        if fbo != 0 {
            let attach = ctx.framebuffers.color_attachment(fbo);
            if attach != 0 && ctx.textures.get(attach).map(|t| t.w > 0 && t.h > 0).unwrap_or(false) {
                fbo_tex_ir.insert(attach, target_tex);
            }
        }
        // The pass clear color is the run's first recorded draw (a leading glClear if present, else the
        // first geometry draw's snapshot). Each run clear-loads its target once, then replays its draws.
        let clear = run.first().map(|d| d.clear).unwrap_or([0.0; 4]);

        let mut copies: Vec<Enc> = Vec::new();
        let mut draw_ops: Vec<Enc> = Vec::new();
        for d in run.iter().filter(|d| !d.is_clear) {
            if let Some(l) = lower_draw(ctx, d, fmt, tw, th, &mut cmds, &fbo_tex_ir) {
                copies.extend(l.copies);
                draw_ops.extend(l.ops);
            }
        }

        let mut ops: Vec<Enc> = copies;
        ops.push(Enc::BeginRenderPass {
            color: vec![ColorAttachment { texture: target_tex, load: LoadOp::Clear, clear, store: true }],
            depth: None,
        });
        ops.extend(draw_ops);
        ops.push(Enc::EndRenderPass);
        cmds.push(Cmd::Submit(CommandBuffer { encoder: ops, signal: None }));

        last = Some((surface, target_tex, tw, th, fmt));
        if fbo == 0 {
            present = Some((surface, target_tex, tw, th, fmt));
        }
    }

    let (surface, texture, tw, th, fmt) = present.or(last)?;
    log_frame(tw, th, draws.len(), groups.len(), cmds.len(), frame_upload_bytes(&cmds));
    Some(Frame {
        cmds,
        present: (surface, texture),
        target_width: tw,
        target_height: th,
        target_format: fmt,
    })
}

/// The render target + presentable surface for a frame whose draws target framebuffer `fbo`. Mints the
/// target's `CreateTexture` + `CreateSurface` (once, cached in the context) and pushes them into `cmds`.
/// Returns `(surface_ir, texture_ir, width, height, format)`.
///
/// * `fbo == 0` (or an FBO with no usable color attachment) → the default window target: `Bgra8Unorm`,
///   sized to the window surface.
/// * a non-default `fbo` with a sized color-attachment texture → an offscreen render target sized to and
///   formatted as that attachment (the "render to a texture instead of the default surface" path).
fn resolve_target(ctx: &mut GlContext, fbo: u32, cmds: &mut Vec<Cmd>) -> (u32, u32, i32, i32, TextureFormat) {
    // Try the FBO's color attachment; fall back to the default target if it is missing/unsized.
    if fbo != 0 {
        let attach = ctx.framebuffers.color_attachment(fbo);
        if attach != 0 {
            if let Some((w, h, fmt)) = ctx.textures.get(attach).filter(|t| t.w > 0 && t.h > 0).map(|t| (t.w, t.h, t.ir_format)) {
                let (surface, texture, needs_create) = ctx.fbo_target(attach);
                if needs_create {
                    // Offscreen targets add SAMPLED: a later default-framebuffer pass samples them (the
                    // atlas/offscreen → window composite), which the CPU oracle's bind-group check requires.
                    push_target_creates(cmds, surface, texture, w, h, fmt, "offscreen-fbo", true);
                }
                return (surface, texture, w, h, fmt);
            }
        }
    }
    let (w, h) = ctx.target_wh();
    let fmt = TextureFormat::Bgra8Unorm;
    let (surface, texture, needs_create) = ctx.default_target();
    if needs_create {
        push_target_creates(cmds, surface, texture, w, h, fmt, "default-fbo", false);
    }
    (surface, texture, w, h, fmt)
}

/// Emit the `CreateTexture(RENDER_TARGET | PRESENT)` + matching `CreateSurface` for a render target. When
/// `sampled` a `SAMPLED` usage bit is added so a later render pass may bind this target as a texture (an
/// offscreen FBO sampled by the default-framebuffer composite); the default window target is never sampled.
fn push_target_creates(cmds: &mut Vec<Cmd>, surface: u32, texture: u32, w: i32, h: i32, fmt: TextureFormat, label: &str, sampled: bool) {
    let (w, h) = (w.max(1) as u32, h.max(1) as u32);
    let sampled = if sampled { texture_usage::SAMPLED } else { 0 };
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
            usage: texture_usage::RENDER_TARGET | texture_usage::PRESENT | texture_usage::COPY_SRC | sampled,
            label: label.into(),
        },
    ));
    cmds.push(Cmd::CreateSurface(
        surface,
        SurfaceDesc { width: w, height: h, format: fmt, hlp_surface: 0 },
    ));
}

/// Clear-only frame: a render pass over the target that clears it (`LoadOp::Clear`).
fn build_clear_frame(ctx: &mut GlContext) -> Frame {
    let mut cmds: Vec<Cmd> = Vec::new();
    let fbo = ctx.draws.last().map(|d| d.fbo).unwrap_or(0);
    let (surface, texture, w, h, fmt) = resolve_target(ctx, fbo, &mut cmds);
    let clear = ctx.draws.last().map(|d| d.clear).unwrap_or([0.0; 4]);
    let ops = vec![
        Enc::BeginRenderPass {
            color: vec![ColorAttachment { texture, load: LoadOp::Clear, clear, store: true }],
            depth: None,
        },
        Enc::EndRenderPass,
    ];
    cmds.push(Cmd::Submit(CommandBuffer { encoder: ops, signal: None }));
    log_frame(w, h, ctx.draws.len(), 1, cmds.len(), frame_upload_bytes(&cmds));
    Frame { cmds, present: (surface, texture), target_width: w, target_height: h, target_format: fmt }
}

/// The per-draw lowering result: the texture staging copies (hoisted before `BeginRenderPass`) and the
/// in-pass encoder ops (`SetPipeline` … `Draw`) for one geometry draw.
struct DrawLowering {
    copies: Vec<Enc>,
    ops: Vec<Enc>,
}

/// Geometry frame: clear once, then replay every geometry draw into a single render pass over the target.
/// Handles single-draw, multi-draw, and clear-then-draw, against the default surface or an offscreen FBO.
fn build_geometry_frame(ctx: &mut GlContext) -> Option<Frame> {
    let geom: Vec<DrawCall> = ctx.draws.iter().filter(|d| !d.is_clear).cloned().collect();
    if geom.is_empty() {
        return None;
    }
    // The render target follows the first geometry draw's framebuffer binding; the clear color is the
    // frame's first recorded draw (a leading glClear if present, else the first draw's snapshot).
    let fbo = geom[0].fbo;
    let clear = ctx.draws.first().map(|d| d.clear).unwrap_or([0.0; 4]);

    let mut cmds: Vec<Cmd> = Vec::new();
    let (surface, target_tex, tw, th, target_fmt) = resolve_target(ctx, fbo, &mut cmds);

    // Single-target frame: no prior offscreen pass to sample, so cross-pass FBO sampling is empty and this
    // path lowers byte-identically to the pre-frame-graph builder.
    let no_fbo_tex = std::collections::HashMap::new();
    let mut copies: Vec<Enc> = Vec::new();
    let mut draw_ops: Vec<Enc> = Vec::new();
    for d in &geom {
        if let Some(lowered) = lower_draw(ctx, d, target_fmt, tw, th, &mut cmds, &no_fbo_tex) {
            copies.extend(lowered.copies);
            draw_ops.extend(lowered.ops);
        }
    }
    // Not one geometry draw could be lowered (e.g. every program was unlinked) → present nothing.
    if draw_ops.is_empty() {
        return None;
    }

    let mut ops: Vec<Enc> = copies;
    ops.push(Enc::BeginRenderPass {
        color: vec![ColorAttachment { texture: target_tex, load: LoadOp::Clear, clear, store: true }],
        depth: None,
    });
    ops.extend(draw_ops);
    ops.push(Enc::EndRenderPass);

    cmds.push(Cmd::Submit(CommandBuffer { encoder: ops, signal: None }));
    log_frame(tw, th, ctx.draws.len(), 1, cmds.len(), frame_upload_bytes(&cmds));
    Some(Frame {
        cmds,
        present: (surface, target_tex),
        target_width: tw,
        target_height: th,
        target_format: target_fmt,
    })
}

/// Lower one geometry draw against a render target of format `target_fmt`: emit its resource creates +
/// uploads into `cmds` and return the staging copies + in-pass encoder ops. `None` if the draw's program
/// is unknown/unlinked (the caller skips it). The byte-shape mirrors gl_shim.c's per-draw lowering.
fn lower_draw(ctx: &mut GlContext, d: &DrawCall, target_fmt: TextureFormat, tw: i32, th: i32, cmds: &mut Vec<Cmd>, fbo_tex_ir: &std::collections::HashMap<u32, u32>) -> Option<DrawLowering> {
    let d = d.clone();
    let prog_name = if d.prog != 0 { d.prog } else { ctx.cur_prog };
    let prog = ctx.programs.program(prog_name)?.clone();
    let vs_ir = prog.vs_ir.clone()?;
    let fs_ir = prog.fs_ir.clone()?;
    let vdecl = crate::adapter::glsl::collect_vertex_attrs(&prog.vs_src);
    let ndecl = vdecl.len();

    // ---- vertex-buffer slot analysis (dedup bound buffers into slots) ----
    let mut slot_gl_buf: Vec<u32> = Vec::new();
    let mut attr_slot = [-1i32; crate::model::program::MAX_ATTR];
    for (i, a) in d.attrs.iter().enumerate() {
        if !a.enabled || a.buffer == 0 || !ctx.buffers.has_data(a.buffer) {
            continue;
        }
        let sl = slot_gl_buf.iter().position(|&x| x == a.buffer).unwrap_or_else(|| {
            slot_gl_buf.push(a.buffer);
            slot_gl_buf.len() - 1
        });
        attr_slot[i] = sl as i32;
    }
    let nslot = slot_gl_buf.len();
    let mut slot_stride = vec![0u32; nslot.max(1)];
    for (i, a) in d.attrs.iter().enumerate() {
        let sl = attr_slot[i];
        if sl < 0 {
            continue;
        }
        let mut st = a.stride as u32;
        if st == 0 {
            st = a.size as u32 * 4;
        }
        if st > slot_stride[sl as usize] {
            slot_stride[sl as usize] = st;
        }
    }
    for st in slot_stride.iter_mut() {
        if *st == 0 {
            *st = 16;
        }
    }
    let nvd = d.attrs.iter().enumerate().filter(|(_, a)| a.enabled).map(|(i, _)| i + 1).max().unwrap_or(0);

    // Resolve the IR buffer id for each vertex slot, uploading its bytes ONLY on first sight / content
    // change (the residency cache): a VBO bound across many draws or frames is created + written once.
    let mut slot_ir: Vec<u32> = Vec::with_capacity(nslot);
    for &gl_buf in &slot_gl_buf {
        let gen = ctx.buffers.get(gl_buf).map(|b| b.gen).unwrap_or(0);
        let (ir, needs_upload) = ctx.data_buffer_ir(gl_buf, buffer_usage::VERTEX, gen);
        slot_ir.push(ir);
        if needs_upload {
            let data = ctx.buffers.get(gl_buf).map(|b| b.data.clone()).unwrap_or_default();
            cmds.push(Cmd::CreateBuffer(ir, BufferDesc { size: data.len() as u64, usage: buffer_usage::VERTEX, label: String::new() }));
            cmds.push(Cmd::WriteBuffer { id: ir, offset: 0, data });
        }
    }

    // ---- client-side vertex arrays (no VBO bound) → transient per-draw VERTEX buffers ----
    // Each captured client array (recorded at draw time from a `glVertexAttribPointer` client pointer)
    // becomes its own tightly-packed buffer + a one-attribute vertex-layout slot appended AFTER the VBO
    // slots. De-interleaving into per-attribute buffers maps 1:1 onto the vertex-layout IR and handles
    // interleaved and separate client arrays uniformly. EMPTY for a bound-VBO draw → that path is unchanged.
    struct ClientSlot {
        ir: u32,
        stride: u32,
        step_mode: u32,
        location: u32,
        format: u32,
    }
    let mut client_slots: Vec<ClientSlot> = Vec::with_capacity(d.client_vbufs.len());
    for ca in &d.client_vbufs {
        let ir = ctx.alloc_buffer_ir();
        cmds.push(Cmd::CreateBuffer(ir, BufferDesc { size: ca.data.len() as u64, usage: buffer_usage::VERTEX, label: String::new() }));
        cmds.push(Cmd::WriteBuffer { id: ir, offset: 0, data: ca.data.clone() });
        let elem = ca.size.clamp(1, 4) as u32 * gl_component_size(ca.kind) as u32;
        client_slots.push(ClientSlot {
            ir,
            stride: elem.max(1),
            step_mode: (ca.divisor > 0) as u32,
            location: ca.location as u32,
            format: vertex_format_wire(ca.kind, ca.size, ca.normalized, ca.integer),
        });
    }

    // Index buffer: a bound element-array-buffer, else the captured client-side index array (transient).
    let mut index_ir = 0u32;
    if d.indexed && d.elem_buf != 0 && ctx.buffers.has_data(d.elem_buf) {
        let gen = ctx.buffers.get(d.elem_buf).map(|b| b.gen).unwrap_or(0);
        let (ir, needs_upload) = ctx.data_buffer_ir(d.elem_buf, buffer_usage::INDEX, gen);
        index_ir = ir;
        if needs_upload {
            let data = ctx.buffers.get(d.elem_buf).map(|b| b.data.clone()).unwrap_or_default();
            cmds.push(Cmd::CreateBuffer(index_ir, BufferDesc { size: data.len() as u64, usage: buffer_usage::INDEX, label: String::new() }));
            cmds.push(Cmd::WriteBuffer { id: index_ir, offset: 0, data });
        }
    } else if d.indexed && !d.client_indices.is_empty() {
        index_ir = ctx.alloc_buffer_ir();
        let data = d.client_indices.clone();
        cmds.push(Cmd::CreateBuffer(index_ir, BufferDesc { size: data.len() as u64, usage: buffer_usage::INDEX, label: String::new() }));
        cmds.push(Cmd::WriteBuffer { id: index_ir, offset: 0, data });
    }

    // ---- sampler-bound textures ----
    struct TexBind {
        /// The sampler's DECLARATION index (its `k` in the translator's `layout(binding=)` scheme). Keeps
        /// the IR binding aligned to the emitted GLSL even when an earlier sampler had no bound texture.
        slot: usize,
        tex_ir: u32,
        samp_ir: u32,
        stage_ir: u32,
        w: u32,
        h: u32,
    }
    let mut texbinds: Vec<TexBind> = Vec::new();
    for i in 0..prog.samp_names.len().min(4) {
        let unit = if (0..8).contains(&prog.samp_units[i]) { prog.samp_units[i] as usize } else { i };
        let gl_tex = d.tex_units[unit];
        // Cross-pass FBO sampling: if this sampled GL texture is the color attachment an earlier render pass
        // rendered into, bind THAT render-target texture (the rendered pixels) directly — no staging upload
        // (its CPU plane is the pre-render zero storage). `stage_ir == 0` marks the copy-free bind.
        if let Some(&rt_ir) = fbo_tex_ir.get(&gl_tex) {
            let t = match ctx.textures.get(gl_tex) {
                Some(t) => t.clone(),
                None => continue,
            };
            let samp_ir = ctx.alloc_sampler_ir();
            cmds.push(Cmd::CreateSampler(
                samp_ir,
                SamplerDesc {
                    min_filter: t.ir_min_filter(),
                    mag_filter: t.ir_mag_filter(),
                    mip_filter: Filter::Nearest,
                    address_u: t.ir_wrap_s(),
                    address_v: t.ir_wrap_t(),
                    address_w: AddressMode::ClampToEdge,
                },
            ));
            texbinds.push(TexBind { slot: i, tex_ir: rt_ir, samp_ir, stage_ir: 0, w: t.w as u32, h: t.h as u32 });
            continue;
        }
        let t = match ctx.textures.get(gl_tex) {
            Some(t) if t.has_data() => t.clone(),
            _ => continue,
        };
        // Residency cache: a sampled texture (a GskGL glyph/mask atlas is bound across hundreds of draws
        // and re-used every frame) is `CreateTexture`d + staged + copied ONLY on first sight / content
        // change; later references reuse the resident IR id and upload nothing (`stage_ir == 0` marks the
        // copy-free bind, the same convention the cross-pass FBO sample uses).
        let (tex_ir, needs_upload) = ctx.sampled_texture_ir(gl_tex, t.gen);
        let samp_ir = ctx.alloc_sampler_ir();
        let stage_ir = if needs_upload {
            let stage_ir = ctx.alloc_buffer_ir();
            cmds.push(Cmd::CreateTexture(
                tex_ir,
                TextureDesc {
                    width: t.w as u32,
                    height: t.h as u32,
                    depth: 1,
                    mip_levels: 1,
                    sample_count: 1,
                    dim: TextureDim::D2,
                    format: t.ir_format,
                    usage: texture_usage::SAMPLED | texture_usage::COPY_DST,
                    label: String::new(),
                },
            ));
            cmds.push(Cmd::CreateBuffer(stage_ir, BufferDesc { size: t.data.len() as u64, usage: buffer_usage::COPY_SRC, label: String::new() }));
            cmds.push(Cmd::WriteBuffer { id: stage_ir, offset: 0, data: t.data.clone() });
            stage_ir
        } else {
            0
        };
        cmds.push(Cmd::CreateSampler(
            samp_ir,
            SamplerDesc {
                min_filter: t.ir_min_filter(),
                mag_filter: t.ir_mag_filter(),
                mip_filter: Filter::Nearest,
                address_u: t.ir_wrap_s(),
                address_v: t.ir_wrap_t(),
                address_w: AddressMode::ClampToEdge,
            },
        ));
        texbinds.push(TexBind { slot: i, tex_ir, samp_ir, stage_ir, w: t.w as u32, h: t.h as u32 });
    }
    let has_u = prog.has_uniforms();
    let has_bg = has_u || !texbinds.is_empty();

    // ---- shaders + pipeline ----
    // The vertex and fragment GLSL are forwarded as two separate `Glsl` shader modules (each carries one
    // stage's source led by GLSL_MAGIC); the render pipeline binds them by their `vmain`/`fmain` entries.
    // naga's `glsl-in` compiles one stage per module, so the two stages are distinct modules (not one
    // combined module as the old pre-translated-MSL path used).
    let vs_id = ctx.alloc_shader_ir();
    let fs_id = ctx.alloc_shader_ir();
    cmds.push(Cmd::CreateShader { id: vs_id, kind: ShaderPayloadKind::Glsl, spirv: vs_ir });
    cmds.push(Cmd::CreateShader { id: fs_id, kind: ShaderPayloadKind::Glsl, spirv: fs_ir });

    // Locations fed by an appended client slot (see below) are NOT folded into a VBO slot's layout.
    let mut client_loc = [false; crate::model::program::MAX_ATTR];
    for ca in &d.client_vbufs {
        if ca.location < crate::model::program::MAX_ATTR {
            client_loc[ca.location] = true;
        }
    }
    // With client slots present, DON'T mint the phantom slot-0 (the `nslot == 0` fallback): the client
    // slots ARE the vertex buffers. Without client slots this stays `nslot.max(1)` — byte-identical VBO path.
    let nvb = if client_slots.is_empty() { nslot.max(1) } else { nslot };
    let mut vbs: Vec<VertexLayout> = Vec::with_capacity(nvb + client_slots.len());
    for sl in 0..nvb {
        let mut attrs = Vec::new();
        for l in 0..nvd {
            if l < crate::model::program::MAX_ATTR && client_loc[l] {
                continue; // fed by an appended client slot, not this VBO slot
            }
            let ls = if l < crate::model::program::MAX_ATTR && attr_slot[l] >= 0 { attr_slot[l] } else { 0 };
            if ls as usize != sl {
                continue;
            }
            let (fmt, off) = if l < crate::model::program::MAX_ATTR && d.attrs[l].enabled && attr_slot[l] >= 0 {
                let a = &d.attrs[l];
                (vertex_format_wire(a.kind, a.size, a.normalized, a.integer), a.offset as u32)
            } else {
                let t = if l < ndecl { vdecl[l].ty.as_str() } else { "vec4" };
                (decl_format_wire(t), 0)
            };
            attrs.push(VertexAttr { location: l as u32, format: fmt, offset: off });
        }
        let stride = if sl < nslot { slot_stride[sl] } else { 16 };
        // A vertex-buffer slot steps per-instance (step_mode 1) when any attribute it feeds carries a
        // non-zero `glVertexAttribDivisor`. This model has one step rate per slot, so a divisor `N>1`
        // (fractional instancing rate) collapses to per-instance stepping — an honest partial lowering.
        let step_mode = (0..crate::model::program::MAX_ATTR)
            .any(|l| attr_slot[l] == sl as i32 && d.attrs[l].enabled && d.attrs[l].divisor > 0)
            as u32;
        vbs.push(VertexLayout { stride, step_mode, attrs });
    }
    // Append one layout per client-side array — a single attribute at offset 0, tightly-packed stride.
    for cs in &client_slots {
        vbs.push(VertexLayout {
            stride: cs.stride,
            step_mode: cs.step_mode,
            attrs: vec![VertexAttr { location: cs.location, format: cs.format, offset: 0 }],
        });
    }
    // Fixed-function state → the pipeline's blend / depth / cull descriptor (the values a real app set via
    // glBlendFunc / glDepthFunc / glCullFace / glFrontFace, mapped to their opaque WebGPU wire enums).
    let blend = if d.blend {
        Some(BlendState {
            src_color: blend_factor_wire(d.blend_src_rgb),
            dst_color: blend_factor_wire(d.blend_dst_rgb),
            op_color: blend_op_wire(d.blend_eq_rgb),
            src_alpha: blend_factor_wire(d.blend_src_alpha),
            dst_alpha: blend_factor_wire(d.blend_dst_alpha),
            op_alpha: blend_op_wire(d.blend_eq_alpha),
        })
    } else {
        None
    };
    // Depth test → a pipeline depth state carrying the compare func + write mask. NOTE: no depth
    // ATTACHMENT is emitted (this model has no depth buffer), so the state is recorded in the pipeline but
    // is not observable on the CPU oracle — an honest partial lowering, asserted at the Cmd level.
    let depth = if d.depth {
        Some(DepthState {
            format: TextureFormat::Depth32Float,
            depth_write: d.depth_write,
            depth_compare: compare_wire(d.depth_func),
        })
    } else {
        None
    };
    let topology = if d.mode == GL_TRIANGLE_STRIP { Topology::TriangleStrip } else { Topology::TriangleList };
    let pipeline_ir = ctx.alloc_pipeline_ir();
    cmds.push(Cmd::CreateRenderPipeline(
        pipeline_ir,
        RenderPipelineDesc {
            vertex: ShaderRef { module: vs_id, entry: "vmain".into() },
            fragment: Some(ShaderRef { module: fs_id, entry: "fmain".into() }),
            vertex_buffers: vbs,
            color_targets: vec![ColorTargetState { format: target_fmt, blend, write_mask: 0xf }],
            depth,
            topology,
            cull: if d.cull_enabled { cull_wire(d.cull_face) } else { 0 },
            front_face: front_face_wire(d.front_face),
            label: String::new(),
        },
    ));

    // ---- uniform buffer + bind group ----
    let mut uniform_ir = 0u32;
    if has_u {
        uniform_ir = ctx.alloc_buffer_ir();
        let ubuf = prog.ubuf[..prog.ubuf_size.max(0) as usize].to_vec();
        cmds.push(Cmd::CreateBuffer(uniform_ir, BufferDesc { size: ubuf.len() as u64, usage: buffer_usage::UNIFORM, label: String::new() }));
        cmds.push(Cmd::WriteBuffer { id: uniform_ir, offset: 0, data: ubuf });
    }
    let mut bind_group_ir = 0u32;
    if has_bg {
        bind_group_ir = ctx.alloc_bind_group_ir();
        // Binding scheme (single wgpu bind-group namespace, matching `adapter::glsl`'s emitted
        // `layout(binding=)` — naga derives the pipeline's bind-group layout from that GLSL, so these
        // MUST agree): the uniform block owns binding 0; sampler `k` (declaration index) owns TEXTURE
        // binding `1 + 2k` and SAMPLER binding `2 + 2k`. Every resource lands on a DISTINCT binding, so a
        // program with a UBO AND 2+ samplers no longer aliases the UBO onto a sampler (the old bug: UBO at
        // 1 collided with the 2nd sampler, also at 1).
        let mut entries = Vec::new();
        if has_u {
            entries.push(BindEntry { binding: 0, resource: BindResource::Buffer { id: uniform_ir, offset: 0, size: prog.ubuf_size as u64 } });
        }
        for tb in texbinds.iter() {
            let tex_binding = 1 + 2 * tb.slot as u32;
            let smp_binding = 2 + 2 * tb.slot as u32;
            entries.push(BindEntry { binding: tex_binding, resource: BindResource::Texture { id: tb.tex_ir } });
            entries.push(BindEntry { binding: smp_binding, resource: BindResource::Sampler { id: tb.samp_ir } });
        }
        cmds.push(Cmd::CreateBindGroup(bind_group_ir, BindGroupDesc { set: 0, entries }));
    }

    // ---- staging copies (hoisted before BeginRenderPass) + the in-pass draw ops ----
    let mut copies: Vec<Enc> = Vec::new();
    for tb in &texbinds {
        // A cross-pass FBO sample (stage_ir == 0) was rendered by a prior pass — no upload/copy to hoist.
        if tb.stage_ir == 0 {
            continue;
        }
        copies.push(Enc::CopyBufferToTexture {
            src: tb.stage_ir,
            src_offset: 0,
            bytes_per_row: tb.w * 4,
            dst: tb.tex_ir,
            mip: 0,
            width: tb.w,
            height: tb.h,
        });
    }
    let mut ops: Vec<Enc> = Vec::new();
    ops.push(Enc::SetPipeline(pipeline_ir));
    ops.push(emit_viewport(&d, tw, th));
    ops.push(emit_scissor(&d, tw, th));
    if has_bg {
        ops.push(Enc::SetBindGroup { index: 0, group: bind_group_ir });
    }
    for (sl, &ir) in slot_ir.iter().enumerate() {
        ops.push(Enc::SetVertexBuffer { slot: sl as u32, buffer: ir, offset: 0 });
    }
    // Client-side transient buffers bind to the slots appended after the VBO slots.
    for (i, cs) in client_slots.iter().enumerate() {
        ops.push(Enc::SetVertexBuffer { slot: (nslot + i) as u32, buffer: cs.ir, offset: 0 });
    }
    if d.indexed && index_ir != 0 {
        let ifmt = if d.index_type == GL_UNSIGNED_INT {
            hl_gpu::protocol::model::enums::IndexFormat::U32
        } else {
            hl_gpu::protocol::model::enums::IndexFormat::U16
        };
        // A bound element buffer indexes at `index_offset`; a captured client index array is transient
        // (its own buffer from byte 0), so it binds at offset 0.
        let ioff = if d.elem_buf != 0 { d.index_offset as u64 } else { 0 };
        ops.push(Enc::SetIndexBuffer { buffer: index_ir, offset: ioff, format: ifmt });
        ops.push(Enc::DrawIndexed { index_count: d.count as u32, instance_count: d.instance_count, first_index: 0, base_vertex: d.base_vertex, first_instance: d.first_instance });
    } else {
        ops.push(Enc::Draw { vertex_count: d.count as u32, instance_count: d.instance_count, first_vertex: d.first as u32, first_instance: d.first_instance });
    }

    Some(DrawLowering { copies, ops })
}

/// `SetViewport` with the GL→Metal Y-flip (`gl_shim.c` `emit_viewport_h`), against a `tw`×`th` target.
fn emit_viewport(d: &DrawCall, tw: i32, th: i32) -> Enc {
    let (mut x, mut y, mut w, mut h) = (0.0f32, 0.0f32, tw as f32, th as f32);
    if d.viewport[2] > 0 && d.viewport[3] > 0 {
        x = d.viewport[0] as f32;
        w = d.viewport[2] as f32;
        h = d.viewport[3] as f32;
        y = (th - d.viewport[1] - d.viewport[3]) as f32;
    }
    Enc::SetViewport { x, y, w, h, min_depth: 0.0, max_depth: 1.0 }
}

/// `SetScissor` with the Y-flip + clamp (`gl_shim.c` `emit_scissor_h`), against a `tw`×`th` target.
fn emit_scissor(d: &DrawCall, tw: i32, th: i32) -> Enc {
    let (mut x, mut y, mut w, mut h) = (0, 0, tw, th);
    if d.scissor_enabled && d.scissor[2] > 0 && d.scissor[3] > 0 {
        x = d.scissor[0];
        y = th - d.scissor[1] - d.scissor[3];
        w = d.scissor[2];
        h = d.scissor[3];
    }
    x = x.clamp(0, tw);
    y = y.clamp(0, th);
    if x + w > tw {
        w = tw - x;
    }
    if y + h > th {
        h = th - y;
    }
    Enc::SetScissor { x: x as u32, y: y as u32, w: w.max(0) as u32, h: h.max(0) as u32 }
}

/// Vertex-attribute format packing (`gl_shim.c` `vertex_format_wire`):
/// `comps | (kind<<8) | (normalized<<16) | (integer<<17)`, comps clamped to [1,4].
fn vertex_format_wire(kind_enum: u32, comps: i32, normalized: bool, integer: bool) -> u32 {
    let comps = comps.clamp(1, 4) as u32;
    let kind = match kind_enum {
        GL_UNSIGNED_BYTE => 1,
        GL_BYTE => 2,
        GL_UNSIGNED_SHORT => 3,
        GL_SHORT => 4,
        GL_UNSIGNED_INT => 5,
        GL_INT => 6,
        GL_HALF_FLOAT => 7,
        _ => 0, // GL_FLOAT and unknown
    };
    comps | (kind << 8) | ((normalized as u32) << 16) | ((integer as u32) << 17)
}

/// GL blend factor enum → opaque WebGPU blend-factor wire value (`gl_shim.c` `blend_factor_wire`).
fn blend_factor_wire(f: u32) -> u32 {
    match f {
        GL_ZERO => 0,
        GL_ONE => 1,
        GL_SRC_COLOR => 2,
        GL_ONE_MINUS_SRC_COLOR => 3,
        GL_SRC_ALPHA => 4,
        GL_ONE_MINUS_SRC_ALPHA => 5,
        GL_DST_COLOR => 6,
        GL_ONE_MINUS_DST_COLOR => 7,
        GL_DST_ALPHA => 8,
        GL_ONE_MINUS_DST_ALPHA => 9,
        GL_SRC_ALPHA_SATURATE => 10,
        _ => 1, // GL_ONE default for an unmodeled factor.
    }
}

/// GL blend equation enum → opaque WebGPU blend-op wire value (`gl_shim.c` `blend_op_wire`).
fn blend_op_wire(e: u32) -> u32 {
    match e {
        GL_FUNC_SUBTRACT => 1,
        GL_FUNC_REVERSE_SUBTRACT => 2,
        GL_MIN => 3,
        GL_MAX => 4,
        _ => 0, // GL_FUNC_ADD and unknown.
    }
}

/// GL depth-compare enum → opaque WebGPU compare-function wire value (WebGPU `CompareFunction`, 1..=8).
fn compare_wire(func: u32) -> u32 {
    match func {
        GL_NEVER => 1,
        GL_LESS => 2,
        GL_EQUAL => 3,
        GL_LEQUAL => 4,
        GL_GREATER => 5,
        GL_NOTEQUAL => 6,
        GL_GEQUAL => 7,
        GL_ALWAYS => 8,
        _ => 2, // GL_LESS default.
    }
}

/// GL cull-face enum → pipeline cull mode (`0` none, `1` front, `2` back). `GL_FRONT_AND_BACK` has no
/// single-face WebGPU equivalent, so it maps to back (the conservative common case).
fn cull_wire(face: u32) -> u32 {
    match face {
        GL_FRONT => 1,
        _ => 2, // GL_BACK / GL_FRONT_AND_BACK.
    }
}

/// GL front-face winding enum → pipeline front-face (`0` CCW, `1` CW).
fn front_face_wire(mode: u32) -> u32 {
    if mode == GL_CW {
        1
    } else {
        0
    }
}

/// Vertex-attribute format from a GLSL declaration type string (`gl_shim.c` `decl_format_wire`).
fn decl_format_wire(t: &str) -> u32 {
    let comps: u32 = if t.contains("vec2") {
        2
    } else if t.contains("vec3") {
        3
    } else if t.starts_with("float") {
        1
    } else {
        4
    };
    let integer = t.starts_with("ivec") || t.starts_with("uvec");
    let kind: u32 = if t.starts_with("ivec") {
        6
    } else if t.starts_with("uvec") {
        5
    } else {
        0
    };
    comps | (kind << 8) | ((integer as u32) << 17)
}
