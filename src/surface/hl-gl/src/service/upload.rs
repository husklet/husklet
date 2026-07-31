//! Texture-upload layout and pixel conversion.
//!
//! The EGL ABI adapter resolves a client pointer (or pixel-unpack-buffer offset) to bytes. This module
//! owns the GL pixel-store layout and format conversion, keeping unsafe pointer access out of the domain
//! operation and making packed browser glyph-atlas uploads directly testable.

use crate::model::context::PixelStore;
use crate::model::glconst::*;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Upload {
    format: u32,
    type_: u32,
    width: usize,
    height: usize,
    source_bpp: usize,
    stride: usize,
    start: usize,
    source_len: usize,
}

impl Upload {
    pub fn new(
        format: u32,
        type_: u32,
        width: i32,
        height: i32,
        store: PixelStore,
    ) -> Option<Self> {
        let (width, height) = (usize::try_from(width).ok()?, usize::try_from(height).ok()?);
        if width == 0 || height == 0 {
            return None;
        }
        let channels = match format {
            GL_RED | GL_ALPHA | GL_LUMINANCE => 1,
            GL_RG | GL_LUMINANCE_ALPHA => 2,
            GL_RGB => 3,
            GL_RGBA | GL_BGRA_EXT => 4,
            _ => return None,
        };
        let source_bpp = match type_ {
            GL_UNSIGNED_BYTE => channels,
            GL_UNSIGNED_SHORT_5_6_5 if format == GL_RGB => 2,
            // ES 3.0 table 3.2 pairs each packed type with exactly one format. Refusing these made the
            // whole upload fail, which is why an RGBA4 / RGB5_A1 / RGB10_A2 texture sampled as all-zero
            // while its `glTexImage2D` reported no error at all.
            GL_UNSIGNED_SHORT_4_4_4_4 | GL_UNSIGNED_SHORT_5_5_5_1 if format == GL_RGBA => 2,
            GL_UNSIGNED_INT_2_10_10_10_REV if format == GL_RGBA => 4,
            _ => return None,
        };
        let row_pixels = if store.unpack_row_length > 0 {
            store.unpack_row_length as usize
        } else {
            width
        };
        let alignment = store.unpack_alignment.max(1) as usize;
        let raw_row = row_pixels.checked_mul(source_bpp)?;
        let stride = raw_row.checked_add(alignment - 1)? / alignment * alignment;
        let start = (store.unpack_skip_rows as usize)
            .checked_mul(stride)?
            .checked_add((store.unpack_skip_pixels as usize).checked_mul(source_bpp)?)?;
        let source_len = (height - 1)
            .checked_mul(stride)?
            .checked_add(width.checked_mul(source_bpp)?)?
            .checked_add(start)?;
        Some(Self {
            format,
            type_,
            width,
            height,
            source_bpp,
            stride,
            start,
            source_len,
        })
    }

    pub fn source_len(self) -> usize {
        self.source_len
    }

