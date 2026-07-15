//! Command-buffer replay: turn a validated [`CommandBuffer`]'s encoder ops into real wgpu work.
//!
//! Render and compute passes are executed as forward-scanned `Begin..End` units (a wgpu pass borrows its
//! encoder for its whole lifetime, so it can't straddle the outer op loop); everything else — sub-rect
//! clears, buffer/texture copies, fills — is CPU-mediated through the byte-addressable buffer/texture
//! helpers so wgpu's copy-alignment rules never leak to the protocol boundary. Each unit submits and, on
//! the readback paths, `poll(Wait)`s, giving the strict sequential semantics the CPU oracle guarantees.
//!
//! The clears, copies, dispatches, and draws here are the executed analogues of the CPU oracle's
//! `submit` (`hl_wip-gpu/src/cpu/executor.rs`) — same ops, same order, now on the GPU.

use hl_gpu::protocol::model::command::{CommandBuffer, Enc};
use hl_gpu::protocol::model::descriptor::{
    ColorAttachment, DepthAttachment, Extent3d, Origin3d, TextureSubresource,
};
use hl_gpu::protocol::model::enums::{IndexFormat, LoadOp, TextureAspect};
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};
use hl_log::tag;

use crate::convert::{clear_texel, texel_bytes};
use crate::pipeline::{self, PipelineNative};
use crate::{bindgroup, buffer, fence, texture, WgpuExecutor};

/// Protocol index format → wgpu index format.
fn index_format(f: IndexFormat) -> wgpu::IndexFormat {
    match f {
        IndexFormat::U16 => wgpu::IndexFormat::Uint16,
        IndexFormat::U32 => wgpu::IndexFormat::Uint32,
    }
}

