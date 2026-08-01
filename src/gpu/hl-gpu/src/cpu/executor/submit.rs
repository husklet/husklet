use super::validation::EncoderState;
use super::*;

impl CpuExecutor {
    pub(super) fn write_buffer(
        &self,
        res: &mut SessionResources,
        id: u32,
        offset: u64,
        data: &[u8],
    ) -> Result<()> {
        let b = buffer_mut(res, id)?;
        let off = offset as usize;
        let end = offset
            .checked_add(data.len() as u64)
            .filter(|e| *e <= b.data.len() as u64)
            .ok_or(GpuError::OutOfBounds)? as usize;
        b.data[off..end].copy_from_slice(data);
        Ok(())
    }

    /// Execute a `FillBuffer`: write the repeating little-endian 4-byte pattern of `value` over
    /// `[offset, offset+size)`. A device-side memset — the pattern tiles from `offset` (byte `i` of the
    /// region takes pattern byte `i % 4`), so a `size` that is not a multiple of 4 fills a partial pattern
    /// at the tail. Bounds are re-checked here (submit-time validation already verified them).
    pub(super) fn fill_buffer(
        &self,
        res: &mut SessionResources,
        id: u32,
        offset: u64,
        size: u64,
        value: u32,
    ) -> Result<()> {
        let b = buffer_mut(res, id)?;
        let start = offset as usize;
        let end = offset
            .checked_add(size)
            .filter(|e| *e <= b.data.len() as u64)
            .ok_or(GpuError::OutOfBounds)? as usize;
        let pat = value.to_le_bytes();
        for (i, byte) in b.data[start..end].iter_mut().enumerate() {
            *byte = pat[i % 4];
        }
        Ok(())
    }

    pub(super) fn present(
        &self,
        res: &SessionResources,
        surface_id: u32,
        texture_id: u32,
        serial: crate::FrameSerial,
    ) -> Result<Presentation> {
        let sdesc = surface(res, surface_id)?.clone();
        let t = texture(res, texture_id)?;
        if t.desc.sample_count != 1 {
            return Err(GpuError::Unsupported(
                "software: present multisample texture",
            ));
        }
        if t.desc.width != sdesc.width || t.desc.height != sdesc.height {
            return Err(GpuError::Invalid(
                "present texture size does not match surface",
            ));
        }
        Ok(Presentation {
            surface: SurfaceId(surface_id),
            token: sdesc.token,
            texture: TextureId(texture_id),
            serial,
        })
    }

