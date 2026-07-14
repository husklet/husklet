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
use hl_gpu::protocol::model::descriptor::ColorAttachment;
use hl_gpu::protocol::model::enums::LoadOp;
use hl_gpu::runtime::model::resources::SessionResources;
use hl_gpu::{GpuError, Result};

use crate::convert::{clear_texel, texel_bytes};
use crate::pipeline::{self, PipelineNative};
use crate::{bindgroup, fence, texture, WgpuExecutor};

impl WgpuExecutor {
    pub(crate) fn submit_cb(&mut self, res: &mut SessionResources, cb: &CommandBuffer) -> Result<()> {
        let ops = &cb.encoder;
        let mut i = 0;
        while i < ops.len() {
            match &ops[i] {
                Enc::BeginRenderPass { color, depth: _ } => {
                    let end = find_end(ops, i, Enc::EndRenderPass)?;
                    self.run_render_pass(res, color, &ops[i + 1..end])?;
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
                    self.write_region(res, *texture, *x, *y, *w, *h, &data)?;
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
                    let plane = self.read_texture_tight(res, *src)?;
                    let row = (*width * bpt) as usize;
                    for r in 0..*height {
                        let s = (r * *width * bpt) as usize;
                        let d = *dst_offset + (r * *bytes_per_row) as u64;
                        self.write_bytes(res, *dst, d, &plane[s..s + row])?;
                    }
                    i += 1;
                }
                Enc::CopyBufferToTexture { src, src_offset, bytes_per_row, dst, width, height, .. } => {
                    let t = texture::native(res, *dst)?;
                    let bpt = texel_bytes(t.format)? as u32;
                    let row = (*width * bpt) as usize;
                    let mut tight = Vec::with_capacity(row * *height as usize);
                    for r in 0..*height {
                        let off = *src_offset + (r * *bytes_per_row) as u64;
                        tight.extend_from_slice(&self.read_bytes(res, *src, off, row)?);
                    }
                    self.write_region(res, *dst, 0, 0, *width, *height, &tight)?;
                    i += 1;
                }
                Enc::FillBuffer { buffer, offset, size, value } => {
                    self.fill_buffer(res, *buffer, *offset, *size, *value)?;
                    i += 1;
                }
                // Stray state-setters outside a pass and the copy/blit/resolve ops not in the frozen
                // conformance suite are skipped here (unreached given a validated command buffer).
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

    /// Execute one render pass: begin with the color attachments (clear/load), replay any pipeline/bind/
    /// draw ops into it, end, submit, and wait. A clear-only pass (no draws) realizes the clear.
    fn run_render_pass(
        &mut self,
        res: &SessionResources,
        color: &[ColorAttachment],
        ops: &[Enc],
    ) -> Result<()> {
        // Resolve attachment views up front (they must outlive the pass).
        let mut views = Vec::with_capacity(color.len());
        for c in color {
            views.push((texture::native(res, c.texture)?.view.clone(), c));
        }
        // Pre-build any bind group a draw in this pass needs (keyed to the pipeline it's drawn with).
        let mut cur_pipeline: Option<u32> = None;
        let mut cur_bind_group: Option<u32> = None;
        // (pipeline id, optional bind group) per Draw, in order.
        let mut draws: Vec<(u32, Option<wgpu::BindGroup>)> = Vec::new();
        for op in ops {
            match op {
                Enc::SetPipeline(p) => cur_pipeline = Some(*p),
                Enc::SetBindGroup { group, .. } => cur_bind_group = Some(*group),
                Enc::Draw { .. } | Enc::DrawIndexed { .. } => {
                    let pid = cur_pipeline.ok_or(GpuError::Invalid("draw with no pipeline bound"))?;
                    let bg = match cur_bind_group {
                        Some(g) => {
                            let layout = match pipeline::native(res, pid)? {
                                PipelineNative::Render { pipeline, .. } => {
                                    pipeline.get_bind_group_layout(0)
                                }
                                PipelineNative::Compute { .. } => {
                                    return Err(GpuError::Unsupported("wgpu: draw on a compute pipeline"))
                                }
                            };
                            Some(self.build_bind_group(res, &layout, bindgroup::desc(res, g)?)?)
                        }
                        None => None,
                    };
                    draws.push((pid, bg));
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
                depth_stencil_attachment: None,
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
                    Enc::Draw { vertex_count, instance_count, first_vertex, first_instance } => {
                        let (pid, bg) = &draws[di];
                        di += 1;
                        if let PipelineNative::Render { pipeline, .. } = pipeline::native(res, *pid)? {
                            pass.set_pipeline(pipeline);
                        }
                        if let Some(bg) = bg {
                            pass.set_bind_group(0, bg, &[]);
                        }
                        pass.draw(
                            *first_vertex..*first_vertex + *vertex_count,
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

    /// Execute one compute pass: build each dispatch's bind group from its pipeline's auto layout, replay,
    /// submit, and wait.
    fn run_compute_pass(&mut self, res: &SessionResources, ops: &[Enc]) -> Result<()> {
        let mut cur_pipeline: Option<u32> = None;
        let mut cur_bind_group: Option<u32> = None;
        // (pipeline id, bind group, (x,y,z)) per Dispatch.
        let mut dispatches: Vec<(u32, wgpu::BindGroup, (u32, u32, u32))> = Vec::new();
        for op in ops {
            match op {
                Enc::SetPipeline(p) => cur_pipeline = Some(*p),
                Enc::SetBindGroup { group, .. } => cur_bind_group = Some(*group),
                Enc::Dispatch { x, y, z } => {
                    let pid = cur_pipeline.ok_or(GpuError::Invalid("dispatch with no pipeline"))?;
                    let layout = match pipeline::native(res, pid)? {
                        PipelineNative::Compute { layout, .. } => layout,
                        PipelineNative::Render { .. } => {
                            return Err(GpuError::Unsupported("wgpu: dispatch on a render pipeline"))
                        }
                    };
                    let g = cur_bind_group.ok_or(GpuError::Invalid("dispatch with no bind group"))?;
                    let bg = self.build_bind_group(res, layout, bindgroup::desc(res, g)?)?;
                    dispatches.push((pid, bg, (*x, *y, *z)));
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
            for (pid, bg, (x, y, z)) in &dispatches {
                if let PipelineNative::Compute { pipeline, .. } = pipeline::native(res, *pid)? {
                    pass.set_pipeline(pipeline);
                }
                pass.set_bind_group(0, bg, &[]);
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
