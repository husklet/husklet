use super::*;

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
    let rec = dev.require_recording(cb)?;
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
fn buffer_image_bytes_per_row(
    buffer_offset: u64,
    row_length_texels: u32,
    image_height_rows: u32,
    width: u32,
    height: u32,
    bpt: u32,
    buf_size: u64,
) -> Option<u32> {
    if width == 0 || height == 0 || bpt == 0 {
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
    let bytes_per_row = row_texels.checked_mul(bpt)?;
    let end = (bytes_per_row as u64)
        .checked_mul(height.saturating_sub(1) as u64)?
        .checked_add(width as u64 * bpt as u64)?
        .checked_add(buffer_offset)?;
    (end <= buf_size).then_some(bytes_per_row)
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
    let (src_ir, buf_size, buf_usage) = {
        let b = dev.buffers.get(&src).ok_or(GpuError::Invalid(
            "vkCmdCopyBufferToImage: unknown src VkBuffer",
        ))?;
        (b.ir_id, b.size, b.usage)
    };
    let (dst_ir, img_usage, iw, ih, dst_fmt) = {
        let i = dev.images.get(&dst).ok_or(GpuError::Invalid(
            "vkCmdCopyBufferToImage: unknown dst VkImage",
        ))?;
        (i.ir_id, i.usage, i.width, i.height, i.format)
    };
    if buf_usage & buffer_usage::COPY_SRC == 0 || img_usage & texture_usage::COPY_DST == 0 {
        return Err(GpuError::Invalid(
            "vkCmdCopyBufferToImage: missing COPY_SRC/COPY_DST usage",
        ));
    }
    if width > iw || height > ih {
        return Err(GpuError::OutOfBounds);
    }
    // The image's bytes-per-texel — an R8 coverage atlas is 1, not 4. Using it makes the stride/bounds math
    // correct so an R8 (or Rg8) upload is not rejected as out-of-bounds.
    let bpt = dst_fmt.bytes_per_texel().ok_or(GpuError::Unsupported(
        "vkCmdCopyBufferToImage: image format has no packed texel layout",
    ))? as u32;
    let bytes_per_row = buffer_image_bytes_per_row(
        buffer_offset,
        row_length_texels,
        image_height_rows,
        width,
        height,
        bpt,
        buf_size,
    )
    .ok_or(GpuError::OutOfBounds)?;
    let rec = dev.require_recording(cb)?;
    rec.enc.push(Enc::CopyBufferToTexture {
        src: src_ir,
        src_offset: buffer_offset,
        bytes_per_row,
        dst: dst_ir,
        mip: 0,
        width,
        height,
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
    let (src_ir, img_usage, iw, ih, src_fmt) = {
        let i = dev.images.get(&src).ok_or(GpuError::Invalid(
            "vkCmdCopyImageToBuffer: unknown src VkImage",
        ))?;
        (i.ir_id, i.usage, i.width, i.height, i.format)
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
    if width > iw || height > ih {
        return Err(GpuError::OutOfBounds);
    }
    let bpt = src_fmt.bytes_per_texel().ok_or(GpuError::Unsupported(
        "vkCmdCopyImageToBuffer: image format has no packed texel layout",
    ))? as u32;
    let bytes_per_row = buffer_image_bytes_per_row(
        buffer_offset,
        row_length_texels,
        image_height_rows,
        width,
        height,
        bpt,
        buf_size,
    )
    .ok_or(GpuError::OutOfBounds)?;
    let rec = dev.require_recording(cb)?;
    rec.enc.push(Enc::CopyTextureToBuffer {
        src: src_ir,
        mip: 0,
        width,
        height,
        dst: dst_ir,
        dst_offset: buffer_offset,
        bytes_per_row,
    });
    Ok(())
}
