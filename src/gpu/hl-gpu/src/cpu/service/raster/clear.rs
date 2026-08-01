pub(crate) fn clear_target(
    res: &mut SessionResources,
    texture_id: u32,
    color: [f32; 4],
) -> Result<()> {
    let (fmt, w, h) = {
        let t = texture(res, texture_id)?;
        (t.desc.format, t.desc.width, t.desc.height)
    };
    let texel = fmt.software_clear_texel(color)?;
    let t = texture_mut(res, texture_id)?;
    let n = (w * h) as usize;
    t.pixels.clear();
    t.pixels.reserve(n * texel.len());
    for _ in 0..n {
        t.pixels.extend_from_slice(&texel);
    }
    Ok(())
}

/// `ClearRect`: fill only the covered sub-rectangle of a texture with the packed clear color.
pub(crate) fn clear_rect(
    res: &mut SessionResources,
    texture_id: u32,
    x: u32,
    y: u32,
    w: u32,
    h: u32,
    color: [f32; 4],
) -> Result<()> {
    let (fmt, tw, th) = {
        let t = texture(res, texture_id)?;
        (t.desc.format, t.desc.width, t.desc.height)
    };
    let texel = fmt.software_clear_texel(color)?;
    let bpt = texel.len();
    let x0 = x.min(tw) as usize;
    let y0 = y.min(th) as usize;
    let x1 = x.saturating_add(w).min(tw) as usize;
    let y1 = y.saturating_add(h).min(th) as usize;
    let tw = tw as usize;
    let t = texture_mut(res, texture_id)?;
    for yy in y0..y1 {
        for xx in x0..x1 {
            let off = (yy * tw + xx) * bpt;
            t.pixels[off..off + bpt].copy_from_slice(&texel);
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
use super::*;