impl WgpuExecutor {
    pub(crate) fn submit_cb(&mut self, res: &mut SessionResources, cb: &CommandBuffer) -> Result<()> {
        let ops = &cb.encoder;
        let mut i = 0;
        while i < ops.len() {
            match &ops[i] {
                Enc::BeginRenderPass { color, depth } => {
                    let end = find_end(ops, i, Enc::EndRenderPass)?;
                    self.run_render_pass(res, color, depth.as_ref(), &ops[i + 1..end])?;
                    i = end + 1;
                }
                Enc::BeginComputePass => {
                    let end = find_end(ops, i, Enc::EndComputePass)?;
                    self.run_compute_pass(res, &ops[i + 1..end])?;
                    i = end + 1;
                }
                Enc::ClearRect { texture, x, y, w, h, color } => {
                    let t = texture::native(res, *texture)?;
                    let texel = clear_texel(t.format, *color)?;
                    let mut data = Vec::with_capacity(texel.len() * (*w as usize) * (*h as usize));
                    for _ in 0..(*w as usize * *h as usize) {
                        data.extend_from_slice(&texel);
                    }
                    self.write_region(res, *texture, *x, *y, 0, *w, *h, 1, 0, &data)?;
                    i += 1;
                }
                Enc::CopyBufferToBuffer { src, src_offset, dst, dst_offset, size } => {
                    let bytes = self.read_bytes(res, *src, *src_offset, *size as usize)?;
                    self.write_bytes(res, *dst, *dst_offset, &bytes)?;
                    i += 1;
                }
                Enc::CopyTextureToBuffer { src, width, height, dst, dst_offset, bytes_per_row, .. } => {
                    let t = texture::native(res, *src)?;
                    let bpt = texel_bytes(t.format)? as u32;
                    // The tight readback plane is packed at the TEXTURE's width, not the copy region's
                    // width — the source row stride is `tex_width*bpt`, so a sub-region copy (width <
                    // texture width) advances by the full plane stride, exactly as the CPU oracle does.
                    let src_stride = (t.width * bpt) as usize;
                    let plane = self.read_texture_tight(res, *src)?;
                    let row = (*width * bpt) as usize;
                    // `bytes_per_row == 0` means "tightly packed" on the destination (the protocol/oracle
                    // convention); a non-zero value is the explicit row stride.
                    let dst_stride = if *bytes_per_row == 0 { row } else { *bytes_per_row as usize };
                    for r in 0..*height as usize {
                        let s = r * src_stride;
                        let d = *dst_offset + (r * dst_stride) as u64;
                        self.write_bytes(res, *dst, d, &plane[s..s + row])?;
                    }
                    i += 1;
                }
                Enc::CopyBufferToTexture { src, src_offset, bytes_per_row, dst, mip, width, height } => {
                    let (bpt, dst_depth) = {
                        let t = texture::native(res, *dst)?;
                        (texel_bytes(t.format)? as u32, t.depth)
                    };
                    let row = (*width * bpt) as usize;
                    // `bytes_per_row == 0` means the source rows are tightly packed (the oracle convention).
                    let src_stride = if *bytes_per_row == 0 { row } else { *bytes_per_row as usize };
                    // A 3D destination has no z/depth field on this op, so the copy fills the WHOLE volume:
                    // the source holds `width*height*depth` tightly-stacked slices (rows advance `height`
                    // per slice). A plain 2D texture keeps `depth == 1`, i.e. the original single-plane copy.
                    let rows = *height as usize * dst_depth as usize;
                    let mut tight = Vec::with_capacity(row * rows);
                    for r in 0..rows {
                        let off = *src_offset + (r * src_stride) as u64;
                        tight.extend_from_slice(&self.read_bytes(res, *src, off, row)?);
                    }
                    self.write_region(res, *dst, 0, 0, 0, *width, *height, dst_depth, *mip, &tight)?;
                    i += 1;
                }
                Enc::CopyTextureToTexture {
                    src,
                    src_sub,
                    src_origin,
                    dst,
                    dst_sub,
                    dst_origin,
                    extent,
                } => {
                    self.copy_texture_to_texture(
                        res, *src, src_sub, src_origin, *dst, dst_sub, dst_origin, extent,
                    )?;
                    i += 1;
                }
                Enc::FillBuffer { buffer, offset, size, value } => {
                    self.fill_buffer(res, *buffer, *offset, *size, *value)?;
                    i += 1;
                }
                // Scaled blit + multisample resolve need image resampling this backend does not implement,
                // and are NOT advertised (see `REPLAYED_COMMANDS`), so the runtime rejects them at validate.
                // Erroring here (rather than the old silent no-op) is the defensive backstop for a direct
                // executor call — a submitted-but-unimplemented op must never vanish without a trace.
                Enc::BlitTexture { .. } => {
                    hl_log::hl_warn!(tag::WGPU, "op rejected op=BlitTexture reason=unimplemented");
                    return Err(GpuError::Unsupported("wgpu: BlitTexture (scaled/filtered) unimplemented"))
                }
                Enc::ResolveTexture { .. } => {
                    hl_log::hl_warn!(tag::WGPU, "op rejected op=ResolveTexture reason=unimplemented");
                    return Err(GpuError::Unsupported("wgpu: ResolveTexture (multisample) unimplemented"))
                }
                // Stray state-setters outside a pass cannot occur in a validated command buffer.
                _ => i += 1,
            }
        }
        if let Some((f, v)) = cb.signal {
            fence::signal(res, f, v)?;
        }
        Ok(())
    }

    /// Fill `[offset, offset+size)` of buffer `id` with the repeating little-endian pattern of `value`
    /// (device memset). Read-modify-write over the 4-aligned window preserves neighbour bytes and matches
    /// the oracle's tiling (buffer byte `offset+i` takes pattern byte `i % 4`).
    fn fill_buffer(
        &self,
        res: &SessionResources,
        id: u32,
        offset: u64,
        size: u64,
        value: u32,
    ) -> Result<()> {
        if size == 0 {
            return Ok(());
        }
        let pat = value.to_le_bytes();
        let astart = offset & !3;
        let aend = (offset + size).div_ceil(4) * 4;
        let mut window = self.read_bytes(res, id, astart, (aend - astart) as usize)?;
        for p in offset..offset + size {
            window[(p - astart) as usize] = pat[((p - offset) % 4) as usize];
        }
        self.write_bytes(res, id, astart, &window)
    }

