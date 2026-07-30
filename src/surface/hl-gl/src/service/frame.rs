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
    /// Render targets written by this frame, deduplicated by GL texture generation. Copies requested for
    /// imported EGLImages are appended only after the complete ordered render graph, so an A→B→A frame
    /// captures A's final contents without replaying or reordering any pass.
    pub targets: Vec<FrameTarget>,
}

/// One GL texture generation resolved to its persistent GPU render target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameTarget {
    pub name: u32,
    pub generation: u64,
    pub shared_storage: Option<u64>,
    pub shared_revision: Option<u64>,
    pub surface: u32,
    pub texture: u32,
    pub width: i32,
    pub height: i32,
    pub format: TextureFormat,
    pub token: Option<hl_gpu::protocol::model::descriptor::SurfaceToken>,
}

/// Device-to-host buffer allocated for one requested frame target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameCapture {
    pub target: FrameTarget,
    pub buffer: u32,
    pub offset: u64,
    pub bytes_per_row: u32,
    pub len: usize,
}

/// Total upload bytes a lowered frame carries: the sum of every `WriteBuffer` payload (vertex / index /
/// uniform data + hoisted texture-staging copies — the CreateTexture pixels ride a staging `WriteBuffer`).
/// This is the single most valuable frame instrument: the 4.3 GiB GTK frame is exactly this number.
impl Frame {
    fn upload_bytes(cmds: &[Cmd]) -> u64 {
        if !hl_log::VERBOSE_COMPILED
            || !hl_log::Logging::global()
                .enabled(hl_log::Tags::from(tag::GPU), hl_log::Level::Debug)
        {
            return cmds
                .iter()
                .map(|command| match command {
                    Cmd::WriteBuffer { data, .. } => data.len() as u64,
                    _ => 0,
                })
                .sum();
        }
        let uploads = Uploads::classify(cmds);
        hl_debug!(
            tag::GPU,
            "gl uploads total_bytes={} total_count={} texture={}/{} bound_vertex={}/{} \
             bound_index={}/{} client={}/{} padded={}/{} expanded_index={}/{} uniform={}/{} \
             other={}/{} top={}",
            uploads.total.bytes,
            uploads.total.count,
            uploads.texture.bytes,
            uploads.texture.count,
            uploads.bound_vertex.bytes,
            uploads.bound_vertex.count,
            uploads.bound_index.bytes,
            uploads.bound_index.count,
            uploads.client.bytes,
            uploads.client.count,
            uploads.padded.bytes,
            uploads.padded.count,
            uploads.expanded_index.bytes,
            uploads.expanded_index.count,
            uploads.uniform.bytes,
            uploads.uniform.count,
            uploads.other.bytes,
            uploads.other.count,
            uploads.top.as_deref().unwrap_or("none"),
        );
        uploads.total.bytes
    }

    /// Append publication commands after every render/copy submit and return the exact generations whose
    /// headers may be advanced after the sink acknowledges the batch.
    pub fn append_external_presents(
        &mut self,
        mut reserve: impl FnMut() -> hl_gpu::Result<hl_gpu::protocol::model::descriptor::FrameSerial>,
    ) -> hl_gpu::Result<
        Vec<(
            FrameTarget,
            hl_gpu::protocol::model::descriptor::FrameSerial,
        )>,
    > {
        let mut publications = Vec::new();
        for target in self
            .targets
            .iter()
            .copied()
            .filter(|target| target.token.is_some())
        {
            let serial = reserve()?;
            self.cmds.push(Cmd::Present {
                surface: target.surface,
                texture: target.texture,
                serial,
            });
            publications.push((target, serial));
        }
        Ok(publications)
    }

    /// Lower a frame and append imported-target captures as one transaction.
    ///
    /// Lowering allocates IR ids and populates residency caches before capture dimensions and negotiated
    /// buffer limits are known. Any failure therefore restores the complete pre-lowering resource state;
    /// retrying produces the same resource-creation stream instead of referring to objects that were never
    /// submitted.
    pub fn build_captured(
        ctx: &mut GlContext,
        names: impl IntoIterator<Item = u32>,
        max_buffer_bytes: u64,
    ) -> hl_gpu::Result<Option<(Self, Vec<FrameCapture>)>> {
        let frame_state = ctx.frame_state();
        let Some(mut frame) = Self::build(ctx) else {
            ctx.restore_frame_state(frame_state);
            return Ok(None);
        };
        match frame.capture_targets(ctx, names, max_buffer_bytes) {
            Ok(captures)
                if !captures.is_empty()
                    || frame.targets.iter().any(|target| target.token.is_some()) =>
            {
                Ok(Some((frame, captures)))
            }
            Ok(_) => {
                ctx.restore_frame_state(frame_state);
                Ok(None)
            }
            Err(error) => {
                ctx.restore_frame_state(frame_state);
                Err(error)
            }
        }
    }

