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
    BindEntry, BindGroupDesc, BindResource, BlendState, BufferDesc, ColorAttachment,
    ColorTargetState, DepthAttachment, DepthState, Extent3d, Origin3d, RenderPipelineDesc,
    SamplerDesc, ShaderRef, StencilFaceState, SurfaceDesc, TextureDesc, TextureSubresource,
    VertexAttr, VertexLayout,
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
    /// For a multiple-render-target frame (`glDrawBuffers` MRT): the render-target texture IR id per
    /// `GL_COLOR_ATTACHMENT{index}` this frame wrote, so `glReadPixels` under a `glReadBuffer(ATTACHMENT{i})`
    /// selection reads the RIGHT attachment (not just `present`). Indexed by attachment index (0-based);
    /// EMPTY for an ordinary single-target frame (the common path, `present` is the only target).
    pub color_attachments: Vec<u32>,
}

/// Total upload bytes a lowered frame carries: the sum of every `WriteBuffer` payload (vertex / index /
/// uniform data + hoisted texture-staging copies — the CreateTexture pixels ride a staging `WriteBuffer`).
/// This is the single most valuable frame instrument: the 4.3 GiB GTK frame is exactly this number.
impl Frame {
    fn upload_bytes(cmds: &[Cmd]) -> u64 {
        cmds.iter()
            .map(|c| match c {
                Cmd::WriteBuffer { data, .. } => data.len() as u64,
                _ => 0,
            })
            .sum()
    }
}

/// Emit the per-frame observability: one scannable `key=val` debug line + the frame counters. Gated +
/// zero-cost when `tag::GL` logging / counters are off.
fn log_frame(w: i32, h: i32, draws: usize, passes: usize, cmds: usize, bytes: u64) {
    hl_debug!(
        tag::GL,
        "frame {}x{} draws={} passes={} cmds={} bytes={}",
        w,
        h,
        draws,
        passes,
        cmds,
        bytes
    );
    hl_add!(tag::GL, "frame_bytes", bytes);
    hl_add!(tag::GL, "frame_cmds", cmds as u64);
    hl_count!(tag::GL, "frames");
}

/// Assemble the frame's `Cmd` stream from the recorded draw-list, or `None` if there is nothing (or
/// nothing yet supported) to present. Mints the IR ids it needs from `ctx`.
///
/// Wraps [`build_frame_ir_raw`] and then FREES the frame's PER-DRAW EPHEMERAL resources: the single-use
/// `COPY_SRC` texture-staging buffers, the per-draw implicit `UNIFORM` uniform buffers, the per-draw bind
/// groups, and the per-draw samplers (except the shared placeholder). Each is created + consumed ENTIRELY
/// within THIS frame's own `Submit`s and never referenced by a later frame, so a `Destroy*` appended AFTER
/// the submits (so the GPU work has run) and BEFORE the swap's `Present` (the builder's cmds carry no
/// `Present` — swap appends that) makes their residency net to ZERO within the transactional per-frame
/// charge (see `hl-gpu/src/runtime/service/account.rs::charge_frame`). Without this a long-running
/// multi-frame app (Chrome) leaks a uniform buffer + bind group + sampler PER DRAW every frame plus ~25 MiB
/// of staging per flushed frame, and the executor NACKs `ResourceLimit("connection residency")`. Only
/// FRAME-LOCAL resources are freed — persistent cached resources (vertex/index buffers, sampled textures,
/// pipelines, shaders, render targets) are re-referenced by id next frame and left intact. (Cross-frame
/// texture retirement is NOT done here: a draw retained across a NACKed swap can still reference an
/// abandoned/deleted id, so destroying it would `UnknownId` the retained frame — a deeper deferral gap.)
impl Frame {
    pub fn build(ctx: &mut GlContext) -> Option<Frame> {
        let mut frame = Self::build_raw(ctx)?;
        // Append `Destroy*` for every PER-DRAW EPHEMERAL resource this frame created — the ones referenced ONLY
        // within the frame's own `Submit`s and never reused across frames: the single-use `COPY_SRC` texture
        // staging buffers, the per-draw implicit `UNIFORM` uniform buffers, the per-draw bind groups, and the
        // per-draw samplers (EXCEPT the shared placeholder sampler, which persists). Freed AFTER the submits and
        // BEFORE the swap's `Present` (the builder's cmds carry no `Present`), so their residency nets to ZERO in
        // the transactional per-frame charge — without this a long-running app (Chrome) leaks a uniform buffer +
        // sampler + bind group PER DRAW every frame and exhausts the connection residency + object caps. The
        // PERSISTENT resources (cached vertex/index buffers, cached sampled textures, pipelines, shaders, render
        // targets, the placeholder texture+sampler) are NOT freed — they are re-referenced by id next frame.
        use hl_gpu::protocol::model::enums::buffer_usage;
        let placeholder_samp = ctx.placeholder_sampler_ir();
        let mut cleanup: Vec<Cmd> = Vec::new();
        for c in &frame.cmds {
            match c {
                Cmd::CreateBuffer(id, d)
                    if d.usage == buffer_usage::COPY_SRC || d.usage == buffer_usage::UNIFORM =>
                {
                    cleanup.push(Cmd::DestroyBuffer(*id));
                }
                Cmd::CreateBindGroup(id, _) => cleanup.push(Cmd::DestroyBindGroup(*id)),
                Cmd::CreateSampler(id, _) if *id != placeholder_samp => {
                    cleanup.push(Cmd::DestroySampler(*id))
                }
                _ => {}
            }
        }
        frame.cmds.extend(cleanup);
        Some(frame)
    }

    /// The raw frame assembler (pre-residency-cleanup). See [`build_frame_ir`].
    fn build_raw(ctx: &mut GlContext) -> Option<Frame> {
        if !ctx.surf.have || ctx.draws.is_empty() {
            return None;
        }
        // Partition the recorded draw-list into contiguous runs that share a bound framebuffer. A frame that
        // renders to more than one framebuffer (the GskGL / offscreen-compositor shape: a glyph atlas + offscreen
        // render targets, then a final default-framebuffer pass that SAMPLES them) lowers as a SEQUENCE of render
        // passes, one per run — not collapsed onto the first draw's FBO. A single run is the single-target fast
        // path (byte-identical to the pre-frame-graph lowering).
        let groups = RenderPasses::groups(&ctx.draws);
        // A `glBlitFramebuffer` frame (or a genuinely multi-framebuffer frame) lowers as a SEQUENCE of passes,
        // one per FBO run, followed by the recorded blit copies — so route to the multi-pass builder whenever a
        // blit was recorded, even if all draws share one framebuffer.
        if groups.len() > 1 || !ctx.blits.is_empty() {
            return RenderPasses::build_multi(ctx, &groups);
        }
        if ctx.draws.iter().all(|d| d.is_clear) {
            return Some(Self::build_clear(ctx));
        }
        // One framebuffer, one or more geometry draws (optionally led by a clear) → the single/multi-draw path.
        Self::build_geometry(ctx)
    }
}

/// Partition the draw-list into maximal contiguous runs that share a bound framebuffer, in record order,
/// as `(fbo, start, end)` half-open index ranges into `draws`. Each run becomes one render pass targeting
/// that FBO's color attachment (fbo `0` = the default window framebuffer). A clear carries the FBO bound
/// when it was recorded, so a `glClear` under an offscreen FBO groups with that FBO's geometry.
mod geometry;
mod globals;
mod lower;
mod passes;
mod pipeline;
mod texture;
mod vertex;

use geometry::*;
use lower::*;
use passes::*;
use pipeline::*;
use texture::*;
use vertex::*;
