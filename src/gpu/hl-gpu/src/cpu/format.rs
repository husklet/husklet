//! Pixel/texel format helpers: sRGB transfer functions, channel-order permutation, texel load/store in
//! linear light, clear-color packing, and bilinear sampling. Ported verbatim (meaning-for-meaning) from
//! the free functions in `hl-gpu/src/software.rs`. These are the byte-exact rules the oracle's clears,
//! blends, copies, and blits round-trip through — every constant here is load-bearing for conformance.

use crate::protocol::model::enums::TextureFormat;
use crate::protocol::model::error::{GpuError, Result};

/// Bytes per texel for a color format, or a typed error for a non-color (depth/stencil) format the
/// software oracle cannot materialize.
impl TextureFormat {
    pub(crate) fn software_texel_bytes(self) -> Result<usize> {
        self.bytes_per_texel()
            .ok_or(GpuError::Unsupported("software: non-color texture format"))
    }

    /// Pack a normalized (linear-light) clear colour into one texel, or a typed refusal naming THIS
    /// backend. The packing rule itself belongs to the format and is shared with the wgpu executor
    /// ([`TextureFormat::clear_texel`]) — it used to be written out here and there separately, and the two
    /// disagreed on sRGB. Only the error message is the oracle's own, so a refusal still says who refused.
    pub(crate) fn software_clear_texel(self, c: [f32; 4]) -> Result<Vec<u8>> {
        TextureFormat::clear_texel(self, c)
            .ok_or(GpuError::Unsupported("software: clear for this format"))
    }

    pub(crate) fn software_clear_texel_f64(self, c: [f64; 4]) -> Result<Vec<u8>> {
        TextureFormat::clear_texel_f64(self, c)
            .ok_or(GpuError::Unsupported("software: clear for this format"))
    }

    fn srgb_decode(v: u8) -> f32 {
        Self::srgb_to_linear(v as f32 / 255.0)
    }

    /// Logical-RGBA → byte-offset permutation for the oracle's 4-channel color formats. `Bgra*` stores blue
    /// and red swapped; alpha is always the last byte. Returns `None` for a non-4-channel format.
    pub(crate) fn rgba_channel_order(self) -> Option<[usize; 4]> {
        match self {
            TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Srgb => Some([0, 1, 2, 3]),
            TextureFormat::Bgra8Unorm | TextureFormat::Bgra8Srgb => Some([2, 1, 0, 3]),
            _ => None,
        }
    }
}

/// Decode a stored 4-byte texel into straight (non-premultiplied) linear-light RGBA in [0,1].
pub(crate) fn load_texel_linear(bytes: &[u8], order: [usize; 4], srgb: bool) -> [f32; 4] {
    let dec = |b: u8| {
        if srgb {
            TextureFormat::srgb_decode(b)
        } else {
            b as f32 / 255.0
        }
    };
    [
        dec(bytes[order[0]]),
        dec(bytes[order[1]]),
        dec(bytes[order[2]]),
        bytes[order[3]] as f32 / 255.0,
    ]
}

