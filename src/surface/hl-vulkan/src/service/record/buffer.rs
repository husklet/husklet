use super::*;
use hl_gpu::protocol::model::enums::{TextureAspect, TextureDim};

/// `vkCmdCopyBuffer` (one region) — record a `CopyBufferToBuffer`. Ported from
/// `command.rs::vkCmdCopyBuffer`.
pub fn cmd_copy_buffer(
    dev: &mut Device,
    cb: VkCommandBuffer,
    src: VkBuffer,
    dst: VkBuffer,
    src_offset: u64,
    dst_offset: u64,
    size: u64,
) -> Result<()> {
    let src_ir = dev
        .buffers
        .get(&src)
        .ok_or(GpuError::Invalid("vkCmdCopyBuffer: unknown src VkBuffer"))?
        .ir_id;
    let dst_ir = dev
        .buffers
        .get(&dst)
        .ok_or(GpuError::Invalid("vkCmdCopyBuffer: unknown dst VkBuffer"))?
        .ir_id;
    let rec = dev.require_recording_outside_pass(
        cb,
        "vkCmdCopyBuffer: must be recorded outside a render pass",
    )?;
    rec.enc.push(Enc::CopyBufferToBuffer {
        src: src_ir,
        src_offset,
        dst: dst_ir,
        dst_offset,
        size,
    });
    Ok(())
}

// ---- buffer <-> image copies -------------------------------------------------------------------
// The hl model images are single-mip, single-layer 2D render/transfer targets (base subresource); the
// copies below lower one `VkBufferImageCopy` region to the matching encoder op with mip 0 / layer 0.
// Ported (simplified for that model) from `command.rs::vkCmdCopyBufferToImage` / `vkCmdCopyImageToBuffer`.

/// The `bytes_per_row` a `VkBufferImageCopy` implies (`bufferRowLength` in texels, 0 = tight-packed to
/// `width`), plus a bounds check that the described span fits in `buf_size`. `bpt` is the destination/source
/// image's bytes-per-texel — 1 for `R8Unorm` (the GPUI glyph-coverage atlas), 2 for `Rg8`, 4 for the RGBA8/
/// BGRA8 color subset — so a non-RGBA upload computes the right stride instead of a 4×-oversized span that
/// would fail the bounds check and silently drop the copy (a void `vkCmdCopyBufferToImage` cannot report the
/// error). `None` (rejected) on a too-narrow row, a too-short image height, or an out-of-bounds span.
/// Mirrors the reference's `checked` buffer-span math.
#[allow(clippy::too_many_arguments)]
fn buffer_image_layout(
    buffer_offset: u64,
    row_length_texels: u32,
    image_height_rows: u32,
    width: u32,
    height: u32,
    layers: u32,
    format: TextureFormat,
    buf_size: u64,
) -> Option<(u32, u32)> {
    if width == 0 || height == 0 || layers == 0 {
        return None;
    }
    let row_texels = if row_length_texels == 0 {
        width
    } else {
        row_length_texels
    };
    let image_rows = if image_height_rows == 0 {
        height
    } else {
        image_height_rows
    };
    if row_texels < width || image_rows < height {
        return None;
    }
    let (bytes_per_row, copy_rows, rows_per_image) =
        if let Some((block_width, block_height, block_bytes)) = format.block_geometry() {
            if row_texels % block_width != 0 || image_rows % block_height != 0 {
                return None;
            }
            (
                row_texels
                    .checked_div(block_width)?
                    .checked_mul(block_bytes)?,
                height.div_ceil(block_height),
                image_rows.div_ceil(block_height),
            )
        } else {
            (
                row_texels.checked_mul(format.bytes_per_texel()? as u32)?,
                height,
                image_rows,
            )
        };
    let (tight_row, _) = format.copy_layout(width, height)?;
    let rows = rows_per_image
        .checked_mul(layers.saturating_sub(1))?
        .checked_add(copy_rows)?;
    let end = (bytes_per_row as u64)
        .checked_mul(rows.saturating_sub(1) as u64)?
        .checked_add(tight_row as u64)?
        .checked_add(buffer_offset)?;
    (end <= buf_size).then_some((bytes_per_row, rows_per_image))
}

