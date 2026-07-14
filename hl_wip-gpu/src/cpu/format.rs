//! Pixel/texel format helpers: sRGB transfer functions, channel-order permutation, texel load/store in
//! linear light, clear-color packing, and bilinear sampling. Ported verbatim (meaning-for-meaning) from
//! the free functions in `hl-gpu/src/software.rs`. These are the byte-exact rules the oracle's clears,
//! blends, copies, and blits round-trip through — every constant here is load-bearing for conformance.

use crate::protocol::model::enums::TextureFormat;
use crate::protocol::model::error::{GpuError, Result};

/// Bytes per texel for a color format, or a typed error for a non-color (depth/stencil) format the
/// software oracle cannot materialize.
pub(crate) fn texel_bytes(fmt: TextureFormat) -> Result<usize> {
    fmt.bytes_per_texel().ok_or(GpuError::Unsupported("software: non-color texture format"))
}

/// Convert a normalized clear color to packed bytes for the 8-bit color formats (round-half-up).
pub(crate) fn clear_texel(fmt: TextureFormat, c: [f32; 4]) -> Result<Vec<u8>> {
    let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    Ok(match fmt {
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Srgb => {
            vec![to_u8(c[0]), to_u8(c[1]), to_u8(c[2]), to_u8(c[3])]
        }
        TextureFormat::Bgra8Unorm | TextureFormat::Bgra8Srgb => {
            vec![to_u8(c[2]), to_u8(c[1]), to_u8(c[0]), to_u8(c[3])]
        }
        TextureFormat::R8Unorm => vec![to_u8(c[0])],
        TextureFormat::Rg8Unorm => vec![to_u8(c[0]), to_u8(c[1])],
        _ => return Err(GpuError::Unsupported("software: clear for this format")),
    })
}

/// The IEC 61966-2-1 sRGB EOTF on a normalized [0,1] value (electrical → linear-light optical).
pub(crate) fn srgb_to_linear(x: f32) -> f32 {
    if x <= 0.04045 {
        x / 12.92
    } else {
        ((x + 0.055) / 1.055).powf(2.4)
    }
}
fn srgb_decode(v: u8) -> f32 {
    srgb_to_linear(v as f32 / 255.0)
}
fn srgb_encode(v: f32) -> u8 {
    let x = v.clamp(0.0, 1.0);
    let y = if x <= 0.0031308 { x * 12.92 } else { 1.055 * x.powf(1.0 / 2.4) - 0.055 };
    (y * 255.0 + 0.5) as u8
}
pub(crate) fn is_srgb(f: TextureFormat) -> bool {
    matches!(f, TextureFormat::Rgba8Srgb | TextureFormat::Bgra8Srgb)
}

/// Logical-RGBA → byte-offset permutation for the oracle's 4-channel color formats. `Bgra*` stores blue
/// and red swapped; alpha is always the last byte. Returns `None` for a non-4-channel format.
pub(crate) fn rgba_channel_order(f: TextureFormat) -> Option<[usize; 4]> {
    match f {
        TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Srgb => Some([0, 1, 2, 3]),
        TextureFormat::Bgra8Unorm | TextureFormat::Bgra8Srgb => Some([2, 1, 0, 3]),
        _ => None,
    }
}

/// Decode a stored 4-byte texel into straight (non-premultiplied) linear-light RGBA in [0,1].
pub(crate) fn load_texel_linear(bytes: &[u8], order: [usize; 4], srgb: bool) -> [f32; 4] {
    let dec = |b: u8| if srgb { srgb_decode(b) } else { b as f32 / 255.0 };
    [dec(bytes[order[0]]), dec(bytes[order[1]]), dec(bytes[order[2]]), bytes[order[3]] as f32 / 255.0]
}

/// Encode straight linear-light RGBA back into a stored 4-byte texel (inverse of [`load_texel_linear`]).
pub(crate) fn store_texel_linear(bytes: &mut [u8], order: [usize; 4], srgb: bool, rgba: [f32; 4]) {
    let enc = |v: f32| if srgb { srgb_encode(v) } else { (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8 };
    bytes[order[0]] = enc(rgba[0]);
    bytes[order[1]] = enc(rgba[1]);
    bytes[order[2]] = enc(rgba[2]);
    bytes[order[3]] = (rgba[3].clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
}

/// Fetch one texel (`bpt` bytes) from a tight-packed level-0 plane at `(x, y)`.
pub(crate) fn texel_at(pixels: &[u8], tex_w: usize, x: usize, y: usize, bpt: usize) -> &[u8] {
    let off = (y * tex_w + x) * bpt;
    &pixels[off..off + bpt]
}

/// Bilinearly sample a tight-packed color plane at fractional `(fx, fy)` (in absolute texel space),
/// clamping neighbors to `[lo, hi]` in each axis — the oracle's `Filter::Linear` blit path.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sample_bilinear(
    pixels: &[u8],
    tex_w: usize,
    bpt: usize,
    fx: f32,
    fy: f32,
    x_lo: usize,
    x_hi: usize,
    y_lo: usize,
    y_hi: usize,
    format: TextureFormat,
) -> Vec<u8> {
    let gx = (fx - 0.5).clamp(x_lo as f32, x_hi as f32);
    let gy = (fy - 0.5).clamp(y_lo as f32, y_hi as f32);
    let x0 = gx.floor() as usize;
    let y0 = gy.floor() as usize;
    let x1 = (x0 + 1).min(x_hi);
    let y1 = (y0 + 1).min(y_hi);
    let tx = gx - x0 as f32;
    let ty = gy - y0 as f32;
    let p00 = texel_at(pixels, tex_w, x0, y0, bpt);
    let p10 = texel_at(pixels, tex_w, x1, y0, bpt);
    let p01 = texel_at(pixels, tex_w, x0, y1, bpt);
    let p11 = texel_at(pixels, tex_w, x1, y1, bpt);
    let mut out = Vec::with_capacity(bpt);
    for c in 0..bpt {
        let cv = |v: u8| if c < 3 && is_srgb(format) { srgb_decode(v) } else { v as f32 / 255.0 };
        let top = cv(p00[c]) * (1.0 - tx) + cv(p10[c]) * tx;
        let bot = cv(p01[c]) * (1.0 - tx) + cv(p11[c]) * tx;
        let v = top * (1.0 - ty) + bot * ty;
        out.push(if c < 3 && is_srgb(format) { srgb_encode(v) } else { (v * 255.0 + 0.5) as u8 });
    }
    out
}
