use super::{validation::*, *};

/// Formats whose texels are raw integers, which cannot take part in a blit on either side. See the
/// refusal in `BlitTexture` below for why this is the host's rule rather than this oracle's.
const INTEGER_BLIT_REFUSED: &[TextureFormat] = &[
    TextureFormat::R8Uint,
    TextureFormat::R8Sint,
    TextureFormat::Rg8Uint,
    TextureFormat::Rg8Sint,
    TextureFormat::Rgba8Uint,
    TextureFormat::Rgba8Sint,
];

pub(super) fn validate_op(res: &SessionResources, op: &Enc, st: &mut EncoderState) -> Result<()> {
    match op {
        Enc::BeginRenderPass { color, depth } => {
            if st.in_render_pass || st.in_compute_pass {
                return Err(GpuError::Invalid("nested render pass"));
            }
            let mut formats = Vec::with_capacity(color.len());
            // Every attachment of a pass must share one extent (WebGPU `renderPass` / Vulkan
            // `VkFramebuffer` both require it). The depth-tested raster path sizes its per-pixel coverage
            // window from the depth attachment and composites those pixels into each color attachment, so
            // a disagreement would index a smaller plane out of range.
            let mut extent: Option<(u32, u32)> = None;
            let mut agree = |t: &Texture| match extent {
                Some(e) if e != (t.desc.width, t.desc.height) => Err(GpuError::Invalid(
                    "render pass attachments disagree on extent",
                )),
                _ => {
                    extent = Some((t.desc.width, t.desc.height));
                    Ok(())
                }
            };
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
                // A LAYERED texture is not a colour attachment, and the reason is the executor's, not
                // this reference's convenience: measured, it refuses one outright, because every render
                // pass there binds the texture's default view and an array texture's is a `D2Array` no
                // colour pass can target. This reference could rasterize into layer 0 quite happily —
                // which is exactly why the refusal has to be explicit. Serving what the subject refuses
                // is a false divergence in the other direction, and it would arrive the moment layered
                // textures became creatable here.
                if t.desc.dim != TextureDim::D2 || t.layers() != 1 {
                    return Err(GpuError::Unsupported(
                        "software: layered render attachment",
                    ));
                }
                if c.load == LoadOp::Clear {
                    t.desc.format.software_clear_texel(c.clear)?;
                }
                agree(t)?;
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
                if t.desc.dim != TextureDim::D2 || t.layers() != 1 {
                    return Err(GpuError::Unsupported(
                        "software: layered depth attachment",
                    ));
                }
                agree(t)?;
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
        Enc::ClearRect {
            texture,
            color,
            base_array_layer,
            layer_count,
            mip_level,
            ..
        } => {
            let t = crate::cpu::model::texture(res, *texture)?;
            if t.desc.sample_count != 1 {
                return Err(GpuError::Unsupported("software: multisample clear"));
            }
            // A LAYER RANGE is served: `pixels` is layer-major, and the executor serves a layered clear
            // too (measured — it clears any base layer and count on a 2D array). A range that runs past
            // the materialized layers is refused rather than clamped, because writing fewer layers than
            // asked is the silent-partial-work shape, and the executor's own bounds would reject it.
            //
            // A non-zero MIP is still refused: only level 0 is materialized. Writing level 0 in response
            // to a clear of some other level is exactly the defect these fields were added to fix, so the
            // subresource this backend cannot address is refused here — before any op in the batch runs —
            // rather than silently redirected.
            if *mip_level != 0 {
                return Err(GpuError::Unsupported(
                    "software: clear of a non-base mip level",
                ));
            }
            let layers = t.layers();
            if *layer_count == 0
                || base_array_layer
                    .checked_add(*layer_count)
                    .is_none_or(|end| end > layers)
            {
                return Err(GpuError::OutOfBounds);
            }
            // Pack the clear color HERE so a format the oracle cannot clear is rejected before any op in
            // this command buffer runs: an unclearable format discovered mid-execution would leave earlier
            // ops' writes behind, which the runtime's id-table transaction cannot undo.
            t.desc.format.software_clear_texel(*color)?;
        }
        Enc::Draw {
            vertex_count,
            instance_count,
            first_vertex,
            first_instance,
        } => {
            validate_draw(res, st, *instance_count, |layout, slot| {
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
            instance_count,
            ..
        } => {
            validate_draw(res, st, *instance_count, |_layout, slot| {
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
            // The span covers EVERY plane: the executor uploads the destination's full layer/slice/face
            // count in one operation and refuses a buffer that cannot supply them all.
            let (row_bytes, _, _) = copy::texture_copy_layout(t, *width, *height, *bytes_per_row)?;
            let stride = if *bytes_per_row == 0 {
                row_bytes
            } else {
                *bytes_per_row as usize
            };
            let total_rows = (*height as usize)
                .checked_mul(t.layers() as usize)
                .ok_or(GpuError::OutOfBounds)?;
            let src_span = if total_rows == 0 {
                0
            } else {
                (total_rows - 1)
                    .checked_mul(stride)
                    .and_then(|v| v.checked_add(row_bytes))
                    .ok_or(GpuError::OutOfBounds)?
            };
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
        // The REGION copies, which are the only channel either backend has for observing a non-base
        // layer, slice or face. Refused as a whole op class until the reference materialized one plane
        // per layer; served now, at every layer and sub-rect the executor serves — measured against it,
        // including that it bounds-checks a layer past the end rather than clamping.
        //
        // A non-zero MIP is the one thing the executor serves here that this reference cannot: it
        // round-trips mip 1 of a two-level texture, and only level 0 is materialized here. Refused by
        // name, narrower than the subject rather than wrong about it. Retire when the reference
        // materializes the mip chain.
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
            copy::check_region_subresource(t, dst_sub)?;
            let (_, row_bytes, stride, span) = copy::region_layout(
                t,
                dst_sub,
                dst_origin,
                extent,
                *bytes_per_row,
                *rows_per_image,
            )?;
            let (_, _) = (row_bytes, stride);
            copy::check_len(s_len, *src_offset, span)?;
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
            let t = texture_with_usage(
                res,
                *src,
                texture_usage::COPY_SRC,
                "copy src lacks COPY_SRC",
            )?;
            copy::check_region_subresource(t, src_sub)?;
            let (_, _, _, span) = copy::region_layout(
                t,
                src_sub,
                src_origin,
                extent,
                *bytes_per_row,
                *rows_per_image,
            )?;
            let d_len =
                buffer_with_usage(res, *dst, buffer_usage::COPY_DST, "copy dst lacks COPY_DST")?
                    .data
                    .len();
            copy::check_len(d_len, *dst_offset, span)?;
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
            // The executor blits by RENDERING, through a 2D view of one layer, and declines a 1D or 3D
            // texture on either side — measured, not assumed, and it is the one operation of the four
            // that does not extend to every dimension. A cube face IS a 2D layer there and blits fine,
            // so cube is deliberately absent from this refusal.
            for id in [*src, *dst] {
                let dim = crate::cpu::model::texture(res, id)?.desc.dim;
                if matches!(dim, TextureDim::D1 | TextureDim::D3) {
                    return Err(GpuError::Unsupported("software: 1D/3D texture blit"));
                }
            }
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
            // Both sides must have a plain-colour texel; their SIZES need not match. Requiring equal
            // sizes was this reference's own invention: the executor blits by rendering — sampling the
            // source and writing the destination as a colour attachment — so `R8Unorm` into `Rgba8Unorm`
            // and `Rgba16Float` into `Rgba8Unorm` both run there, and refusing them here produced a
            // divergence that belonged to the oracle rather than to either backend.
            let d_fmt = d.desc.format;
            let (_, _) = (
                s_fmt.software_texel_bytes()?,
                d_fmt.software_texel_bytes()?,
            );
            // An INTEGER format cannot take part in a blit on either side, which the host settles rather
            // than this oracle: wgpu refuses the source (an integer view cannot bind to a filterable
            // float sampler) and refuses the destination (the blit shader's float output is incompatible
            // with an integer colour target). Matching that refusal here keeps the two backends agreeing
            // about what is legal instead of only about what the legal cases produce.
            //
            // UNREACHABLE TODAY, and asserted as such by test: integer formats are absent from
            // `COLOR_FORMATS`, so this oracle advertises none and `CreateTexture` refuses one before a
            // blit can name it. Kept because the capability doc contemplates that changing.
            for format in [s_fmt, d_fmt] {
                if INTEGER_BLIT_REFUSED.contains(&format) {
                    return Err(GpuError::Unsupported(
                        "software: blit of an integer texel format",
                    ));
                }
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