    /// Exact (no-scaling) texture→texture copy: move `extent` texels from `src`'s `src_origin` to `dst`'s
    /// `dst_origin`, CPU-mediated through the tight readback plane + region upload (mirrors the CPU oracle's
    /// `copy_texture_to_texture`). Only the base subresource (mip 0 / layer 0 / whole color aspect) of a 2D
    /// color texture is supported; anything else, a format-size mismatch, or an out-of-range region is a
    /// clean typed error rather than a panic (the runtime does not range-check this op).
    #[allow(clippy::too_many_arguments)]
    fn copy_texture_to_texture(
        &self,
        res: &SessionResources,
        src: u32,
        src_sub: &TextureSubresource,
        src_origin: &Origin3d,
        dst: u32,
        dst_sub: &TextureSubresource,
        dst_origin: &Origin3d,
        extent: &Extent3d,
    ) -> Result<()> {
        for sub in [src_sub, dst_sub] {
            if sub.mip != 0 || sub.layer != 0 || sub.aspect != TextureAspect::All {
                return Err(GpuError::Unsupported("wgpu: non-base subresource texture copy"));
            }
        }
        if src_origin.z != 0 || dst_origin.z != 0 || extent.depth > 1 {
            return Err(GpuError::Unsupported("wgpu: 3D/layer texture-to-texture copy"));
        }
        let (sw, sh, s_bpt) = {
            let t = texture::native(res, src)?;
            (t.width, t.height, texel_bytes(t.format)? as u32)
        };
        let (dw, dh, d_bpt) = {
            let t = texture::native(res, dst)?;
            (t.width, t.height, texel_bytes(t.format)? as u32)
        };
        if s_bpt != d_bpt {
            return Err(GpuError::Invalid("wgpu: texture-to-texture copy between incompatible formats"));
        }
        let (ew, eh) = (extent.width, extent.height);
        // Range guards (wrapping-safe): the source region must lie in `src`, the dest region in `dst`.
        let ok = |x: u32, y: u32, w: u32, h: u32, tw: u32, th: u32| {
            x.checked_add(w).is_some_and(|e| e <= tw) && y.checked_add(h).is_some_and(|e| e <= th)
        };
        if !ok(src_origin.x, src_origin.y, ew, eh, sw, sh)
            || !ok(dst_origin.x, dst_origin.y, ew, eh, dw, dh)
        {
            return Err(GpuError::OutOfBounds);
        }
        let bpt = s_bpt as usize;
        let sw = sw as usize;
        let plane = self.read_texture_tight(res, src)?;
        let (sx, sy) = (src_origin.x as usize, src_origin.y as usize);
        let row = ew as usize * bpt;
        let mut block = Vec::with_capacity(row * eh as usize);
        for r in 0..eh as usize {
            let start = ((sy + r) * sw + sx) * bpt;
            block.extend_from_slice(&plane[start..start + row]);
        }
        self.write_region(res, dst, dst_origin.x, dst_origin.y, 0, ew, eh, 1, 0, &block)
    }