    /// Append readbacks for the requested GL texture names to this frame. Copies are packed into the
    /// smallest set of negotiated-size buffers found by first-fit placement, after every render/blit pass.
    /// Repeated writes to one texture generation produce one final capture.
    pub fn capture_targets(
        &mut self,
        ctx: &mut GlContext,
        names: impl IntoIterator<Item = u32>,
        max_buffer_bytes: u64,
    ) -> hl_gpu::Result<Vec<FrameCapture>> {
        let names: std::collections::HashSet<u32> = names.into_iter().collect();
        self.capture_targets_matching(ctx, max_buffer_bytes, |target| {
            names.contains(&target.name) && target.token.is_none()
        })
    }

    /// Append capture copies for external targets without changing their publication or CPU backing plane.
    /// Used only by opt-in presentation diagnostics to inspect the exact GPU texture before `Present`.
    pub fn capture_external_targets(
        &mut self,
        ctx: &mut GlContext,
        max_buffer_bytes: u64,
    ) -> hl_gpu::Result<Vec<FrameCapture>> {
        self.capture_targets_matching(ctx, max_buffer_bytes, |target| target.token.is_some())
    }

    fn capture_targets_matching(
        &mut self,
        ctx: &mut GlContext,
        max_buffer_bytes: u64,
        include: impl Fn(&FrameTarget) -> bool,
    ) -> hl_gpu::Result<Vec<FrameCapture>> {
        const COPY_OFFSET_ALIGNMENT: u64 = 256;

        let targets = self
            .targets
            .iter()
            .copied()
            .filter(include)
            .collect::<Vec<_>>();
        if targets.is_empty() {
            return Ok(Vec::new());
        }

        if max_buffer_bytes == 0 {
            return Err(hl_gpu::GpuError::ResourceLimit(
                "frame capture buffer bytes",
            ));
        }

        struct Planned {
            target: FrameTarget,
            group: usize,
            offset: u64,
            bytes_per_row: u32,
            len: usize,
            allocation: u64,
        }

        let mut planned = Vec::with_capacity(targets.len());
        for target in targets {
            let width = target.width.max(1) as u32;
            let height = target.height.max(1) as u32;
            let row_bytes = width
                .checked_mul(4)
                .ok_or(hl_gpu::GpuError::ResourceLimit("frame capture row bytes"))?;
            let bytes_per_row = row_bytes
                .checked_add(COPY_OFFSET_ALIGNMENT as u32 - 1)
                .map(|value| value & !(COPY_OFFSET_ALIGNMENT as u32 - 1))
                .ok_or(hl_gpu::GpuError::ResourceLimit("frame capture row pitch"))?;
            let len = usize::try_from(
                u64::from(row_bytes)
                    .checked_mul(u64::from(height))
                    .ok_or(hl_gpu::GpuError::ResourceLimit("frame capture bytes"))?,
            )
            .map_err(|_| hl_gpu::GpuError::ResourceLimit("frame capture bytes"))?;
            let allocation = u64::from(bytes_per_row)
                .checked_mul(u64::from(height))
                .ok_or(hl_gpu::GpuError::ResourceLimit("frame capture pitch bytes"))?;
            if allocation > max_buffer_bytes {
                return Err(hl_gpu::GpuError::ResourceLimit(
                    "frame capture buffer bytes",
                ));
            }
            planned.push(Planned {
                target,
                group: usize::MAX,
                offset: 0,
                bytes_per_row,
                len,
                allocation,
            });
        }

        // Largest-first placement avoids an early run of small targets fragmenting buffers that later
        // targets could otherwise share. Capture records retain render-target order independently.
        let mut placement_order = (0..planned.len()).collect::<Vec<_>>();
        placement_order.sort_by_key(|&index| std::cmp::Reverse(planned[index].allocation));
        let mut group_sizes = Vec::<u64>::new();
        for index in placement_order {
            let allocation = planned[index].allocation;
            let placement = group_sizes.iter().enumerate().find_map(|(group, end)| {
                let offset =
                    end.checked_add(COPY_OFFSET_ALIGNMENT - 1)? & !(COPY_OFFSET_ALIGNMENT - 1);
                let next = offset.checked_add(allocation)?;
                (next <= max_buffer_bytes).then_some((group, offset, next))
            });
            let (group, offset) = if let Some((group, offset, next)) = placement {
                group_sizes[group] = next;
                (group, offset)
            } else {
                group_sizes.push(allocation);
                (group_sizes.len() - 1, 0)
            };
            planned[index].group = group;
            planned[index].offset = offset;
        }

        // Allocate only after every target has been validated. A rejected plan must not consume IR ids.
        let buffers = group_sizes
            .iter()
            .map(|_| ctx.alloc_buffer_ir())
            .collect::<hl_gpu::Result<Vec<_>>>()?;
        let mut copies = Vec::with_capacity(planned.len());
        let captures = planned
            .into_iter()
            .map(|capture| {
                let buffer = buffers[capture.group];
                copies.push(Enc::CopyTextureToBuffer {
                    src: capture.target.texture,
                    mip: 0,
                    width: capture.target.width.max(1) as u32,
                    height: capture.target.height.max(1) as u32,
                    dst: buffer,
                    dst_offset: capture.offset,
                    bytes_per_row: capture.bytes_per_row,
                });
                FrameCapture {
                    target: capture.target,
                    buffer,
                    offset: capture.offset,
                    bytes_per_row: capture.bytes_per_row,
                    len: capture.len,
                }
            })
            .collect::<Vec<_>>();
        for (&buffer, &size) in buffers.iter().zip(&group_sizes) {
            self.cmds.push(Cmd::CreateBuffer(
                buffer,
                BufferDesc {
                    size,
                    usage: buffer_usage::COPY_DST,
                    label: "gl-image-readback".into(),
                },
            ));
        }
        self.cmds.push(Cmd::Submit(CommandBuffer {
            encoder: copies,
            signal: None,
        }));
        Ok(captures)
    }
}