    pub fn rgba8(self, source: &[u8]) -> Option<Vec<u8>> {
        if source.len() < self.source_len {
            return None;
        }
        let mut out = Vec::with_capacity(self.width.checked_mul(self.height)?.checked_mul(4)?);
        for y in 0..self.height {
            let begin = self.start + y * self.stride;
            let row = &source[begin..begin + self.width * self.source_bpp];
            for pixel in row.chunks_exact(self.source_bpp) {
                // The packed types (ES 3.0 table 3.2). Each component is an unsigned normalized integer of
                // its own width, expanded to 8 bits by [`unorm8`]. Bit positions are MSB-first for the
                // `_5_6_5` / `_4_4_4_4` / `_5_5_5_1` types and LSB-first (`_REV`) for `_2_10_10_10_REV`.
                if let Some(packed) = self.packed(pixel) {
                    out.extend_from_slice(&packed);
                    continue;
                }
                match self.format {
                    GL_RED => out.extend_from_slice(&[pixel[0], 0, 0, 0xff]),
                    // ES 2.0 table 3.11: a GL_ALPHA texel samples as (0, 0, 0, A) — the RGB is ZERO, not
                    // one. Glyph and mask atlases are GL_ALPHA, so white RGB here tinted every masked
                    // draw white instead of leaving the mask's own colour to the shader.
                    GL_ALPHA => out.extend_from_slice(&[0, 0, 0, pixel[0]]),
                    GL_LUMINANCE => out.extend_from_slice(&[pixel[0], pixel[0], pixel[0], 0xff]),
                    GL_RG => out.extend_from_slice(&[pixel[0], pixel[1], 0, 0xff]),
                    GL_LUMINANCE_ALPHA => {
                        out.extend_from_slice(&[pixel[0], pixel[0], pixel[0], pixel[1]])
                    }
                    GL_RGB => out.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 0xff]),
                    GL_RGBA => out.extend_from_slice(pixel),
                    GL_BGRA_EXT => out.extend_from_slice(&[pixel[2], pixel[1], pixel[0], pixel[3]]),
                    _ => unreachable!(),
                }
            }
        }
        Some(out)
    }

    /// Decode one texel of a PACKED pixel type into RGBA8, or `None` when the upload's type is not packed
    /// (the caller then takes the per-channel path). Component order follows ES 3.0 table 3.2.
    fn packed(self, pixel: &[u8]) -> Option<[u8; 4]> {
        match self.type_ {
            GL_UNSIGNED_SHORT_5_6_5 => {
                let packed = u16::from_ne_bytes([pixel[0], pixel[1]]);
                Some([
                    unorm8((packed >> 11) as u32 & 0x1f, 5),
                    unorm8((packed >> 5) as u32 & 0x3f, 6),
                    unorm8(packed as u32 & 0x1f, 5),
                    0xff,
                ])
            }
            GL_UNSIGNED_SHORT_4_4_4_4 => {
                let packed = u16::from_ne_bytes([pixel[0], pixel[1]]);
                Some([
                    unorm8((packed >> 12) as u32 & 0xf, 4),
                    unorm8((packed >> 8) as u32 & 0xf, 4),
                    unorm8((packed >> 4) as u32 & 0xf, 4),
                    unorm8(packed as u32 & 0xf, 4),
                ])
            }
            GL_UNSIGNED_SHORT_5_5_5_1 => {
                let packed = u16::from_ne_bytes([pixel[0], pixel[1]]);
                Some([
                    unorm8((packed >> 11) as u32 & 0x1f, 5),
                    unorm8((packed >> 6) as u32 & 0x1f, 5),
                    unorm8((packed >> 1) as u32 & 0x1f, 5),
                    // A one-bit alpha is 0 or 255; the general expansion already yields exactly that.
                    unorm8(packed as u32 & 0x1, 1),
                ])
            }
            GL_UNSIGNED_INT_2_10_10_10_REV => {
                let packed = u32::from_ne_bytes([pixel[0], pixel[1], pixel[2], pixel[3]]);
                Some([
                    unorm8(packed & 0x3ff, 10),
                    unorm8((packed >> 10) & 0x3ff, 10),
                    unorm8((packed >> 20) & 0x3ff, 10),
                    unorm8((packed >> 30) & 0x3, 2),
                ])
            }
            _ => None,
        }
    }
}

/// Expand a `bits`-wide unsigned normalized integer to 8 bits.
///
/// ES 3.0 §2.3.5 defines the value as `f = c / (2^bits − 1)`, and the 8-bit result as `round(f × 255)`.
/// Both steps matter: this was `c * 255 / bits_max`, whose integer division TRUNCATES, so a 6-bit green of
/// 57 gave `14535 / 63 = 230` where the spec requires `round(230.714) = 231`. The error is invisible in
/// channels where the two happen to coincide — which is why a 565 texel could look half-right — and it
/// applies to every packed format, not just 565.
fn unorm8(value: u32, bits: u32) -> u8 {
    let max = (1u32 << bits) - 1;
    (((value.min(max) * 255) + max / 2) / max) as u8
}
