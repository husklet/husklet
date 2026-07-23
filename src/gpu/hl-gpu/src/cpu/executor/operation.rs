use super::{validation::*, *};

pub(super) fn validate_op(res: &SessionResources, op: &Enc, st: &mut EncoderState) -> Result<()> {
    match op {
        Enc::BeginRenderPass { color, depth } => {
            if st.in_render_pass || st.in_compute_pass {
                return Err(GpuError::Invalid("nested render pass"));
            }
            let mut formats = Vec::with_capacity(color.len());
            for c in color {
                let t = texture_with_usage(
                    res,
                    c.texture,
                    texture_usage::RENDER_TARGET,
                    "color attachment lacks RENDER_TARGET usage",
                )?;
                if t.desc.sample_count != 1 {
                    return Err(GpuError::Unsupported(
                        "software: multisample render attachment",
                    ));
                }
                if c.load == LoadOp::Clear {
                    t.desc.format.clear_texel(c.clear)?;
                }
                formats.push(t.desc.format);
            }
            if let Some(dp) = depth {
                let t = texture_with_usage(
                    res,
                    dp.texture,
                    texture_usage::RENDER_TARGET,
                    "depth attachment lacks RENDER_TARGET usage",
                )?;
                if t.desc.sample_count != 1 {
                    return Err(GpuError::Unsupported(
                        "software: multisample depth attachment",
                    ));
                }
            }
            st.in_render_pass = true;
            st.color_targets = color.iter().map(|c| c.texture).collect();
            st.color_formats = formats;
        }
        Enc::EndRenderPass => {
            if !st.in_render_pass {
                return Err(GpuError::Invalid("EndRenderPass outside a render pass"));
            }
            st.end_pass();
        }
        Enc::BeginComputePass => {
            if st.in_render_pass || st.in_compute_pass {
                return Err(GpuError::Invalid("nested compute pass"));
            }
            st.in_compute_pass = true;
        }
        Enc::EndComputePass => {
            if !st.in_compute_pass {
                return Err(GpuError::Invalid("EndComputePass outside a compute pass"));
            }
            st.end_pass();
        }
        Enc::SetPipeline(p) => {
            res.pipelines.get(*p)?;
            st.pipeline = Some(*p);
        }
        Enc::SetBindGroup { group, .. } => {
            res.bind_groups.get(*group)?;
            st.bind_group = Some(*group);
        }
        Enc::SetVertexBuffer {
            slot,
            buffer,
            offset,
        } => {
            res.buffers.get(*buffer)?;
            st.vertex_buffers.insert(*slot, (*buffer, *offset));
        }
        Enc::SetIndexBuffer {
            buffer,
            offset,
            format,
        } => {
            res.buffers.get(*buffer)?;
            st.index_buffer = Some((*buffer, *offset, *format));
        }
        Enc::SetViewport {
            min_depth,
            max_depth,
            ..
        } => {
            if !(0.0..=1.0).contains(min_depth)
                || !(0.0..=1.0).contains(max_depth)
                || min_depth > max_depth
            {
                return Err(GpuError::Invalid(
                    "viewport depth range out of [0,1] or inverted",
                ));
            }
        }
        Enc::SetScissor { .. } => {}
        // Dynamic stencil reference: pure dynamic pass state (the value the compare tests against and a
        // `REPLACE` op writes). Like `SetViewport`/`SetScissor` it carries no validation obligation — the
        // reference is any `u32`; the executor applies it to the draws that follow (see `submit`).
        Enc::SetStencilReference { .. } => {}
        Enc::SetBlendConstant { .. } => {}
        Enc::ClearRect { texture, .. } => {
            let t = crate::cpu::model::texture(res, *texture)?;
            if t.desc.sample_count != 1 {
                return Err(GpuError::Unsupported("software: multisample clear"));
            }
        }
        Enc::Draw {
            vertex_count,
            instance_count,
            first_vertex,
            first_instance,
        } => {
            validate_draw(res, st, |layout, slot| {
                let (buffer, offset) =
                    st.vertex_buffers
                        .get(&slot)
                        .copied()
                        .ok_or(GpuError::Invalid(
                            "draw with no vertex buffer bound for a layout slot",
                        ))?;
                let count = if layout.step_mode == 1 {
                    first_instance.checked_add(*instance_count)
                } else {
                    first_vertex.checked_add(*vertex_count)
                };
                check_vertex_range(res, buffer, offset, layout.stride, count)
            })?;
        }
        Enc::DrawIndexed {
            index_count,
            first_index,
            ..
        } => {
            validate_draw(res, st, |_layout, slot| {
                st.vertex_buffers
                    .get(&slot)
                    .map(|_| ())
                    .ok_or(GpuError::Invalid(
                        "indexed draw with no vertex buffer bound for a layout slot",
                    ))
            })?;
            let (buffer, offset, format) = st
                .index_buffer
                .ok_or(GpuError::Invalid("indexed draw with no index buffer bound"))?;
            let isz = match format {
                IndexFormat::U16 => 2usize,
                IndexFormat::U32 => 4usize,
            };
            let last = first_index
                .checked_add(*index_count)
                .ok_or(GpuError::OutOfBounds)?;
            let need = (last as usize)
                .checked_mul(isz)
                .ok_or(GpuError::OutOfBounds)?;
            let b = crate::cpu::model::buffer(res, buffer)?;
            let end = (offset as usize)
                .checked_add(need)
                .ok_or(GpuError::OutOfBounds)?;
            if end > b.data.len() {
                return Err(GpuError::OutOfBounds);
            }
        }
        Enc::Dispatch { .. } => {
            if !st.in_compute_pass {
                return Err(GpuError::Invalid("Dispatch outside a compute pass"));
            }
            match st.pipeline {
                Some(p) => match crate::cpu::model::pipeline(res, p)? {
                    Pipeline::Compute { .. } => {}
                    Pipeline::Render { .. } => {
                        return Err(GpuError::Unsupported("dispatch on a render pipeline"))
                    }
                },
                None => return Err(GpuError::Invalid("Dispatch with no pipeline bound")),
            }
            if let Some(id) = st.bind_group {
                bind_group(res, id)?.validate(res)?;
            }
        }
        Enc::CopyBufferToBuffer {
            src,
            src_offset,
            dst,
            dst_offset,
            size,
        } => {
            let s =
                buffer_with_usage(res, *src, buffer_usage::COPY_SRC, "copy src lacks COPY_SRC")?;
            copy::check_range(s.data.len(), *src_offset, *size)?;
            let d =
                buffer_with_usage(res, *dst, buffer_usage::COPY_DST, "copy dst lacks COPY_DST")?;
            copy::check_range(d.data.len(), *dst_offset, *size)?;
        }
        Enc::CopyBufferToTexture {
            src,
            src_offset,
            bytes_per_row,
            dst,
            mip,
            width,
            height,
        } => {
            if *mip != 0 {
                return Err(GpuError::Unsupported("software: non-zero mip copy"));
            }
            let s_len =
                buffer_with_usage(res, *src, buffer_usage::COPY_SRC, "copy src lacks COPY_SRC")?
                    .data
                    .len();
            let t = texture_with_usage(
                res,
                *dst,
                texture_usage::COPY_DST,
                "copy dst lacks COPY_DST",
            )?;
            if t.desc.sample_count != 1 {
                return Err(GpuError::Unsupported(
                    "software: buffer copy to multisample texture",
                ));
            }
            let (_, _, src_span) = copy::texture_copy_layout(t, *width, *height, *bytes_per_row)?;
            copy::check_len(s_len, *src_offset, src_span)?;
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
            if *mip != 0 {
                return Err(GpuError::Unsupported("software: non-zero mip copy"));
            }
            let t = texture_with_usage(
                res,
                *src,
                texture_usage::COPY_SRC,
                "copy src lacks COPY_SRC",
            )?;
            if t.desc.sample_count != 1 {
                return Err(GpuError::Unsupported(
                    "software: multisample texture readback copy",
                ));
            }
            let bpt = t.desc.format.software_texel_bytes()?;
            if *dst_offset % bpt as u64 != 0 {
                return Err(GpuError::Invalid(
                    "texture readback offset not texel-aligned",
                ));
            }
            let (_, _, dst_span) = copy::texture_copy_layout(t, *width, *height, *bytes_per_row)?;
            let d_len =
                buffer_with_usage(res, *dst, buffer_usage::COPY_DST, "copy dst lacks COPY_DST")?
                    .data
                    .len();
            copy::check_len(d_len, *dst_offset, dst_span)?;
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
            copy::check_copy_subresource(src_sub, src_origin, extent.depth)?;
            copy::check_copy_subresource(dst_sub, dst_origin, extent.depth)?;
            let s = texture_with_usage(
                res,
                *src,
                texture_usage::COPY_SRC,
                "copy src lacks COPY_SRC",
            )?;
            let s_fmt = s.desc.format;
            let s_samples = s.desc.sample_count;
            let d = texture_with_usage(
                res,
                *dst,
                texture_usage::COPY_DST,
                "copy dst lacks COPY_DST",
            )?;
            if s_samples != 1 || d.desc.sample_count != 1 {
                return Err(GpuError::Unsupported("software: multisample texture copy"));
            }
            if s_fmt.software_texel_bytes()? != d.desc.format.software_texel_bytes()? {
                return Err(GpuError::Invalid(
                    "texture copy between incompatible texel sizes",
                ));
            }
            let s = texture(res, *src)?;
            copy::check_region_in_texture(s, src_origin, extent)?;
            let d = texture(res, *dst)?;
            copy::check_region_in_texture(d, dst_origin, extent)?;
        }
        Enc::BlitTexture {
            src,
            src_sub,
            src_origin,
            src_extent,
            dst,
            dst_sub,
            dst_origin,
            dst_extent,
            ..
        } => {
            copy::check_copy_subresource(src_sub, src_origin, src_extent.depth)?;
            copy::check_copy_subresource(dst_sub, dst_origin, dst_extent.depth)?;
            if src_extent.width == 0
                || src_extent.height == 0
                || dst_extent.width == 0
                || dst_extent.height == 0
            {
                return Err(GpuError::Invalid("blit with a zero-sized region"));
            }
            let s = texture_with_usage(
                res,
                *src,
                texture_usage::COPY_SRC,
                "blit src lacks COPY_SRC",
            )?;
            let s_fmt = s.desc.format;
            let s_samples = s.desc.sample_count;
            let d = texture_with_usage(
                res,
                *dst,
                texture_usage::COPY_DST,
                "blit dst lacks COPY_DST",
            )?;
            if s_samples != 1 || d.desc.sample_count != 1 {
                return Err(GpuError::Unsupported("software: multisample blit"));
            }
            if s_fmt.software_texel_bytes()? != d.desc.format.software_texel_bytes()? {
                return Err(GpuError::Invalid("blit between incompatible texel sizes"));
            }
            let s = texture(res, *src)?;
            copy::check_region_in_texture(s, src_origin, src_extent)?;
            let d = texture(res, *dst)?;
            copy::check_region_in_texture(d, dst_origin, dst_extent)?;
        }
        Enc::ResolveTexture {
            src,
            src_sub,
            src_origin,
            dst,
            dst_sub,
            dst_origin,
            extent,
        } => {
            copy::check_copy_subresource(src_sub, src_origin, extent.depth)?;
            copy::check_copy_subresource(dst_sub, dst_origin, extent.depth)?;
            let s = texture_with_usage(
                res,
                *src,
                texture_usage::COPY_SRC,
                "resolve src lacks COPY_SRC",
            )?;
            let s_fmt = s.desc.format;
            let s_samples = s.desc.sample_count;
            let d = texture_with_usage(
                res,
                *dst,
                texture_usage::COPY_DST,
                "resolve dst lacks COPY_DST",
            )?;
            if s_samples <= 1 || d.desc.sample_count != 1 {
                return Err(GpuError::Invalid("resolve sample counts"));
            }
            if s_fmt != d.desc.format {
                return Err(GpuError::Invalid("resolve formats differ"));
            }
            let s = texture(res, *src)?;
            copy::check_region_in_texture(s, src_origin, extent)?;
            let d = texture(res, *dst)?;
            copy::check_region_in_texture(d, dst_origin, extent)?;
        }
        Enc::FillBuffer {
            buffer,
            offset,
            size,
            ..
        } => {
            let b = buffer_with_usage(
                res,
                *buffer,
                buffer_usage::COPY_DST,
                "fill dst lacks COPY_DST",
            )?;
            copy::check_range(b.data.len(), *offset, *size)?;
        }
    }
    Ok(())
}