#[derive(Clone, Copy, Default)]
struct Upload {
    bytes: u64,
    count: u64,
}

impl Upload {
    fn add(&mut self, bytes: u64) {
        self.bytes += bytes;
        self.count += 1;
    }
}

#[derive(Default)]
struct Uploads {
    total: Upload,
    texture: Upload,
    bound_vertex: Upload,
    bound_index: Upload,
    client: Upload,
    padded: Upload,
    expanded_index: Upload,
    uniform: Upload,
    other: Upload,
    top: Option<String>,
}

impl Uploads {
    fn classify(cmds: &[Cmd]) -> Self {
        use std::collections::{HashMap, HashSet};

        let buffers = cmds
            .iter()
            .filter_map(|command| match command {
                Cmd::CreateBuffer(id, desc) => Some((*id, (desc.usage, desc.label.as_str()))),
                _ => None,
            })
            .collect::<HashMap<_, _>>();
        let texture_stages = cmds
            .iter()
            .filter_map(|command| match command {
                Cmd::Submit(buffer) => Some(&buffer.encoder),
                _ => None,
            })
            .flatten()
            .filter_map(|command| match command {
                Enc::CopyBufferToTexture { src, .. } => Some(*src),
                _ => None,
            })
            .collect::<HashSet<_>>();

        let mut uploads = Self::default();
        let mut top_bytes = 0;
        for command in cmds {
            let Cmd::WriteBuffer { id, data, .. } = command else {
                continue;
            };
            let bytes = data.len() as u64;
            uploads.total.add(bytes);
            let (usage, label) = buffers.get(id).copied().unwrap_or((0, ""));
            if texture_stages.contains(id) {
                uploads.texture.add(bytes);
            } else if label.starts_with("gl-bound-vertex:") {
                uploads.bound_vertex.add(bytes);
            } else if label.starts_with("gl-bound-index:") {
                uploads.bound_index.add(bytes);
            } else if label.starts_with("gl-client-") || label == "gl-constant-vertex" {
                uploads.client.add(bytes);
            } else if label.starts_with("gl-padded-vertex:") {
                uploads.padded.add(bytes);
            } else if label == "gl-expanded-index" {
                uploads.expanded_index.add(bytes);
            } else if usage & buffer_usage::UNIFORM != 0 {
                uploads.uniform.add(bytes);
            } else {
                uploads.other.add(bytes);
            }
            if bytes > top_bytes && label.starts_with("gl-bound-") {
                top_bytes = bytes;
                uploads.top = Some(format!("{label}:{bytes}"));
            }
        }
        uploads
    }
}