    /// Execute one render pass: begin with the color attachments (clear/load), replay any pipeline/bind/
    /// draw ops into it, end, submit, and wait. A clear-only pass (no draws) realizes the clear.
    ///
    /// Bind groups are honored PER SET INDEX (like `run_compute_pass`): each draw binds every set the stream
    /// currently has bound, each built against THAT set's own layout on the bound pipeline
    /// (`get_bind_group_layout(index)`) and bound at its declared index. This is what lets a pipeline whose
    /// vertex/fragment read bindings in group 0 AND group 1 (Zed's GPUI draws) match — a set-1 bind group is
    /// validated against group 1's layout, not group 0's. A draw with no bind group (the bindingless
    /// conformance triangle) stays the fast path — unlike compute, an empty group list is legal.
    fn run_render_pass(
        &mut self,
        res: &SessionResources,
        color: &[ColorAttachment],
        depth: Option<&DepthAttachment>,
        ops: &[Enc],
    ) -> Result<()> {
        let _sp = hl_log::hl_span!(tag::EXEC, "submit");
        hl_log::hl_count!(tag::EXEC, "passes");
        // Resolve attachment views up front (they must outlive the pass).
        let mut views = Vec::with_capacity(color.len());
        for c in color {
            views.push((texture::native(res, c.texture)?.view.clone(), c));
        }
        // Resolve the depth attachment's view + load/clear (it too must outlive the pass). A pipeline
        // built with a depth-stencil state MUST run in a pass with a matching depth attachment (wgpu
        // enforces this), so honoring the attachment here is what makes depth-tested draws real instead
        // of silently dropping the depth field.
        let depth_view = match depth {
            Some(d) => Some((texture::native(res, d.texture)?.view.clone(), d.load, d.clear_depth)),
            None => None,
        };
        // Pre-build the bind groups every draw in this pass needs, keyed to the pipeline it's drawn with.
        // Bind-group binding is tracked PER SET INDEX (mirroring `run_compute_pass`): a draw builds a
        // concrete bind group for EVERY set index currently bound, each against THAT set's own layout on the
        // bound pipeline (`get_bind_group_layout(index)`) and bound at that index below. Sized to the
        // advertised `max_bind_groups` (4); a higher index would have been rejected by the runtime.
        let mut cur_pipeline: Option<u32> = None;
        let mut cur_groups: [Option<u32>; 4] = [None; 4];
        // (pipeline id, [(set index, bind group)]) per Draw, in order. An empty group list is the bindingless
        // fast path (e.g. the conformance triangle) — unlike compute, a draw with no bind group is legal.
        #[allow(clippy::type_complexity)]
        let mut draws: Vec<(u32, Vec<(u32, wgpu::BindGroup)>)> = Vec::new();
        for op in ops {
            match op {
                Enc::SetPipeline(p) => cur_pipeline = Some(*p),
                Enc::SetBindGroup { index, group } => {
                    let slot = *index as usize;
                    if slot >= cur_groups.len() {
                        return Err(GpuError::Invalid("wgpu: render bind-group index out of range"));
                    }
                    cur_groups[slot] = Some(*group);
                }
                Enc::Draw { .. } | Enc::DrawIndexed { .. } => {
                    hl_log::hl_count!(tag::EXEC, "draws");
                    let pid = cur_pipeline.ok_or(GpuError::Invalid("draw with no pipeline bound"))?;
                    let mut groups = Vec::new();
                    for (idx, bound) in cur_groups.iter().enumerate() {
                        if let Some(g) = bound {
                            let (layout, filter) = match pipeline::native(res, pid)? {
                                PipelineNative::Render { pipeline, used_bindings, .. } => {
                                    (pipeline.get_bind_group_layout(idx as u32), used_bindings.as_slice())
                                }
                                PipelineNative::Compute { .. } => {
                                    return Err(GpuError::Unsupported("wgpu: draw on a compute pipeline"))
                                }
                            };
                            // Filter the driver's bind-group entries to the bindings this pipeline's shaders
                            // actually read in THIS group (the filter keys on the bind group's own `set`, so it
                            // restricts to this set's slots), so a GskGpu bind group that carries an unsampled
                            // texture/sampler pair still matches (see `bindgroup::build_bind_group`).
                            let bg =
                                self.build_bind_group(res, &layout, bindgroup::desc(res, *g)?, Some(filter))?;
                            groups.push((idx as u32, bg));
                        }
                    }
                    draws.push((pid, groups));
                }
                _ => {}
            }
        }

        let mut enc =
            self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let attachments: Vec<Option<wgpu::RenderPassColorAttachment>> = views
                .iter()
                .map(|(view, c)| {
                    Some(wgpu::RenderPassColorAttachment {
                        view,
                        resolve_target: None,
                        ops: wgpu::Operations {
                            load: match c.load {
                                LoadOp::Clear => wgpu::LoadOp::Clear(wgpu::Color {
                                    r: c.clear[0] as f64,
                                    g: c.clear[1] as f64,
                                    b: c.clear[2] as f64,
                                    a: c.clear[3] as f64,
                                }),
                                _ => wgpu::LoadOp::Load,
                            },
                            store: wgpu::StoreOp::Store,
                        },
                    })
                })
                .collect();
            let mut pass = enc.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("hl-render-pass"),
                color_attachments: &attachments,
                depth_stencil_attachment: depth_view.as_ref().map(|(view, load, clear)| {
                    wgpu::RenderPassDepthStencilAttachment {
                        view,
                        depth_ops: Some(wgpu::Operations {
                            load: match load {
                                LoadOp::Clear => wgpu::LoadOp::Clear(*clear),
                                _ => wgpu::LoadOp::Load,
                            },
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }
                }),
                timestamp_writes: None,
                occlusion_query_set: None,
            });
            let mut di = 0usize;
            for op in ops {
                match op {
                    Enc::SetViewport { x, y, w, h, min_depth, max_depth } => {
                        pass.set_viewport(*x, *y, *w, *h, *min_depth, *max_depth);
                    }
                    Enc::SetScissor { x, y, w, h } => pass.set_scissor_rect(*x, *y, *w, *h),
                    Enc::SetVertexBuffer { slot, buffer, offset } => {
                        let vb = buffer::native(res, *buffer)?;
                        pass.set_vertex_buffer(*slot, vb.buffer.slice(*offset..));
                    }
                    Enc::SetIndexBuffer { buffer, offset, format } => {
                        let ib = buffer::native(res, *buffer)?;
                        pass.set_index_buffer(ib.buffer.slice(*offset..), index_format(*format));
                    }
                    Enc::Draw { vertex_count, instance_count, first_vertex, first_instance } => {
                        let (pid, groups) = &draws[di];
                        di += 1;
                        if let PipelineNative::Render { pipeline, .. } = pipeline::native(res, *pid)? {
                            pass.set_pipeline(pipeline);
                        }
                        for (idx, bg) in groups {
                            pass.set_bind_group(*idx, bg, &[]);
                        }
                        pass.draw(
                            *first_vertex..*first_vertex + *vertex_count,
                            *first_instance..*first_instance + *instance_count,
                        );
                    }
                    Enc::DrawIndexed {
                        index_count,
                        instance_count,
                        first_index,
                        base_vertex,
                        first_instance,
                    } => {
                        let (pid, groups) = &draws[di];
                        di += 1;
                        if let PipelineNative::Render { pipeline, .. } = pipeline::native(res, *pid)? {
                            pass.set_pipeline(pipeline);
                        }
                        for (idx, bg) in groups {
                            pass.set_bind_group(*idx, bg, &[]);
                        }
                        pass.draw_indexed(
                            *first_index..*first_index + *index_count,
                            *base_vertex,
                            *first_instance..*first_instance + *instance_count,
                        );
                    }
                    _ => {}
                }
            }
        }
        self.gpu.queue.submit(Some(enc.finish()));
        self.gpu.device.poll(wgpu::Maintain::Wait);
        Ok(())
    }

    /// Execute one compute pass: for each dispatch, build a concrete bind group for every set index the
    /// stream currently has bound, each against THAT group's layout on the bound pipeline
    /// (`get_bind_group_layout(index)`), then replay, submit, and wait.
    ///
    /// Unlike the earlier single-group path this honors `SetBindGroup.index`, so a SPIR-V compute pipeline
    /// with 2+ bind groups (e.g. wgpu-core's own indirect-draw-validation pipeline: group 0 the params,
    /// group 1 the indirect buffers) binds each group at its declared set. Dynamic offsets are already
    /// baked into each `BindResource::Buffer.offset` by the shim (the protocol's `SetBindGroup` carries no
    /// separate offset list, and an auto layout reflects no dynamic-offset binding), so the wgpu offsets
    /// slice is empty. Push constants are likewise not a protocol op — a compute pipeline that declares a
    /// `var<push_constant>` needs it only to CREATE (that is the wgpu-core validation pipeline Zed builds
    /// during device creation, which is never dispatched through this executor); reading push constants at
    /// a dispatch that the protocol never wrote is not reachable here.
    fn run_compute_pass(&mut self, res: &SessionResources, ops: &[Enc]) -> Result<()> {
        let _sp = hl_log::hl_span!(tag::EXEC, "submit");
        hl_log::hl_count!(tag::EXEC, "passes");
        // Current bind-group binding per set index. Sized to the advertised `max_bind_groups` (4); a higher
        // index would have been rejected by the runtime before reaching here.
        let mut cur_pipeline: Option<u32> = None;
        let mut cur_groups: [Option<u32>; 4] = [None; 4];
        // (pipeline id, [(set index, bind group)], (x,y,z)) per Dispatch.
        #[allow(clippy::type_complexity)]
        let mut dispatches: Vec<(u32, Vec<(u32, wgpu::BindGroup)>, (u32, u32, u32))> = Vec::new();
        for op in ops {
            match op {
                Enc::SetPipeline(p) => cur_pipeline = Some(*p),
                Enc::SetBindGroup { index, group } => {
                    let slot = *index as usize;
                    if slot >= cur_groups.len() {
                        return Err(GpuError::Invalid("wgpu: compute bind-group index out of range"));
                    }
                    cur_groups[slot] = Some(*group);
                }
                Enc::Dispatch { x, y, z } => {
                    hl_log::hl_count!(tag::EXEC, "dispatches");
                    let pid = cur_pipeline.ok_or(GpuError::Invalid("dispatch with no pipeline"))?;
                    if let PipelineNative::Render { .. } = pipeline::native(res, pid)? {
                        return Err(GpuError::Unsupported("wgpu: dispatch on a render pipeline"));
                    }
                    let mut groups = Vec::new();
                    for (idx, bound) in cur_groups.iter().enumerate() {
                        if let Some(g) = bound {
                            let layout = match pipeline::native(res, pid)? {
                                PipelineNative::Compute { pipeline } => {
                                    pipeline.get_bind_group_layout(idx as u32)
                                }
                                PipelineNative::Render { .. } => unreachable!("checked above"),
                            };
                            // Compute bind groups already match their layout (the kernel path's explicit
                            // layout / the SPIR-V path's auto layout are built to), so no filter is applied.
                            let bg = self.build_bind_group(res, &layout, bindgroup::desc(res, *g)?, None)?;
                            groups.push((idx as u32, bg));
                        }
                    }
                    if groups.is_empty() {
                        return Err(GpuError::Invalid("dispatch with no bind group"));
                    }
                    dispatches.push((pid, groups, (*x, *y, *z)));
                }
                _ => {}
            }
        }

        let mut enc =
            self.gpu.device.create_command_encoder(&wgpu::CommandEncoderDescriptor { label: None });
        {
            let mut pass = enc.begin_compute_pass(&wgpu::ComputePassDescriptor {
                label: Some("hl-compute-pass"),
                timestamp_writes: None,
            });
            for (pid, groups, (x, y, z)) in &dispatches {
                if let PipelineNative::Compute { pipeline } = pipeline::native(res, *pid)? {
                    pass.set_pipeline(pipeline);
                }
                for (idx, bg) in groups {
                    pass.set_bind_group(*idx, bg, &[]);
                }
                pass.dispatch_workgroups(*x, *y, *z);
            }
        }
        self.gpu.queue.submit(Some(enc.finish()));
        self.gpu.device.poll(wgpu::Maintain::Wait);
        Ok(())
    }
}