/// Encode straight linear-light RGBA back into a stored 4-byte texel (inverse of [`load_texel_linear`]).
pub(crate) fn store_texel_linear(bytes: &mut [u8], order: [usize; 4], srgb: bool, rgba: [f32; 4]) {
    let enc = |v: f32| {
        if srgb {
            TextureFormat::srgb_encode(v)
        } else {
            (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8
        }
    };
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

/// Bilinearly sample a tight-packed colour plane at fractional `(fx, fy)` (in absolute texel space),
/// clamping neighbours to `[lo, hi]` in each axis — the oracle's `Filter::Linear` blit path.
///
/// Interpolation happens on VALUES, decoded through the format's own rule and re-encoded through its
/// inverse. It used to happen on raw BYTES: every plane was read as `bpt` independent unsigned-normalized
/// channels, which is right for the eight-bit formats by coincidence of layout and meaningless for any
/// other. A half-float 2x1 blitted down to 1x1 averaged the two encodings byte-wise, so (1.0, 0, 0, 1)
/// and (0, 0, 1.0, 1) gave 0.0059 instead of 0.5 — the mean of the high bytes `0x3C` and `0x00` read back
/// as `0x1E00`. Nothing caught it: no test blitted a float plane, and the alpha channel came out correct
/// because both texels carried identical alpha bytes.
///
/// Returns the interpolated colour as VALUES; the caller encodes them into whatever format the
/// destination is. A format with no plain-colour texel yields `None`, as does an INTEGER format —
/// averaging raw integers has no defined meaning and both GL and Vulkan forbid linear filtering of
/// integer textures. The caller turns that into a typed refusal.
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
) -> Option<[f32; 4]> {
    if INTEGER_FILTER_REFUSED.contains(&format) || FILTERABLE_REFUSED.contains(&format) {
        return None;
    }
    let gx = (fx - 0.5).clamp(x_lo as f32, x_hi as f32);
    let gy = (fy - 0.5).clamp(y_lo as f32, y_hi as f32);
    let x0 = gx.floor() as usize;
    let y0 = gy.floor() as usize;
    let x1 = (x0 + 1).min(x_hi);
    let y1 = (y0 + 1).min(y_hi);
    let tx = gx - x0 as f32;
    let ty = gy - y0 as f32;
    let at = |x: usize, y: usize| format.texel_to_f32(texel_at(pixels, tex_w, x, y, bpt));
    let (p00, p10, p01, p11) = (at(x0, y0)?, at(x1, y0)?, at(x0, y1)?, at(x1, y1)?);
    let mut rgba = [0.0f32; 4];
    for c in 0..4 {
        let top = p00[c] * (1.0 - tx) + p10[c] * tx;
        let bot = p01[c] * (1.0 - tx) + p11[c] * tx;
        rgba[c] = top * (1.0 - ty) + bot * ty;
    }
    // Returns VALUES, not an encoding: the caller decides which format receives them, and a blit whose
    // destination differs from its source must encode once, at the destination, rather than round-trip
    // through the source's encoding on the way.
    Some(rgba)
}

/// Formats whose texels are raw integers, for which a linear filter has no defined meaning. Both GL and
/// Vulkan forbid linear filtering of an integer texture; averaging the values would produce a plausible
/// number that no specification asks for.
/// Formats the HOST cannot filter, so this reference must not either.
///
/// One of three independent layers that decline these two formats a linear filter; the others are the
/// executor's blit and the Vulkan surface's `FILTERABLE` list. `float_filter_agreement.rs` in
/// `hl-gpu-wgpu`'s tests binds the three so none can move alone, and records why the optional feature
/// stays off: the adapter measured DOES offer it, but it is adapter-dependent while these two lists are
/// compile-time, so enabling it would make the differential's answer depend on the host it ran on.
///
/// WebGPU makes the 32-bit float formats non-filterable unless `FLOAT32_FILTERABLE` is enabled, and the
/// executor refuses a linear blit from one. This oracle could interpolate them perfectly well in
/// software — and doing so would be the wrong kind of better: a reference that ACCEPTS what the subject
/// refuses is a false divergence, the same defect as refusing what the subject performs. Vulkan agrees
/// independently, forbidding a linear filter unless the source format supports linear filtering.
const FILTERABLE_REFUSED: &[TextureFormat] = &[
    TextureFormat::R32Float,
    TextureFormat::Rg32Float,
    TextureFormat::Rgba32Float,
];

const INTEGER_FILTER_REFUSED: &[TextureFormat] = &[
    TextureFormat::R8Uint,
    TextureFormat::R8Sint,
    TextureFormat::Rg8Uint,
    TextureFormat::Rg8Sint,
    TextureFormat::Rgba8Uint,
    TextureFormat::Rgba8Sint,
];