/// `vkCmdCopyBufferToImage` (one region, base subresource) — record `CopyBufferToTexture`. The src buffer
/// must be `COPY_SRC` and the dst image `COPY_DST`; the region must fit the buffer + the image extent.
#[allow(clippy::too_many_arguments)]
pub fn cmd_copy_buffer_to_image(
    dev: &mut Device,
    cb: VkCommandBuffer,
    src: VkBuffer,
    dst: VkImage,
    buffer_offset: u64,
    row_length_texels: u32,
    image_height_rows: u32,
    width: u32,
    height: u32,
) -> Result<()> {
    cmd_copy_buffer_to_image_region(
        dev,
        cb,
        src,
        dst,
        buffer_offset,
        row_length_texels,
        image_height_rows,
        0,
        0,
        0,
        0,
        0,
        width,
        height,
        1,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_copy_buffer_to_image_region(
    dev: &mut Device,
    cb: VkCommandBuffer,
    src: VkBuffer,
    dst: VkImage,
    buffer_offset: u64,
    row_length_texels: u32,
    image_height_rows: u32,
    mip: u32,
    base_layer: u32,
    x: u32,
    y: u32,
    z: u32,
    width: u32,
    height: u32,
    layers: u32,
) -> Result<()> {
    let (src_ir, buf_size, buf_usage) = {
        let b = dev.buffers.get(&src).ok_or(GpuError::Invalid(
            "vkCmdCopyBufferToImage: unknown src VkBuffer",
        ))?;
        (b.ir_id, b.size, b.usage)
    };
    let (dst_ir, img_usage, iw, ih, image_depth, image_dim, image_layers, mip_levels, dst_fmt) = {
        let i = dev.images.get(&dst).ok_or(GpuError::Invalid(
            "vkCmdCopyBufferToImage: unknown dst VkImage",
        ))?;
        (
            i.ir_id,
            i.usage,
            i.width,
            i.height,
            i.depth,
            i.dim,
            i.layers,
            i.mip_levels,
            i.format,
        )
    };
    if buf_usage & buffer_usage::COPY_SRC == 0 || img_usage & texture_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid(
            "vkCmdCopyBufferToImage: missing COPY_SRC/COPY_DST usage",
        ));
    }
    if mip >= mip_levels {
        return Err(GpuError::OutOfBounds);
    }
    let mip_width = (iw >> mip).max(1);
    let mip_height = (ih >> mip).max(1);
    let mip_depth = (image_depth >> mip).max(1);
    let (layer, origin_z, copy_depth) = if image_dim == TextureDim::D3 {
        if base_layer != 0 || z.checked_add(layers).is_none_or(|end| end > mip_depth) {
            return Err(GpuError::OutOfBounds);
        }
        (0, z, layers)
    } else {
        let layer = base_layer.checked_add(z).ok_or(GpuError::OutOfBounds)?;
        if layer
            .checked_add(layers)
            .is_none_or(|end| end > image_layers)
        {
            return Err(GpuError::OutOfBounds);
        }
        (layer, 0, layers)
    };
    if x.checked_add(width).is_none_or(|end| end > mip_width)
        || y.checked_add(height).is_none_or(|end| end > mip_height)
    {
        return Err(GpuError::OutOfBounds);
    }
    // The image's bytes-per-texel — an R8 coverage atlas is 1, not 4. Using it makes the stride/bounds math
    // correct so an R8 (or Rg8) upload is not rejected as out-of-bounds.
    let (bytes_per_row, rows_per_image) = buffer_image_layout(
        buffer_offset,
        row_length_texels,
        image_height_rows,
        width,
        height,
        layers,
        dst_fmt,
        buf_size,
    )
    .ok_or(GpuError::OutOfBounds)?;
    let rec = dev.require_recording_outside_pass(
        cb,
        "vkCmdCopyBufferToImage: must be recorded outside a render pass",
    )?;
    if mip == 0 && layer == 0 && x == 0 && y == 0 && origin_z == 0 && layers == 1 {
        rec.enc.push(Enc::CopyBufferToTexture {
            src: src_ir,
            src_offset: buffer_offset,
            bytes_per_row,
            dst: dst_ir,
            mip,
            width,
            height,
        });
        return Ok(());
    }
    rec.enc.push(Enc::CopyBufferToTextureRegion {
        src: src_ir,
        src_offset: buffer_offset,
        bytes_per_row,
        rows_per_image,
        dst: dst_ir,
        dst_sub: TextureSubresource {
            mip,
            layer,
            aspect: TextureAspect::All,
        },
        dst_origin: Origin3d { x, y, z: origin_z },
        extent: Extent3d {
            width,
            height,
            depth: copy_depth,
        },
    });
    Ok(())
}