fn frame_target(
    ctx: &GlContext,
    fbo: u32,
    snapshot: Option<crate::model::program::TargetSnapshot>,
    texture: u32,
    width: i32,
    height: i32,
    format: TextureFormat,
) -> Option<FrameTarget> {
    if fbo == 0 {
        return None;
    }
    let target = snapshot.or_else(|| {
        let name = ctx.local.framebuffers.color_attachment(fbo);
        ctx.textures
            .get(name)
            .filter(|texture| texture.w > 0 && texture.h > 0)
            .map(|texture| crate::model::program::TargetSnapshot {
                texture: name,
                generation: texture.gen,
                shared_storage: texture.shared_storage(),
                shared_revision: texture
                    .shared_current_identity()
                    .map(|(_, revision)| revision),
                width: texture.w,
                height: texture.h,
                format: texture.ir_format,
            })
    })?;
    Some(FrameTarget {
        name: target.texture,
        generation: target.generation,
        shared_storage: target.shared_storage,
        shared_revision: target.shared_revision,
        surface: ctx
            .fbo_surface(target.texture, target.generation)
            .unwrap_or_default(),
        texture,
        width,
        height,
        format,
        token: ctx.external_target(target.texture, target.generation),
    })
}

fn push_final_target(targets: &mut Vec<FrameTarget>, target: FrameTarget) {
    if let Some(existing) = targets.iter_mut().find(|existing| {
        (existing.name, existing.generation) == (target.name, target.generation)
            || target
                .token
                .is_some_and(|token| existing.token == Some(token))
    }) {
        *existing = target;
    } else {
        targets.push(target);
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
        let frame = Self::build_raw(ctx);
        if ctx.allocation_exhausted() {
            ctx.set_gl_error(crate::result::GL_OUT_OF_MEMORY);
            return None;
        }
        let mut frame = frame?;
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
                Cmd::CreateSampler(id, _)
                    if *id != placeholder_samp && !ctx.is_resident_sampler(*id) =>
                {
                    cleanup.push(Cmd::DestroySampler(*id))
                }
                Cmd::CreateTexture(id, desc)
                    if desc.label == "gl-retired-snapshot" || desc.label == "gl-retired-fbo" =>
                {
                    cleanup.push(Cmd::DestroyTexture(*id))
                }
                _ => {}
            }
        }
        frame.cmds.extend(cleanup);
        Some(frame)
    }

    /// The raw frame assembler (pre-residency-cleanup). See [`build_frame_ir`].
    fn build_raw(ctx: &mut GlContext) -> Option<Frame> {
        if ctx.local.recording.draws.is_empty() && ctx.local.recording.blits.is_empty() {
            return None;
        }
        // A surfaceless EGL context has no default framebuffer, but user FBOs remain fully renderable.
        // Reject only work that actually targets framebuffer 0; globally rejecting the context made Chrome's
        // surfaceless GPU command buffers acknowledge `glFlush` without executing their FBO commands.
        if !ctx.local.surf.have && ctx.local.recording.draws.iter().any(|draw| draw.fbo == 0) {
            return None;
        }
        let groups = RenderPasses::groups(&ctx.local.recording.draws);
        let ordered = !ctx.local.recording.operations.is_empty()
            && (!ctx.local.recording.blits.is_empty() || groups.len() > 1);
        if ordered {
            return RenderPasses::build_ordered(ctx);
        }
        // Partition the recorded draw-list into contiguous runs that share a bound framebuffer. A frame that
        // renders to more than one framebuffer (the GskGL / offscreen-compositor shape: a glyph atlas + offscreen
        // render targets, then a final default-framebuffer pass that SAMPLES them) lowers as a SEQUENCE of render
        // passes, one per run — not collapsed onto the first draw's FBO. A single run is the single-target fast
        // path (byte-identical to the pre-frame-graph lowering).
        // A `glBlitFramebuffer` frame (or a genuinely multi-framebuffer frame) lowers as a SEQUENCE of passes,
        // one per FBO run, followed by the recorded blit copies — so route to the multi-pass builder whenever a
        // blit was recorded, even if all draws share one framebuffer.
        if groups.len() > 1 || !ctx.local.recording.blits.is_empty() {
            return RenderPasses::build_multi(ctx, &groups);
        }
        if ctx.local.recording.draws.iter().all(|d| d.is_clear) {
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
mod range;
mod texture;
mod vertex;

use geometry::*;
use lower::*;
use passes::*;
use pipeline::*;
use texture::*;
use vertex::*;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn upload_classification_accounts_for_every_written_byte() {
        let buffer = |id, usage, label: &str, bytes| {
            vec![
                Cmd::CreateBuffer(
                    id,
                    BufferDesc {
                        size: bytes as u64,
                        usage,
                        label: label.to_owned(),
                    },
                ),
                Cmd::WriteBuffer {
                    id,
                    offset: 0,
                    data: vec![0; bytes],
                },
            ]
        };
        let mut cmds = buffer(1, buffer_usage::VERTEX, "gl-bound-vertex:7:3", 11);
        cmds.extend(buffer(2, buffer_usage::INDEX, "gl-bound-index:8:4", 13));
        cmds.extend(buffer(3, buffer_usage::VERTEX, "gl-client-vertex", 17));
        cmds.extend(buffer(4, buffer_usage::VERTEX, "gl-padded-vertex:9:5", 19));
        cmds.extend(buffer(5, buffer_usage::INDEX, "gl-expanded-index", 23));
        cmds.extend(buffer(6, buffer_usage::UNIFORM, "", 29));
        cmds.extend(buffer(7, buffer_usage::COPY_SRC, "", 31));
        cmds.extend(buffer(8, buffer_usage::STORAGE, "", 37));
        cmds.push(Cmd::Submit(CommandBuffer {
            encoder: vec![Enc::CopyBufferToTexture {
                src: 7,
                src_offset: 0,
                bytes_per_row: 4,
                dst: 70,
                mip: 0,
                width: 1,
                height: 1,
            }],
            signal: None,
        }));

        let uploads = Uploads::classify(&cmds);
        assert_eq!(uploads.bound_vertex.bytes, 11);
        assert_eq!(uploads.bound_index.bytes, 13);
        assert_eq!(uploads.client.bytes, 17);
        assert_eq!(uploads.padded.bytes, 19);
        assert_eq!(uploads.expanded_index.bytes, 23);
        assert_eq!(uploads.uniform.bytes, 29);
        assert_eq!(uploads.texture.bytes, 31);
        assert_eq!(uploads.other.bytes, 37);
        assert_eq!(uploads.total.bytes, 180);
        assert_eq!(
            uploads.total.bytes,
            uploads.bound_vertex.bytes
                + uploads.bound_index.bytes
                + uploads.client.bytes
                + uploads.padded.bytes
                + uploads.expanded_index.bytes
                + uploads.uniform.bytes
                + uploads.texture.bytes
                + uploads.other.bytes
        );
        assert_eq!(uploads.total.count, 8);
        assert_eq!(uploads.top.as_deref(), Some("gl-bound-index:8:4:13"));
        assert_eq!(Frame::upload_bytes(&cmds), uploads.total.bytes);
    }

    #[test]
    fn capture_batches_selected_final_targets() {
        let mut ctx = GlContext::new();
        let mut targets = Vec::new();
        let first_a = FrameTarget {
            name: 7,
            generation: 2,
            shared_storage: None,
            shared_revision: None,
            surface: 700,
            texture: 70,
            width: 8,
            height: 4,
            format: TextureFormat::Rgba8Unorm,
            token: None,
        };
        let target_b = FrameTarget {
            name: 9,
            generation: 1,
            shared_storage: None,
            shared_revision: None,
            surface: 900,
            texture: 90,
            width: 2,
            height: 3,
            format: TextureFormat::Rgba8Unorm,
            token: None,
        };
        let final_a = FrameTarget {
            texture: 71,
            ..first_a
        };
        push_final_target(&mut targets, first_a);
        push_final_target(&mut targets, target_b);
        push_final_target(&mut targets, final_a);

        let mut frame = Frame {
            cmds: vec![Cmd::Submit(CommandBuffer {
                encoder: Vec::new(),
                signal: None,
            })],
            present: (0, 0),
            target_width: 1,
            target_height: 1,
            target_format: TextureFormat::Bgra8Unorm,
            color_attachments: Vec::new(),
            targets,
        };
        let captures = frame.capture_targets(&mut ctx, [7, 9, 7], 1 << 20).unwrap();

        assert_eq!(captures.len(), 2);
        assert_eq!(captures[0].target, final_a);
        assert_eq!(captures[0].offset, 0);
        assert_eq!(captures[0].bytes_per_row, 256);
        assert_eq!(captures[0].len, 8 * 4 * 4);
        assert_eq!(captures[1].target, target_b);
        assert_eq!(captures[1].buffer, captures[0].buffer);
        assert_eq!(captures[1].offset, 1024);
        assert_eq!(captures[1].bytes_per_row, 256);
        assert_eq!(captures[1].len, 2 * 3 * 4);
        let create_buffers = frame
            .cmds
            .iter()
            .filter_map(|command| match command {
                Cmd::CreateBuffer(buffer, descriptor) => Some((*buffer, descriptor.size)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            create_buffers,
            [(
                captures[0].buffer,
                captures[1].offset
                    + u64::from(captures[1].bytes_per_row) * captures[1].target.height as u64
            )]
        );
        assert_eq!(
            frame
                .cmds
                .iter()
                .filter(|command| matches!(command, Cmd::Submit(_)))
                .count(),
            2,
            "all capture copies must share one additional command buffer"
        );
        let Cmd::Submit(batch) = frame.cmds.last().unwrap() else {
            panic!("capture batch must be submitted after the render graph");
        };
        assert_eq!(batch.encoder.len(), 2);
        assert!(matches!(
            batch.encoder[0],
            Enc::CopyTextureToBuffer {
                src: 71,
                dst_offset: 0,
                bytes_per_row: 256,
                ..
            }
        ));
        assert!(matches!(
            batch.encoder[1],
            Enc::CopyTextureToBuffer {
                src: 90,
                dst_offset: 1024,
                bytes_per_row: 256,
                ..
            }
        ));
    }

    #[test]
    fn capture_rejects_hostile_dimensions_without_allocating() {
        let mut ctx = GlContext::new();
        let mut frame = Frame {
            cmds: Vec::new(),
            present: (0, 0),
            target_width: 1,
            target_height: 1,
            target_format: TextureFormat::Bgra8Unorm,
            color_attachments: Vec::new(),
            targets: vec![FrameTarget {
                name: 7,
                generation: 1,
                shared_storage: None,
                shared_revision: None,
                surface: 700,
                texture: 70,
                width: i32::MAX,
                height: i32::MAX,
                format: TextureFormat::Bgra8Unorm,
                token: None,
            }],
        };

        assert!(matches!(
            frame.capture_targets(&mut ctx, [7], 256 << 20),
            Err(hl_gpu::GpuError::ResourceLimit(_))
        ));
        assert!(frame.cmds.is_empty());
    }

    #[test]
    fn capture_splits_targets_at_negotiated_buffer_limit() {
        let mut ctx = GlContext::new();
        let targets = [7, 9, 11]
            .into_iter()
            .map(|name| FrameTarget {
                name,
                generation: 1,
                shared_storage: None,
                shared_revision: None,
                surface: name * 100,
                texture: name * 10,
                width: 64,
                height: 1,
                format: TextureFormat::Bgra8Unorm,
                token: None,
            })
            .collect();
        let mut frame = Frame {
            cmds: Vec::new(),
            present: (0, 0),
            target_width: 1,
            target_height: 1,
            target_format: TextureFormat::Bgra8Unorm,
            color_attachments: Vec::new(),
            targets,
        };

        let captures = frame.capture_targets(&mut ctx, [7, 9, 11], 512).unwrap();

        assert_eq!(captures[0].buffer, captures[1].buffer);
        assert_ne!(captures[1].buffer, captures[2].buffer);
        assert_eq!(captures[0].offset, 0);
        assert_eq!(captures[1].offset, 256);
        assert_eq!(captures[2].offset, 0);
        let sizes = frame
            .cmds
            .iter()
            .filter_map(|command| match command {
                Cmd::CreateBuffer(_, descriptor) => Some(descriptor.size),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(sizes, [512, 256]);
    }

    #[test]
    fn external_target_is_presented_after_submit_without_capture() {
        let mut ctx = GlContext::new();
        let token = hl_gpu::protocol::model::descriptor::SurfaceToken::new(9).unwrap();
        let mut frame = Frame {
            cmds: vec![
                Cmd::CreateSurface(
                    70,
                    hl_gpu::protocol::model::descriptor::SurfaceDesc {
                        width: 64,
                        height: 32,
                        format: TextureFormat::Bgra8Unorm,
                        token,
                    },
                ),
                Cmd::Submit(CommandBuffer {
                    encoder: Vec::new(),
                    signal: None,
                }),
            ],
            present: (0, 0),
            target_width: 1,
            target_height: 1,
            target_format: TextureFormat::Bgra8Unorm,
            color_attachments: Vec::new(),
            targets: vec![FrameTarget {
                name: 7,
                generation: 3,
                shared_storage: None,
                shared_revision: None,
                surface: 70,
                texture: 71,
                width: 64,
                height: 32,
                format: TextureFormat::Bgra8Unorm,
                token: Some(token),
            }],
        };

        assert!(frame
            .capture_targets(&mut ctx, [7], 1 << 20)
            .unwrap()
            .is_empty());
        let publications = frame
            .append_external_presents(|| hl_gpu::protocol::model::descriptor::FrameSerial::new(11))
            .unwrap();

        assert_eq!(publications[0].0.generation, 3);
        let create = frame
            .cmds
            .iter()
            .position(|command| matches!(command, Cmd::CreateSurface(70, _)))
            .unwrap();
        let submit = frame
            .cmds
            .iter()
            .position(|command| matches!(command, Cmd::Submit(_)))
            .unwrap();
        let present = frame
            .cmds
            .iter()
            .position(|command| matches!(command, Cmd::Present { .. }))
            .unwrap();
        assert!(create < submit && submit < present);
        assert!(!frame.cmds.iter().any(|command| {
            matches!(
                command,
                Cmd::CreateBuffer(_, descriptor) if descriptor.label.contains("readback")
            ) || matches!(
                command,
                Cmd::Submit(CommandBuffer { encoder, .. })
                    if encoder.iter().any(|command| matches!(command, Enc::CopyTextureToBuffer { .. }))
            )
        }));
        assert!(matches!(
            frame.cmds[present],
            Cmd::Present {
                surface: 70,
                texture: 71,
                serial,
            } if serial.get() == 11
        ));
    }

    #[test]
    fn external_diagnostic_capture_precedes_the_same_frame_present() {
        let mut ctx = GlContext::new();
        let token = hl_gpu::protocol::model::descriptor::SurfaceToken::new(9).unwrap();
        let mut frame = Frame {
            cmds: vec![Cmd::Submit(CommandBuffer {
                encoder: Vec::new(),
                signal: None,
            })],
            present: (0, 0),
            target_width: 1,
            target_height: 1,
            target_format: TextureFormat::Bgra8Unorm,
            color_attachments: Vec::new(),
            targets: vec![FrameTarget {
                name: 7,
                generation: 3,
                shared_storage: None,
                shared_revision: None,
                surface: 70,
                texture: 71,
                width: 64,
                height: 32,
                format: TextureFormat::Bgra8Unorm,
                token: Some(token),
            }],
        };

        let captures = frame.capture_external_targets(&mut ctx, 1 << 20).unwrap();
        let publications = frame
            .append_external_presents(|| hl_gpu::protocol::model::descriptor::FrameSerial::new(11))
            .unwrap();

        assert_eq!(captures.len(), 1);
        assert_eq!(captures[0].target.token, Some(token));
        assert_eq!(publications[0].1.get(), 11);
        let copy = frame
            .cmds
            .iter()
            .position(|command| {
                matches!(
                    command,
                    Cmd::Submit(CommandBuffer { encoder, .. })
                        if encoder.iter().any(
                            |command| matches!(command, Enc::CopyTextureToBuffer { src: 71, .. })
                        )
                )
            })
            .expect("external target capture copy");
        let present = frame
            .cmds
            .iter()
            .position(|command| matches!(command, Cmd::Present { .. }))
            .expect("external target present");
        assert!(copy < present);
    }

    #[test]
    fn external_a_b_c_a_publishes_each_final_generation_once() {
        let external = |name: u32, texture: u32, token: u64| FrameTarget {
            name,
            generation: 1,
            shared_storage: None,
            shared_revision: None,
            surface: name * 10,
            texture,
            width: 8,
            height: 8,
            format: TextureFormat::Bgra8Unorm,
            token: Some(hl_gpu::protocol::model::descriptor::SurfaceToken::new(token).unwrap()),
        };
        let mut targets = Vec::new();
        push_final_target(&mut targets, external(1, 101, 11));
        push_final_target(&mut targets, external(2, 201, 12));
        push_final_target(&mut targets, external(3, 301, 13));
        push_final_target(&mut targets, external(1, 102, 11));
        let mut frame = Frame {
            cmds: vec![Cmd::Submit(CommandBuffer {
                encoder: Vec::new(),
                signal: None,
            })],
            present: (0, 0),
            target_width: 1,
            target_height: 1,
            target_format: TextureFormat::Bgra8Unorm,
            color_attachments: Vec::new(),
            targets,
        };
        let mut serial = 20;
        let publications = frame
            .append_external_presents(|| {
                serial += 1;
                hl_gpu::protocol::model::descriptor::FrameSerial::new(serial)
            })
            .unwrap();

        assert_eq!(
            publications
                .iter()
                .map(|(target, _)| (target.name, target.texture))
                .collect::<Vec<_>>(),
            [(1, 102), (2, 201), (3, 301)]
        );
        assert_eq!(
            frame
                .cmds
                .iter()
                .filter(|command| matches!(command, Cmd::Present { .. }))
                .count(),
            3
        );
    }

    #[test]
    fn external_siblings_publish_the_shared_surface_once() {
        let token = hl_gpu::protocol::model::descriptor::SurfaceToken::new(17).unwrap();
        let mut targets = Vec::new();
        push_final_target(
            &mut targets,
            FrameTarget {
                name: 1,
                generation: 1,
                shared_storage: None,
                shared_revision: None,
                surface: 10,
                texture: 20,
                width: 8,
                height: 8,
                format: TextureFormat::Bgra8Unorm,
                token: Some(token),
            },
        );
        push_final_target(
            &mut targets,
            FrameTarget {
                name: 2,
                generation: 4,
                shared_storage: None,
                shared_revision: None,
                surface: 10,
                texture: 20,
                width: 8,
                height: 8,
                format: TextureFormat::Bgra8Unorm,
                token: Some(token),
            },
        );
        assert_eq!(targets.len(), 1);
        assert_eq!((targets[0].name, targets[0].generation), (2, 4));
    }

    #[test]
    fn multi_target_serial_exhaustion_fails_before_submission() {
        let token =
            |value| Some(hl_gpu::protocol::model::descriptor::SurfaceToken::new(value).unwrap());
        let mut frame = Frame {
            cmds: vec![Cmd::Submit(CommandBuffer {
                encoder: Vec::new(),
                signal: None,
            })],
            present: (0, 0),
            target_width: 1,
            target_height: 1,
            target_format: TextureFormat::Bgra8Unorm,
            color_attachments: Vec::new(),
            targets: vec![
                FrameTarget {
                    name: 1,
                    generation: 1,
                    shared_storage: None,
                    shared_revision: None,
                    surface: 10,
                    texture: 11,
                    width: 1,
                    height: 1,
                    format: TextureFormat::Bgra8Unorm,
                    token: token(1),
                },
                FrameTarget {
                    name: 2,
                    generation: 1,
                    shared_storage: None,
                    shared_revision: None,
                    surface: 20,
                    texture: 21,
                    width: 1,
                    height: 1,
                    format: TextureFormat::Bgra8Unorm,
                    token: token(2),
                },
            ],
        };
        let mut calls = 0;
        let error = frame
            .append_external_presents(|| {
                calls += 1;
                if calls == 1 {
                    hl_gpu::protocol::model::descriptor::FrameSerial::new(u64::MAX)
                } else {
                    Err(hl_gpu::GpuError::ResourceLimit("external frame serials"))
                }
            })
            .unwrap_err();
        assert!(matches!(
            error,
            hl_gpu::GpuError::ResourceLimit("external frame serials")
        ));
        assert_eq!(calls, 2);
    }
}
