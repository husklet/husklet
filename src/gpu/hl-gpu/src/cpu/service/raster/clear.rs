use super::*;
use crate::cpu::model::texture::Texture;

pub(crate) fn clear_target(
    res: &mut SessionResources,
    texture_id: u32,
    color: [f64; 4],
) -> Result<()> {
    let fmt = texture(res, texture_id)?.desc.format;
    let texel = fmt.software_clear_texel_f64(color)?;
    let t = texture_mut(res, texture_id)?;
    // Fill IN PLACE rather than rebuilding the vector at `w * h * texel.len()`. The rebuild silently
    // resized the allocation to one single-sampled plane, which was invisible only because a multisampled
    // and (now) a layered attachment are both refused before reaching here — a correctness that depended
    // on two guards elsewhere rather than on this function. Writing over what is allocated cannot shrink
    // it whatever the shape turns out to be.
    for chunk in t.pixels.chunks_exact_mut(texel.len()) {
        chunk.copy_from_slice(&texel);
    }
    Ok(())
}

/// `ClearRect`: fill only the covered sub-rectangle of `layer_count` array layers from `base_array_layer`
/// with the packed clear color.
///
/// The layer range is the caller's, not assumed to be the base one: `pixels` is layer-major and the
/// executor clears exactly the range it is given, so this does too. Validation has already established
/// that the range lies inside the materialized layers.
#[allow(clippy::too_many_arguments)]
pub(crate) fn clear_rect(
    res: &mut SessionResources,
    texture_id: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    color: [f64; 4],
    base_array_layer: u32,
    layer_count: u32,
    mip_level: u32,
) -> Result<()> {
    // The LEVEL's own extent, not the base one: a rect is clamped against the level it names, or a clear
    // of a small level would be clamped against a bound it cannot reach and overhang its own plane.
    let (fmt, tw, th) = {
        let t = texture(res, texture_id)?;
        let (w, h) = Texture::level_size(&t.desc, mip_level);
        (t.desc.format, w, h)
    };
    let texel = fmt.software_clear_texel_f64(color)?;
    let bpt = texel.len();
    let x0 = x.min(tw) as usize;
    let y0 = y.min(th) as usize;
    let x1 = x.saturating_add(w).min(tw) as usize;
    let y1 = y.saturating_add(h).min(th) as usize;
    let tw = tw as usize;
    let t = texture_mut(res, texture_id)?;
    for layer in base_array_layer..base_array_layer.saturating_add(layer_count) {
        let plane = t
            .plane_at(mip_level, layer)
            .ok_or(GpuError::OutOfBounds)?
            .start;
        for yy in y0..y1 {
            for xx in x0..x1 {
                let off = plane + (yy * tw + xx) * bpt;
                t.pixels[off..off + bpt].copy_from_slice(&texel);
            }
        }
    }
    Ok(())
}

/// A `BeginRenderPass` `LoadOp::Clear` on a depth (or combined depth+stencil) attachment. A `Depth32Float`
/// plane is filled with the packed clear depth (little-endian f32, one per texel); a `Depth24PlusStencil8`
/// plane is filled per texel with `[depth: f32 le | stencil: clear_stencil as u8 @ byte 4 | 0,0,0]` — the
/// oracle-internal 8-byte layout the depth/stencil rasterizer reads (`clear_stencil` is truncated to the
/// 8-bit stencil buffer). Formats without a depth aspect are left unchanged.
pub(crate) fn clear_depth_stencil_target(
    res: &mut SessionResources,
    texture_id: u32,
    clear_depth: f32,
    clear_stencil: u32,
) -> Result<()> {
    let (fmt, w, h) = {
        let t = texture(res, texture_id)?;
        (t.desc.format, t.desc.width as usize, t.desc.height as usize)
    };
    let n = w * h;
    let t = texture_mut(res, texture_id)?;
    match fmt {
        TextureFormat::Depth32Float => {
            let bytes = clear_depth.to_le_bytes();
            t.pixels.clear();
            t.pixels.reserve(n * 4);
            for _ in 0..n {
                t.pixels.extend_from_slice(&bytes);
            }
        }
        TextureFormat::Depth24PlusStencil8 => {
            let d = clear_depth.to_le_bytes();
            let s = clear_stencil as u8;
            let texel = [d[0], d[1], d[2], d[3], s, 0, 0, 0];
            t.pixels.clear();
            t.pixels.reserve(n * 8);
            for _ in 0..n {
                t.pixels.extend_from_slice(&texel);
            }
        }
        _ => {}
    }
    Ok(())
}