    /// Validate the whole command buffer (failure atomicity), then execute its clears/copies/draws/
    /// dispatches. Ported from `SoftwareBackend::submit`.
    pub(super) fn submit(&mut self, res: &mut SessionResources, cb: &CommandBuffer) -> Result<()> {
        let _span = hl_log::hl_span!(hl_log::tag::CPU, "submit");
        hl_log::hl_count!(hl_log::tag::CPU, "submits");
        EncoderState::validate(res, cb)?;

        let mut cur_pipeline: Option<u32> = None;
        let mut cur_bind_group: Option<u32> = None;
        let mut cur_targets: Vec<(u32, TextureFormat)> = Vec::new();
        let mut cur_depth: Option<u32> = None;
        let mut cur_vertex: HashMap<u32, (u32, u64)> = HashMap::new();
        let mut cur_index: Option<(u32, u64, IndexFormat)> = None;
        // The dynamic stencil reference value (WebGPU `setStencilReference`). It resets to 0 per render pass
        // (mirroring the wgpu executor, whose reference is pass-scoped state) and the stream's
        // `SetStencilReference` ops update it for the draws that follow. The pipeline's `DepthState` carries
        // the static stencil compare/ops/masks; this is the one dynamic operand the compare tests against
        // and a `REPLACE` op writes.
        let mut cur_stencil_ref: u32 = 0;
        // The viewport/scissor rectangles in force at a draw. Pass-scoped, exactly as in wgpu: a new
        // render pass starts with neither bound (full-target viewport, no scissor).
        let mut cur_rect = raster::PassRect::default();
        for op in &cb.encoder {
            match op {
                Enc::BeginRenderPass { color, depth } => {
                    cur_targets.clear();
                    cur_stencil_ref = 0;
                    cur_rect = raster::PassRect::default();
                    for c in color {
                        let fmt = texture(res, c.texture)?.desc.format;
                        cur_targets.push((c.texture, fmt));
                        if c.load == LoadOp::Clear {
                            raster::clear_target(res, c.texture, c.clear)?;
                        }
                    }
                    cur_depth = None;
                    if let Some(dp) = depth {
                        cur_depth = Some(dp.texture);
                        if dp.load == LoadOp::Clear {
                            // Clears both the depth plane (to `clear_depth`) and, for a combined
                            // depth+stencil attachment, the stencil plane (to `clear_stencil`); a
                            // `LoadOp::Load` preserves both, which a two-pass mark-then-test IR relies on.
                            raster::clear_depth_stencil_target(
                                res,
                                dp.texture,
                                dp.clear_depth,
                                dp.clear_stencil,
                            )?;
                        }
                    }
                }
                Enc::EndRenderPass => {
                    cur_targets.clear();
                    cur_depth = None;
                }
                // Only the base subresource reaches here, because this oracle materializes exactly one
                // plane per LAYER and nowhere to write a non-zero mip. The refusal is in THIS executor's
                // own encoder pre-pass (`operation::validate_op`, run over every command by
                // `EncoderState::validate` before any of them executes) — NOT in the runtime's `validate`
                // service, which this comment used to name and which has no such check. Two different
                // things are called "validate" here, and sending a reader to the one without the guard is
                // how a redundant check gets added, or the real one removed as unnecessary.
                //
                // The LAYER RANGE is passed through rather than dropped: it was `base_array_layer: _`
                // while validation refused anything but the base layer, which was consistent then and
                // would silently clear the wrong layer now that a range is legal.
                Enc::ClearRect {
                    texture,
                    x,
                    y,
                    w,
                    h,
                    color,
                    base_array_layer,
                    layer_count,
                    mip_level,
                } => {
                    raster::clear_rect(
                        res,
                        *texture,
                        *x,
                        *y,
                        *w,
                        *h,
                        *color,
                        *base_array_layer,
                        *layer_count,
                        *mip_level,
                    )?;
                }
                Enc::SetViewport { x, y, w, h, .. } => {
                    cur_rect.viewport = Some([*x, *y, *w, *h]);
                }
                Enc::SetScissor { x, y, w, h } => {
                    cur_rect.scissor = Some([*x, *y, *w, *h]);
                }
                Enc::SetPipeline(p) => cur_pipeline = Some(*p),
                Enc::SetStencilReference { reference } => cur_stencil_ref = *reference,
                Enc::SetBlendConstant { .. } => {}
                Enc::SetBindGroup { group, .. } => cur_bind_group = Some(*group),
                Enc::SetVertexBuffer {
                    slot,
                    buffer,
                    offset,
                } => {
                    cur_vertex.insert(*slot, (*buffer, *offset));
                }
                Enc::SetIndexBuffer {
                    buffer,
                    offset,
                    format,
                } => {
                    cur_index = Some((*buffer, *offset, *format));
                }
                Enc::Draw {
                    vertex_count,
                    first_vertex,
                    instance_count,
                    ..
                } => {
                    self.draws += 1;
                    hl_log::hl_count!(hl_log::tag::CPU, "draws");
                    let vb = cur_vertex.get(&0).copied();
                    raster::exec_draw(
                        res,
                        cur_pipeline,
                        &cur_targets,
                        cur_depth,
                        vb,
                        *first_vertex,
                        *vertex_count,
                        *instance_count,
                        cur_stencil_ref,
                        cur_rect,
                    )?;
                }
                Enc::DrawIndexed {
                    index_count,
                    first_index,
                    base_vertex,
                    instance_count,
                    ..
                } => {
                    self.draws += 1;
                    hl_log::hl_count!(hl_log::tag::CPU, "draws");
                    let vb = cur_vertex.get(&0).copied();
                    raster::exec_draw_indexed(
                        res,
                        cur_pipeline,
                        &cur_targets,
                        cur_depth,
                        vb,
                        cur_index,
                        *first_index,
                        *index_count,
                        *base_vertex,
                        *instance_count,
                        cur_stencil_ref,
                        cur_rect,
                    )?;
                }
                Enc::Dispatch { x, y, z } => {
                    self.dispatches += 1;
                    hl_log::hl_count!(hl_log::tag::CPU, "dispatches");
                    run_dispatch(res, cur_pipeline, cur_bind_group, (*x, *y, *z))?;
                }
                Enc::CopyBufferToBuffer {
                    src,
                    src_offset,
                    dst,
                    dst_offset,
                    size,
                } => {
                    copy::copy_buffer_to_buffer(res, *src, *src_offset, *dst, *dst_offset, *size)?;
                }
                Enc::CopyBufferToTexture {
                    src,
                    src_offset,
                    bytes_per_row,
                    dst,
                    width,
                    height,
                    mip,
                } => {
                    copy::copy_buffer_to_texture(
                        res,
                        *src,
                        *src_offset,
                        *bytes_per_row,
                        *dst,
                        *width,
                        *height,
                        *mip,
                    )?;
                }
                Enc::CopyTextureToBuffer {
                    src,
                    mip,
                    width,
                    height,
                    dst,
                    dst_offset,
                    bytes_per_row,
                } => {
                    copy::copy_texture_to_buffer(
                        res,
                        *src,
                        *width,
                        *height,
                        *dst,
                        *dst_offset,
                        *bytes_per_row,
                        *mip,
                    )?;
                }
                Enc::CopyBufferToTextureRegion {
                    src,
                    src_offset,
                    bytes_per_row,
                    rows_per_image,
                    dst,
                    dst_sub,
                    dst_origin,
                    extent,
                } => {
                    copy::copy_buffer_to_texture_region(
                        res,
                        *src,
                        *src_offset,
                        *bytes_per_row,
                        *rows_per_image,
                        *dst,
                        dst_sub,
                        dst_origin,
                        extent,
                    )?;
                }
                Enc::CopyTextureToBufferRegion {
                    src,
                    src_sub,
                    src_origin,
                    extent,
                    dst,
                    dst_offset,
                    bytes_per_row,
                    rows_per_image,
                } => {
                    copy::copy_texture_to_buffer_region(
                        res,
                        *src,
                        src_sub,
                        src_origin,
                        extent,
                        *dst,
                        *dst_offset,
                        *bytes_per_row,
                        *rows_per_image,
                    )?;
                }
                Enc::CopyTextureToTexture {
                    src,
                    src_origin,
                    dst,
                    dst_origin,
                    extent,
                    ..
                } => {
                    copy::copy_texture_to_texture(res, *src, src_origin, *dst, dst_origin, extent)?;
                }
                Enc::BlitTexture {
                    src,
                    src_origin,
                    src_extent,
                    dst,
                    dst_origin,
                    dst_extent,
                    filter,
                    mirror,
                    ..
                } => {
                    copy::blit_texture(
                        res, *src, src_origin, src_extent, *dst, dst_origin, dst_extent, *filter,
                        *mirror,
                    )?;
                }
                Enc::ResolveTexture {
                    src,
                    src_origin,
                    dst,
                    dst_origin,
                    extent,
                    ..
                } => {
                    copy::resolve_texture(res, *src, src_origin, *dst, dst_origin, extent)?;
                }
                Enc::FillBuffer {
                    buffer,
                    offset,
                    size,
                    value,
                } => {
                    self.fill_buffer(res, *buffer, *offset, *size, *value)?;
                }
                _ => {}
            }
        }
        if let Some((f, v)) = cb.signal {
            let slot = fence_mut(res, f)?;
            *slot = (*slot).max(v);
        }
        Ok(())
    }
}