/// Find the encoder index of the pass-closing op matching the `Begin` at `start`, rejecting an unclosed
/// or nested pass (a validated command buffer never nests, but stay defensive).
fn find_end(ops: &[Enc], start: usize, close: Enc) -> Result<usize> {
    for (k, op) in ops.iter().enumerate().skip(start + 1) {
        if std::mem::discriminant(op) == std::mem::discriminant(&close) {
            return Ok(k);
        }
    }
    Err(GpuError::Invalid("command buffer ends inside an open pass"))
}

#[cfg(test)]
mod multi_group_render_proof {
    //! The Zed multi-bind-group unblock, FAIL-before / PASS-after in one test.
    //!
    //! Zed's GPUI/wgpu renderer draws with a render pipeline whose VERTEX reads a uniform in group 0 and
    //! whose FRAGMENT samples a texture+sampler in group **1** (two distinct bind-group SET INDICES). It
    //! binds a set-0 bind group (the uniform) AND a set-1 bind group (the texture+sampler), then draws.
    //!
    //! The OLD `run_render_pass` was single-group: it tracked only the LAST `SetBindGroup`, built every bind
    //! group against `pipeline.get_bind_group_layout(0)`, and bound it at slot 0 — so the set-1 bind group
    //! (2 entries: texture+sampler) was validated against GROUP 0's layout (1 uniform buffer). wgpu rejects
    //! that with "Number of bindings (…) does not match the bind group layout (…)"; the uncaptured device
    //! error marked Zed's device lost. `run_render_pass` now tracks a pending bind group PER set index and
    //! builds each against THAT set's own layout (`get_bind_group_layout(index)`), binding it at its index —
    //! mirroring `run_compute_pass` — so the set-1 group validates against group 1's layout and the draw
    //! samples the group-1 texture. This test asserts both halves against the SAME built pipeline: the
    //! group-0-layout binding of the set-1 descriptor errors (the old bug), and the full two-group draw reads
    //! back the sampled texel (the fix).

