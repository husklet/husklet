//! Buffer/texture copy + blit + resolve execution, plus the shared bounds/layout helpers the submit-time
//! validation and the copy execution both use. Ported from the copy arms of `SoftwareBackend::submit` and
//! the free layout helpers in `hl-gpu/src/software.rs`.

use crate::cpu::format::{sample_bilinear, texel_at};
use crate::cpu::model::texture::Texture;
use crate::cpu::model::{buffer, buffer_mut, texture, texture_mut};
use crate::protocol::model::descriptor::{Extent3d, Origin3d, TextureSubresource};
use crate::protocol::model::enums::{Filter, TextureAspect};
use crate::protocol::model::error::{GpuError, Result};
use crate::runtime::model::resources::SessionResources;

// ---------------------------------------------------------------------------------------------------
// bounds / layout helpers (shared by validation + execution)
// ---------------------------------------------------------------------------------------------------

/// Resolve a bind-group buffer slice's `(offset, len)`, treating `size == 0` as "to the end of the
/// buffer" and rejecting any offset/size that would run past the buffer (with wrapping-safe math).
pub(crate) fn buffer_slice_bounds(
    buf_len: usize,
    offset: u64,
    size: u64,
) -> Result<(usize, usize)> {
    if offset > buf_len as u64 {
        return Err(GpuError::OutOfBounds);
    }
    let off = offset as usize;
    let len = if size == 0 {
        buf_len - off
    } else {
        let end = offset.checked_add(size).ok_or(GpuError::OutOfBounds)?;
        if end > buf_len as u64 {
            return Err(GpuError::OutOfBounds);
        }
        size as usize
    };
    Ok((off, len))
}

/// Bounds-check `[offset, offset+size)` against `len` with wrapping-safe arithmetic.
pub(crate) fn check_range(len: usize, offset: u64, size: u64) -> Result<()> {
    let end = offset.checked_add(size).ok_or(GpuError::OutOfBounds)?;
    if end > len as u64 {
        return Err(GpuError::OutOfBounds);
    }
    Ok(())
}

/// Bounds-check a `usize` span starting at `offset` against `len`.
pub(crate) fn check_len(len: usize, offset: u64, span: usize) -> Result<()> {
    check_range(len, offset, span as u64)
}

/// Compute `(row_bytes, tight_bytes, buffer_span)` for a width×height texture copy, honoring the row
/// stride and validating the extent against the texture dimensions. `bytes_per_row == 0` means tight.
pub(crate) fn texture_copy_layout(
    t: &Texture,
    width: u32,
    height: u32,
    bytes_per_row: u32,
) -> Result<(usize, usize, usize)> {
    if width > t.desc.width || height > t.desc.height {
        return Err(GpuError::OutOfBounds);
    }
    let bpt = t
        .desc
        .format
        .bytes_per_texel()
        .ok_or(GpuError::Unsupported("software: non-color texture format"))?;
    let row_bytes = bpt
        .checked_mul(width as usize)
        .ok_or(GpuError::OutOfBounds)?;
    let rows = height as usize;
    let stride = if bytes_per_row == 0 {
        row_bytes
    } else {
        bytes_per_row as usize
    };
    if stride < row_bytes {
        return Err(GpuError::OutOfBounds);
    }
    let tight = row_bytes.checked_mul(rows).ok_or(GpuError::OutOfBounds)?;
    let span = if rows == 0 {
        0
    } else {
        (rows - 1)
            .checked_mul(stride)
            .and_then(|v| v.checked_add(row_bytes))
            .ok_or(GpuError::OutOfBounds)?
    };
    Ok((row_bytes, tight, span))
}

/// Bounds-check that `[origin, origin+extent)` lies fully within a texture's level-0 plane (wrapping-safe).
pub(crate) fn check_region_in_texture(
    t: &Texture,
    origin: &Origin3d,
    extent: &Extent3d,
) -> Result<()> {
    let x_end = origin
        .x
        .checked_add(extent.width)
        .ok_or(GpuError::OutOfBounds)?;
    let y_end = origin
        .y
        .checked_add(extent.height)
        .ok_or(GpuError::OutOfBounds)?;
    if x_end > t.desc.width || y_end > t.desc.height {
        return Err(GpuError::OutOfBounds);
    }
    Ok(())
}