/// `vkCmdCopyImageToBuffer` (one region, base subresource) — record `CopyTextureToBuffer`. The src image
/// must be `COPY_SRC` and the dst buffer `COPY_DST`; the region must fit the image extent + the buffer.
#[allow(clippy::too_many_arguments)]
pub fn cmd_copy_image_to_buffer(
    dev: &mut Device,
    cb: VkCommandBuffer,
    src: VkImage,
    dst: VkBuffer,
    buffer_offset: u64,
    row_length_texels: u32,
    image_height_rows: u32,
    width: u32,
    height: u32,
) -> Result<()> {
    cmd_copy_image_to_buffer_region(
        dev,
        cb,
        src,
        dst,
        buffer_offset,
        row_length_texels,
        image_height_rows,
        0,
        0,
        0,
        0,
        0,
        width,
        height,
        1,
    )
}

#[allow(clippy::too_many_arguments)]
pub fn cmd_copy_image_to_buffer_region(
    dev: &mut Device,
    cb: VkCommandBuffer,
    src: VkImage,
    dst: VkBuffer,
    buffer_offset: u64,
    row_length_texels: u32,
    image_height_rows: u32,
    mip: u32,
    base_layer: u32,
    x: u32,
    y: u32,
    z: u32,
    width: u32,
    height: u32,
    layers: u32,
) -> Result<()> {
    let (src_ir, img_usage, iw, ih, image_depth, image_dim, image_layers, mip_levels, src_fmt) = {
        let i = dev.images.get(&src).ok_or(GpuError::Invalid(
            "vkCmdCopyImageToBuffer: unknown src VkImage",
        ))?;
        (
            i.ir_id,
            i.usage,
            i.width,
            i.height,
            i.depth,
            i.dim,
            i.layers,
            i.mip_levels,
            i.format,
        )
    };
    let (dst_ir, buf_size, buf_usage) = {
        let b = dev.buffers.get(&dst).ok_or(GpuError::Invalid(
            "vkCmdCopyImageToBuffer: unknown dst VkBuffer",
        ))?;
        (b.ir_id, b.size, b.usage)
    };
    if img_usage & texture_usage::COPY_SRC == 0 || buf_usage & buffer_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid(
            "vkCmdCopyImageToBuffer: missing COPY_SRC/COPY_DST usage",
        ));
    }
    if mip >= mip_levels {
        return Err(GpuError::OutOfBounds);
    }
    let mip_width = (iw >> mip).max(1);
    let mip_height = (ih >> mip).max(1);
    let mip_depth = (image_depth >> mip).max(1);
    let (layer, origin_z, copy_depth) = if image_dim == TextureDim::D3 {
        if base_layer != 0 || z.checked_add(layers).is_none_or(|end| end > mip_depth) {
            return Err(GpuError::OutOfBounds);
        }
        (0, z, layers)
    } else {
        let layer = base_layer.checked_add(z).ok_or(GpuError::OutOfBounds)?;
        if layer
            .checked_add(layers)
            .is_none_or(|end| end > image_layers)
        {
            return Err(GpuError::OutOfBounds);
        }
        (layer, 0, layers)
    };
    if x.checked_add(width).is_none_or(|end| end > mip_width)
        || y.checked_add(height).is_none_or(|end| end > mip_height)
    {
        return Err(GpuError::OutOfBounds);
    }
    let (bytes_per_row, rows_per_image) = buffer_image_layout(
        buffer_offset,
        row_length_texels,
        image_height_rows,
        width,
        height,
        layers,
        src_fmt,
        buf_size,
    )
    .ok_or(GpuError::OutOfBounds)?;
    let rec = dev.require_recording_outside_pass(
        cb,
        "vkCmdCopyImageToBuffer: must be recorded outside a render pass",
    )?;
    if mip == 0 && layer == 0 && x == 0 && y == 0 && origin_z == 0 && layers == 1 {
        rec.enc.push(Enc::CopyTextureToBuffer {
            src: src_ir,
            mip,
            width,
            height,
            dst: dst_ir,
            dst_offset: buffer_offset,
            bytes_per_row,
        });
        return Ok(());
    }
    rec.enc.push(Enc::CopyTextureToBufferRegion {
        src: src_ir,
        src_sub: TextureSubresource {
            mip,
            layer,
            aspect: TextureAspect::All,
        },
        src_origin: Origin3d { x, y, z: origin_z },
        extent: Extent3d {
            width,
            height,
            depth: copy_depth,
        },
        dst: dst_ir,
        dst_offset: buffer_offset,
        bytes_per_row,
        rows_per_image,
    });
    Ok(())
}