    use hl_gpu::protocol::model::descriptor::{
        BindEntry, BindGroupDesc, BindResource, BufferDesc, ColorAttachment, ColorTargetState,
        RenderPipelineDesc, SamplerDesc, ShaderRef, TextureDesc,
    };
    use hl_gpu::protocol::model::enums::{
        buffer_usage, texture_usage, AddressMode, Filter, LoadOp, TextureDim, TextureFormat, Topology,
    };
    use hl_gpu::protocol::model::kernel::{glsl_stage, GlslDescriptor};
    use hl_gpu::{
        Cmd, CommandBuffer, Enc, FakeClock, GlobalLedger, GpuExecutor, Limits, Session,
        ShaderPayloadKind,
    };

    use crate::pipeline::PipelineNative;
    use crate::{bindgroup, pipeline, DeviceConfig, WgpuExecutor};

    // Vertex reads the group-0 uniform (its `.w` scales the clip position → 1.0 = identity), emits a
    // fullscreen triangle with a constant uv.
    const VS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U0 { vec4 scale; } u0;
layout(location = 0) out vec2 uv;
void main() {
    vec2 p[3] = vec2[3](vec2(-1.0, -1.0), vec2(3.0, -1.0), vec2(-1.0, 3.0));
    uv = vec2(0.5, 0.5);
    gl_Position = vec4(p[gl_VertexIndex], 0.0, u0.scale.w);
}
"#;