/// The software oracle materializes only 2D, single-layer, level-0 color textures, so a copy/blit
/// subresource that names a non-zero mip/layer, a non-color aspect, or a 3D depth slice is rejected.
pub(crate) fn check_copy_subresource(
    sub: &TextureSubresource,
    origin: &Origin3d,
    depth: u32,
) -> Result<()> {
    if sub.mip != 0 {
        return Err(GpuError::Unsupported("software: non-zero mip texture copy"));
    }
    if sub.layer != 0 {
        return Err(GpuError::Unsupported("software: array-layer texture copy"));
    }
    if sub.aspect != TextureAspect::All {
        return Err(GpuError::Unsupported(
            "software: non-color aspect texture copy",
        ));
    }
    if origin.z != 0 || depth > 1 {
        return Err(GpuError::Unsupported(
            "software: 3D/depth-slice texture copy",
        ));
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------------
// copy execution (mirrors the copy arms of SoftwareBackend::submit)
// ---------------------------------------------------------------------------------------------------

pub(crate) fn copy_buffer_to_buffer(
    res: &mut SessionResources,
    src: u32,
    src_offset: u64,
    dst: u32,
    dst_offset: u64,
    size: u64,
) -> Result<()> {
    let chunk = {
        let s = buffer(res, src)?;
        let so = src_offset as usize;
        let sz = size as usize;
        s.data[so..so + sz].to_vec()
    };
    let d = buffer_mut(res, dst)?;
    let d_off = dst_offset as usize;
    d.data[d_off..d_off + chunk.len()].copy_from_slice(&chunk);
    Ok(())
}

pub(crate) fn copy_buffer_to_texture(
    res: &mut SessionResources,
    src: u32,
    src_offset: u64,
    bytes_per_row: u32,
    dst: u32,
    width: u32,
    height: u32,
) -> Result<()> {
    let (row_bytes, tight, _span) = {
        let t = texture(res, dst)?;
        texture_copy_layout(t, width, height, bytes_per_row)?
    };
    let rows = height as usize;
    let src_stride = if bytes_per_row == 0 {
        row_bytes
    } else {
        bytes_per_row as usize
    };
    let so = src_offset as usize;
    let chunk = {
        let s = buffer(res, src)?;
        let mut out = Vec::with_capacity(tight);
        for row in 0..rows {
            let start = so + row * src_stride;
            out.extend_from_slice(&s.data[start..start + row_bytes]);
        }
        out
    };
    let t = texture_mut(res, dst)?;
    t.pixels[..tight].copy_from_slice(&chunk);
    Ok(())
}

pub(crate) fn copy_texture_to_buffer(
    res: &mut SessionResources,
    src: u32,
    width: u32,
    height: u32,
    dst: u32,
    dst_offset: u64,
    bytes_per_row: u32,
) -> Result<()> {
    let (row_bytes, _tight, _span) = {
        let t = texture(res, src)?;
        texture_copy_layout(t, width, height, bytes_per_row)?
    };
    let rows = height as usize;
    let dst_stride = if bytes_per_row == 0 {
        row_bytes
    } else {
        bytes_per_row as usize
    };
    let (tw, bpt) = {
        let t = texture(res, src)?;
        (t.desc.width as usize, t.desc.format.software_texel_bytes()?)
    };
    let rows_data: Vec<u8> = {
        let t = texture(res, src)?;
        let mut out = Vec::with_capacity(rows * row_bytes);
        for row in 0..rows {
            let start = row * tw * bpt;
            out.extend_from_slice(&t.pixels[start..start + row_bytes]);
        }
        out
    };
    let d = buffer_mut(res, dst)?;
    let base = dst_offset as usize;
    for row in 0..rows {
        let dstart = base + row * dst_stride;
        d.data[dstart..dstart + row_bytes]
            .copy_from_slice(&rows_data[row * row_bytes..row * row_bytes + row_bytes]);
    }
    Ok(())
}

pub(crate) fn copy_texture_to_texture(
    res: &mut SessionResources,
    src: u32,
    src_origin: &Origin3d,
    dst: u32,
    dst_origin: &Origin3d,
    extent: &Extent3d,
) -> Result<()> {
    let (sw, bpt) = {
        let t = texture(res, src)?;
        (t.desc.width as usize, t.desc.format.software_texel_bytes()?)
    };
    let ew = extent.width as usize;
    let eh = extent.height as usize;
    let row_bytes = ew * bpt;
    let block: Vec<u8> = {
        let t = texture(res, src)?;
        let mut out = Vec::with_capacity(row_bytes * eh);
        for row in 0..eh {
            let sy = src_origin.y as usize + row;
            let sx = src_origin.x as usize;
            let start = (sy * sw + sx) * bpt;
            out.extend_from_slice(&t.pixels[start..start + row_bytes]);
        }
        out
    };
    let dw = texture(res, dst)?.desc.width as usize;
    let t = texture_mut(res, dst)?;
    for row in 0..eh {
        let dy = dst_origin.y as usize + row;
        let dx = dst_origin.x as usize;
        let dstart = (dy * dw + dx) * bpt;
        t.pixels[dstart..dstart + row_bytes]
            .copy_from_slice(&block[row * row_bytes..(row + 1) * row_bytes]);
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
/// `BlitTexture`: resample the source region into the destination region, CONVERTING between the two
/// formats rather than reinterpreting bytes.
///
/// The contract is the host's, established by measuring it rather than by preference. The wgpu executor
/// performs a blit by RENDERING — it samples the source and writes the destination as a colour attachment
/// — so a differing format is a conversion through the sampler and the ROP, and the texel sizes need not
/// match at all. `R8Unorm` into `Rgba8Unorm` and `Rgba16Float` into `Rgba8Unorm` both run there.
///
/// This oracle disagreed with that in both directions. It REFUSED any pair whose texel sizes differed,
/// which the executor accepts — a divergence that is purely the reference's. And its nearest-filter path
/// copied the source texel's raw bytes into the destination, so a same-size pair with different meanings
/// (`Rgba8Unorm` into `R32Float`, four bytes either way) reinterpreted unsigned bytes as a float and both
/// backends ran and disagreed silently. Now both filters decode through the source format and re-encode
/// through the destination's, which is the same arithmetic the sampler-and-ROP path performs.
pub(crate) fn blit_texture(
    res: &mut SessionResources,
    src: u32,
    src_origin: &Origin3d,
    src_extent: &Extent3d,
    dst: u32,
    dst_origin: &Origin3d,
    dst_extent: &Extent3d,
    filter: Filter,
) -> Result<()> {
    let (sw, src_bpt, src_fmt) = {
        let t = texture(res, src)?;
        (
            t.desc.width as usize,
            t.desc.format.software_texel_bytes()?,
            t.desc.format,
        )
    };
    let src_pixels = texture(res, src)?.pixels.clone();
    let (sox, soy) = (src_origin.x as usize, src_origin.y as usize);
    let (sew, seh) = (src_extent.width as usize, src_extent.height as usize);
    let (dew, deh) = (dst_extent.width as usize, dst_extent.height as usize);
    let (dw, dst_bpt, dst_fmt) = {
        let t = texture(res, dst)?;
        (
            t.desc.width as usize,
            t.desc.format.software_texel_bytes()?,
            t.desc.format,
        )
    };
    let t = texture_mut(res, dst)?;
    for dy in 0..deh {
        let fy = soy as f32 + (dy as f32 + 0.5) * seh as f32 / deh as f32;
        for dx in 0..dew {
            let fx = sox as f32 + (dx as f32 + 0.5) * sew as f32 / dew as f32;
            // Both filters produce the source colour as VALUES; only how the neighbourhood is reduced
            // differs. Encoding into the destination is then one shared step, so the two filters cannot
            // come to different conclusions about what a format means — which is exactly what happened
            // while nearest copied bytes and linear interpolated them.
            let rgba = match filter {
                Filter::Nearest => {
                    let sx = (fx as usize).clamp(sox, sox + sew - 1);
                    let sy = (fy as usize).clamp(soy, soy + seh - 1);
                    src_fmt.texel_to_f32(texel_at(&src_pixels, sw, sx, sy, src_bpt))
                }
                Filter::Linear => sample_bilinear(
                    &src_pixels,
                    sw,
                    src_bpt,
                    fx,
                    fy,
                    sox,
                    sox + sew - 1,
                    soy,
                    soy + seh - 1,
                    src_fmt,
                ),
            };
            // A filter or a format this oracle cannot express is a typed refusal, never a fallback:
            // quietly substituting another filter or a byte copy would read as a filtering difference
            // rather than as an unsupported operation.
            let rgba = rgba.ok_or(GpuError::Unsupported(
                "software: blit filter for this texel format",
            ))?;
            let texel = dst_fmt.clear_texel(rgba).ok_or(GpuError::Unsupported(
                "software: blit into this texel format",
            ))?;
            let hlx = dst_origin.x as usize + dx;
            let hly = dst_origin.y as usize + dy;
            let off = (hly * dw + hlx) * dst_bpt;
            t.pixels[off..off + dst_bpt].copy_from_slice(&texel);
        }
    }
    Ok(())
}

pub(crate) fn resolve_texture(
    res: &mut SessionResources,
    src: u32,
    src_origin: &Origin3d,
    dst: u32,
    dst_origin: &Origin3d,
    extent: &Extent3d,
) -> Result<()> {
    let (sw, samples, bpt, source) = {
        let t = texture(res, src)?;
        (
            t.desc.width as usize,
            t.desc.sample_count as usize,
            t.desc.format.software_texel_bytes()?,
            t.pixels.clone(),
        )
    };
    let dw = texture(res, dst)?.desc.width as usize;
    let t = texture_mut(res, dst)?;
    for y in 0..extent.height as usize {
        for x in 0..extent.width as usize {
            let sp = ((src_origin.y as usize + y) * sw + src_origin.x as usize + x) * samples * bpt;
            let dp = ((dst_origin.y as usize + y) * dw + dst_origin.x as usize + x) * bpt;
            for channel in 0..bpt {
                let sum: u32 = (0..samples)
                    .map(|sample| source[sp + sample * bpt + channel] as u32)
                    .sum();
                t.pixels[dp + channel] = (sum / samples as u32) as u8;
            }
        }
    }
    Ok(())
}