    // Fragment samples the group-1 texture through the group-1 sampler and multiplies by the group-0
    // uniform (so the pipeline genuinely reads BOTH sets: group 0 in vertex+fragment, group 1 in fragment).
    const FS: &str = r#"#version 460
layout(std140, set = 0, binding = 0) uniform U0 { vec4 scale; } u0;
layout(set = 1, binding = 0) uniform texture2D t0_tex;
layout(set = 1, binding = 1) uniform sampler   t0_smp;
layout(location = 0) in vec2 uv;
layout(location = 0) out vec4 color;
void main() {
    color = texture(sampler2D(t0_tex, t0_smp), uv) * u0.scale;
}
"#;

    fn glsl(stage: u32, entry: &str, source: &str) -> Vec<u32> {
        GlslDescriptor { stage, entry: entry.to_string(), source: source.to_string() }.to_words()
    }

    fn tex(w: u32, h: u32, usage: u32) -> TextureDesc {
        TextureDesc {
            width: w,
            height: h,
            depth: 1,
            mip_levels: 1,
            sample_count: 1,
            dim: TextureDim::D2,
            format: TextureFormat::Rgba8Unorm,
            usage,
            label: String::new(),
        }
    }

    fn nearest() -> SamplerDesc {
        SamplerDesc {
            min_filter: Filter::Nearest,
            mag_filter: Filter::Nearest,
            mip_filter: Filter::Nearest,
            address_u: AddressMode::ClampToEdge,
            address_v: AddressMode::ClampToEdge,
            address_w: AddressMode::ClampToEdge,
        }
    }

    #[test]
    fn set_index_one_binds_against_its_own_group_layout() {
        let mut exec = match WgpuExecutor::new(DeviceConfig::default()) {
            Ok(e) => e,
            // No adapter (no lavapipe/Vulkan ICD reachable) — skip, mirroring the suite's other gpu tests.
            Err(_) => return,
        };

        let texel: [u8; 4] = [30, 150, 220, 255]; // the group-1 texture's single texel
        let scale: [f32; 4] = [1.0, 1.0, 1.0, 1.0]; // group-0 uniform: identity (passthrough of the texel)

        let caps = exec.capabilities();
        let mut limits = Limits::from_capabilities(caps);
        limits.copy_alignment = 1;
        let mut s = Session::new(limits, GlobalLedger::unbounded(), Box::new(FakeClock::new(0)));

        // Create resources, the two-group pipeline, and both bind groups (set 0 = the uniform; set 1 = the
        // texture+sampler). No draw yet — this populates `s.resources` so the FAIL-before check below can
        // reach the built pipeline's per-group layouts.
        hl_gpu::runtime::submit(
            &mut s,
            &mut exec,
            0,
            &[
                Cmd::CreateTexture(1, tex(4, 4, texture_usage::RENDER_TARGET | texture_usage::COPY_SRC)),
                Cmd::CreateTexture(2, tex(1, 1, texture_usage::SAMPLED | texture_usage::COPY_DST)),
                Cmd::CreateBuffer(1, BufferDesc { size: 16, usage: buffer_usage::UNIFORM, label: String::new() }),
                Cmd::WriteBuffer { id: 1, offset: 0, data: scale.iter().flat_map(|f| f.to_le_bytes()).collect() },
                Cmd::CreateBuffer(2, BufferDesc { size: 4, usage: buffer_usage::COPY_SRC, label: String::new() }),
                Cmd::WriteBuffer { id: 2, offset: 0, data: texel.to_vec() },
                Cmd::CreateShader { id: 1, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::VERTEX, "vmain", VS) },
                Cmd::CreateShader { id: 2, kind: ShaderPayloadKind::Glsl, spirv: glsl(glsl_stage::FRAGMENT, "fmain", FS) },
                Cmd::CreateSampler(1, nearest()),
                Cmd::CreateRenderPipeline(
                    1,
                    RenderPipelineDesc {
                        vertex: ShaderRef { module: 1, entry: "vmain".into() },
                        fragment: Some(ShaderRef { module: 2, entry: "fmain".into() }),
                        vertex_buffers: vec![],
                        color_targets: vec![ColorTargetState { format: TextureFormat::Rgba8Unorm, blend: None, write_mask: 0xF }],
                        depth: None,
                        topology: Topology::TriangleList,
                        cull: 0,
                        front_face: 0,
                        label: String::new(),
                    },
                ),
                // Bind group 1 = SET 0: the uniform buffer (1 entry).
                Cmd::CreateBindGroup(
                    1,
                    BindGroupDesc {
                        set: 0,
                        entries: vec![
                            BindEntry { binding: 0, resource: BindResource::Buffer { id: 1, offset: 0, size: 16 } },
                        ],
                    },
                ),
                // Bind group 2 = SET 1: the texture + sampler (2 entries).
                Cmd::CreateBindGroup(
                    2,
                    BindGroupDesc {
                        set: 1,
                        entries: vec![
                            BindEntry { binding: 0, resource: BindResource::Texture { id: 2 } },
                            BindEntry { binding: 1, resource: BindResource::Sampler { id: 1 } },
                        ],
                    },
                ),
            ],
        )
        .expect("the two-group pipeline + both bind groups must create cleanly");

        // FAIL-BEFORE: the OLD single-group path built EVERY bind group against group 0's layout and bound it
        // at slot 0. Reproduce that for the SET-1 bind group (id 2): built against `get_bind_group_layout(0)`
        // its 2 texture/sampler entries do NOT match group 0's single uniform-buffer binding — the exact
        // wgpu validation error that marked Zed's device lost. (The new path builds it against group 1's
        // layout instead, which the passing draw below proves.)
        let (layout0, filter) = match pipeline::native(&s.resources, 1).unwrap() {
            PipelineNative::Render { pipeline, used_bindings, .. } => {
                (pipeline.get_bind_group_layout(0), used_bindings.clone())
            }
            PipelineNative::Compute { .. } => unreachable!("pipeline 1 is a render pipeline"),
        };
        exec.gpu.device.push_error_scope(wgpu::ErrorFilter::Validation);
        let _bad = exec
            .build_bind_group(&s.resources, &layout0, bindgroup::desc(&s.resources, 2).unwrap(), Some(&filter))
            .expect("building the descriptor does not itself return Err (the error surfaces via the scope)");
        let err = pollster::block_on(exec.gpu.device.pop_error_scope()).expect(
            "validating the SET-1 bind group against GROUP 0's layout MUST error — if it did not, this test \
             no longer reproduces the single-group bug that lost Zed its device",
        );
        assert!(
            err.to_string().to_lowercase().contains("bind"),
            "the old-path failure must be a bind-group/layout mismatch, got: {err}"
        );

        // PASS-AFTER: the full two-group draw. `run_render_pass` binds set 0 against group 0's layout and set
        // 1 against group 1's layout, at their declared indices, and samples the group-1 texture.
        hl_gpu::runtime::submit(
            &mut s,
            &mut exec,
            0,
            &[Cmd::Submit(CommandBuffer {
                encoder: vec![
                    Enc::CopyBufferToTexture { src: 2, src_offset: 0, bytes_per_row: 4, dst: 2, mip: 0, width: 1, height: 1 },
                    Enc::BeginRenderPass {
                        color: vec![ColorAttachment { texture: 1, load: LoadOp::Clear, clear: [0.0, 0.0, 0.0, 1.0], store: true }],
                        depth: None,
                    },
                    Enc::SetPipeline(1),
                    Enc::SetBindGroup { index: 0, group: 1 }, // set 0 → the uniform
                    Enc::SetBindGroup { index: 1, group: 2 }, // set 1 → the texture + sampler
                    Enc::Draw { vertex_count: 3, instance_count: 1, first_vertex: 0, first_instance: 0 },
                    Enc::EndRenderPass,
                ],
                signal: None,
            })],
        )
        .expect(
            "the two-group draw must run: set 1 validated against GROUP 1's layout (not group 0), the exact \
             multi-bind-group draw the old single-group path could not honor",
        );

        let px = exec.read_texture(&s.resources, 1).unwrap();
        for (i, out) in px.chunks_exact(4).enumerate() {
            assert_eq!(
                out, texel,
                "pixel {i}: must be the group-1 texture's texel {texel:?} (group-0 scale is identity), \
                 proving the set-1 bind group matched GROUP 1's layout and the draw sampled it"
            );
        }
    }
}
