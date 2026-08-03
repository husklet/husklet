//! Texture-upload layout and pixel conversion.
//!
//! The EGL ABI adapter resolves a client pointer (or pixel-unpack-buffer offset) to bytes. This module
//! owns the GL pixel-store layout and format conversion, keeping unsafe pointer access out of the domain
//! operation and making packed browser glyph-atlas uploads directly testable.

use crate::model::context::PixelStore;
use crate::model::glconst::*;
use hl_gpu::protocol::model::enums::TextureFormat;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Upload {
    format: u32,
    type_: u32,
    /// Component count of the source FORMAT (1..=4).
    channels: usize,
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
        let channels: usize = match format {
            GL_RED | GL_ALPHA | GL_LUMINANCE | GL_RED_INTEGER | GL_DEPTH_COMPONENT => 1,
            GL_RG | GL_LUMINANCE_ALPHA | GL_RG_INTEGER => 2,
            GL_RGB | GL_RGB_INTEGER => 3,
            GL_RGBA | GL_BGRA_EXT | GL_RGBA_INTEGER => 4,
            _ => return None,
        };
        // The component width of the source type. The model materializes an RGBA8 plane whatever the
        // declared format, so a wider or signed component is narrowed at conversion — an honest partial.
        // What matters here is that the upload is ACCEPTED: refusing these types made `glTexSubImage2D`
        // into immutable storage fail for thirty-two sized formats with GL_INVALID_VALUE, which left
        // `glTexStorage2D` broadly unusable.
        let source_bpp = match type_ {
            GL_UNSIGNED_BYTE | GL_BYTE => channels,
            GL_UNSIGNED_SHORT | GL_SHORT | GL_HALF_FLOAT => channels.checked_mul(2)?,
            GL_UNSIGNED_INT | GL_INT | GL_FLOAT => channels.checked_mul(4)?,
            GL_UNSIGNED_SHORT_5_6_5 if format == GL_RGB => 2,
            // ES 3.0 table 3.2 pairs each packed type with exactly one format. Refusing these made the
            // whole upload fail, which is why an RGBA4 / RGB5_A1 / RGB10_A2 texture sampled as all-zero
            // while its `glTexImage2D` reported no error at all.
            GL_UNSIGNED_SHORT_4_4_4_4 | GL_UNSIGNED_SHORT_5_5_5_1 if format == GL_RGBA => 2,
            GL_UNSIGNED_INT_2_10_10_10_REV if format == GL_RGBA || format == GL_RGBA_INTEGER => 4,
            GL_UNSIGNED_INT_5_9_9_9_REV | GL_UNSIGNED_INT_10F_11F_11F_REV if format == GL_RGB => 4,
            GL_UNSIGNED_INT_24_8 if format == GL_DEPTH_STENCIL => 4,
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
            channels,
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

    /// Whether this upload's FORMAT is an integer one, whose texels are stored raw at their own channel
    /// count rather than expanded to RGBA8 (see [`Self::rgba8`]).
    pub fn is_integer(self) -> bool {
        matches!(
            self.format,
            GL_RED_INTEGER | GL_RG_INTEGER | GL_RGB_INTEGER | GL_RGBA_INTEGER
        )
    }

    /// The bytes-per-texel of the produced plane: four for the RGBA8 shadow every normalized format
    /// materializes, and the channel count for an 8-bit integer format, whose plane is `R8`/`Rg8`/`Rgba8`
    /// of raw integers.
    ///
    /// Restricted to 8-BIT integer sources on purpose: those are exactly the formats with real integer
    /// storage in the IR (`GL_R8UI`/`GL_RG8UI`/`GL_RGBA8UI` and their signed siblings). A wider integer
    /// format has no IR storage, still resolves to an `Rgba8Unorm` plane, and must therefore keep
    /// producing four bytes per texel — otherwise the plane and the texture's declared format disagree
    /// about its size and the upload's row pitch walks off the end of it. Three-channel integer formats
    /// have no IR storage either and take the same RGBA8 path.
    pub fn destination_bpt(self) -> usize {
        let eight_bit = matches!(self.type_, GL_UNSIGNED_BYTE | GL_BYTE);
        if self.is_integer() && eight_bit && self.channels != 3 {
            self.channels
        } else {
            4
        }
    }

    /// Convert this upload into texels of the DESTINATION plane's format.
    ///
    /// The narrowing to eight bits per channel that [`Self::rgba8`] performs happens BEFORE the
    /// destination is known, which is why it cannot serve a float plane: a `GL_FLOAT` source routed
    /// through it is clamped to `0..=1` and quantised to 256 levels, and re-widening that to half-float
    /// afterwards recovers nothing. A float destination therefore reads its source as floating-point
    /// channels and encodes them directly.
    ///
    /// A float destination whose source type this cannot read as a real value refuses the upload rather
    /// than substituting narrowed bytes — the texture stays data-less and is truthfully skipped at draw
    /// time, which is the same answer an unmodeled format has always received here.
    pub fn plane(self, source: &[u8], destination: TextureFormat) -> Option<Vec<u8>> {
        match destination {
            TextureFormat::Rgba16Float => self.float_plane(source, FloatTexel::Half4),
            TextureFormat::Rgba32Float => self.float_plane(source, FloatTexel::Single4),
            TextureFormat::R32Float => self.float_plane(source, FloatTexel::Single1),
            _ => self.rgba8(source),
        }
    }

    fn float_plane(self, source: &[u8], texel: FloatTexel) -> Option<Vec<u8>> {
        if source.len() < self.source_len {
            return None;
        }
        // Refuse before allocating: a source whose components cannot be read as values has nothing to
        // contribute to a float plane, and writing zeros would be indistinguishable from a black image.
        if !self.reads_as_float() {
            return None;
        }
        let mut out = Vec::with_capacity(
            self.width
                .checked_mul(self.height)?
                .checked_mul(texel.bytes())?,
        );
        for y in 0..self.height {
            let begin = self.start + y * self.stride;
            let row = &source[begin..begin + self.width * self.source_bpp];
            for pixel in row.chunks_exact(self.source_bpp) {
                let rgba = self.rgba_f32(pixel)?;
                texel.encode(rgba, &mut out);
            }
        }
        Some(out)
    }

    /// One source texel as RGBA floating-point channels, with the source FORMAT's channel mapping applied
    /// — the same mapping [`Self::rgba8`] applies to bytes (ES 2.0 table 3.11: a `GL_ALPHA` texel is
    /// `(0, 0, 0, A)`, a `GL_LUMINANCE` texel is `(L, L, L, 1)`).
    fn rgba_f32(self, pixel: &[u8]) -> Option<[f32; 4]> {
        let mut source = [0.0f32; 4];
        for channel in 0..self.channels {
            source[channel] = self.component_value(pixel, channel)?;
        }
        let [a, b, c, d] = source;
        Some(match self.format {
            GL_RED | GL_RED_INTEGER | GL_DEPTH_COMPONENT => [a, 0.0, 0.0, 1.0],
            GL_ALPHA => [0.0, 0.0, 0.0, a],
            GL_LUMINANCE => [a, a, a, 1.0],
            GL_RG | GL_RG_INTEGER => [a, b, 0.0, 1.0],
            GL_LUMINANCE_ALPHA => [a, a, a, b],
            GL_RGB | GL_RGB_INTEGER => [a, b, c, 1.0],
            GL_BGRA_EXT => [c, b, a, d],
            _ => [a, b, c, d],
        })
    }

    /// Whether this upload's component type carries a value a float plane can hold. The packed and
    /// integer-typed sources do not: their texels are bit fields or counts, and a float destination has no
    /// meaning for them. `GL_UNSIGNED_INT_10F_11F_11F_REV` and `GL_UNSIGNED_INT_5_9_9_9_REV` are the two
    /// packed types that genuinely carry floating-point values and are the obvious next additions here;
    /// they are refused rather than guessed at, so an application using them gets a data-less texture it
    /// can see instead of a plane of nonsense it cannot.
    fn reads_as_float(self) -> bool {
        matches!(
            self.type_,
            GL_UNSIGNED_BYTE | GL_BYTE | GL_UNSIGNED_SHORT | GL_SHORT | GL_HALF_FLOAT | GL_FLOAT
        )
    }

    /// One component of a source texel as a floating-point value: normalized types are scaled to their
    /// `0..=1` (or `-1..=1`) range, float types are read as they are, and an integer or packed type
    /// answers `None` — it has no value a float plane can carry.
    fn component_value(self, pixel: &[u8], channel: usize) -> Option<f32> {
        let width = match self.type_ {
            GL_UNSIGNED_BYTE | GL_BYTE => 1,
            GL_UNSIGNED_SHORT | GL_SHORT | GL_HALF_FLOAT => 2,
            GL_FLOAT => 4,
            _ => return None,
        };
        let base = channel * width;
        Some(match self.type_ {
            GL_UNSIGNED_BYTE => *pixel.get(base)? as f32 / 255.0,
            // ES 3.0 §2.3.5: a signed normalized value is `max(c / (2^(b-1) - 1), -1)`.
            GL_BYTE => (*pixel.get(base)? as i8 as f32 / 127.0).max(-1.0),
            GL_UNSIGNED_SHORT => {
                u16::from_le_bytes(pixel.get(base..base + 2)?.try_into().ok()?) as f32 / 65535.0
            }
            GL_SHORT => (i16::from_le_bytes(pixel.get(base..base + 2)?.try_into().ok()?) as f32
                / 32767.0)
                .max(-1.0),
            GL_HALF_FLOAT => hl_gpu::protocol::model::half::to_f32(u16::from_le_bytes(
                pixel.get(base..base + 2)?.try_into().ok()?,
            )),
            GL_FLOAT => f32::from_le_bytes(pixel.get(base..base + 4)?.try_into().ok()?),
            _ => return None,
        })
    }

    pub fn rgba8(self, source: &[u8]) -> Option<Vec<u8>> {
        if source.len() < self.source_len {
            return None;
        }
        let bpt = self.destination_bpt();
        let mut out = Vec::with_capacity(self.width.checked_mul(self.height)?.checked_mul(bpt)?);
        for y in 0..self.height {
            let begin = self.start + y * self.stride;
            let row = &source[begin..begin + self.width * self.source_bpp];
            for pixel in row.chunks_exact(self.source_bpp) {
                // An INTEGER texel is stored RAW at its own channel count: the value is a number, not a
                // colour, so it is neither normalized nor padded out to four channels. A three-channel
                // integer format has no IR storage and still takes the RGBA8 path below.
                if bpt != 4 {
                    for channel in 0..bpt {
                        out.push(self.integer_channel(pixel, channel));
                    }
                    continue;
                }

                // The packed types (ES 3.0 table 3.2). Each component is an unsigned normalized integer of
                // its own width, expanded to 8 bits by [`unorm8`]. Bit positions are MSB-first for the
                // `_5_6_5` / `_4_4_4_4` / `_5_5_5_1` types and LSB-first (`_REV`) for `_2_10_10_10_REV`.
                if let Some(packed) = self.packed(pixel) {
                    out.extend_from_slice(&packed);
                    continue;
                }
                // A non-byte component type is narrowed to 8 bits per channel (see `Upload::new`).
                if let Some(narrowed) = self.narrowed(pixel) {
                    out.extend_from_slice(&narrowed);
                    continue;
                }
                match self.format {
                    GL_RED | GL_RED_INTEGER | GL_DEPTH_COMPONENT => {
                        out.extend_from_slice(&[pixel[0], 0, 0, 0xff])
                    }
                    GL_RG_INTEGER => out.extend_from_slice(&[pixel[0], pixel[1], 0, 0xff]),
                    GL_RGB_INTEGER => out.extend_from_slice(&[pixel[0], pixel[1], pixel[2], 0xff]),
                    GL_RGBA_INTEGER => out.extend_from_slice(pixel),
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

impl Upload {
    /// Narrow one texel whose components are WIDER than a byte (or signed) into RGBA8, or `None` when the
    /// components are already unsigned bytes and the per-format path applies.
    ///
    /// The model materializes an RGBA8 plane whatever the declared format, so this is where precision is
    /// lost. Integer formats carry a numeric value and are clamped to `0..=255`; float formats carry a
    /// normalized value and are scaled by 255. Both are honest partials, and both are better than the
    /// refusal they replace — the whole upload used to fail.
    fn narrowed(self, pixel: &[u8]) -> Option<[u8; 4]> {
        let integer = matches!(
            self.format,
            GL_RED_INTEGER | GL_RG_INTEGER | GL_RGB_INTEGER | GL_RGBA_INTEGER
        );
        let channels = pixel.len() / self.component_width()?;
        let mut out = [0u8, 0, 0, 0xff];
        for channel in 0..channels.min(4) {
            let base = channel * self.component_width()?;
            let value = match self.type_ {
                GL_BYTE => (pixel[base] as i8).max(0) as u8,
                GL_UNSIGNED_SHORT => {
                    let raw = u16::from_le_bytes([pixel[base], pixel[base + 1]]);
                    if integer {
                        raw.min(255) as u8
                    } else {
                        (raw / 257) as u8
                    }
                }
                GL_SHORT => {
                    let raw = i16::from_le_bytes([pixel[base], pixel[base + 1]]);
                    raw.clamp(0, 255) as u8
                }
                GL_UNSIGNED_INT => {
                    let raw = u32::from_le_bytes(pixel[base..base + 4].try_into().ok()?);
                    raw.min(255) as u8
                }
                GL_INT => {
                    let raw = i32::from_le_bytes(pixel[base..base + 4].try_into().ok()?);
                    raw.clamp(0, 255) as u8
                }
                GL_FLOAT => {
                    let raw = f32::from_le_bytes(pixel[base..base + 4].try_into().ok()?);
                    (raw.clamp(0.0, 1.0) * 255.0).round() as u8
                }
                _ => return None,
            };
            out[channel] = value;
        }
        // A one- or two-channel source leaves the remaining colour channels at zero and alpha opaque,
        // matching the per-format byte path above.
        if channels < 4 {
            out[3] = 0xff;
        }
        Some(out)
    }

    /// The byte width of ONE component of this upload's type, or `None` for a packed type (whose whole
    /// texel is decoded by [`Self::packed`]) or an unsigned-byte type (the per-format path).
    fn component_width(self) -> Option<usize> {
        Some(match self.type_ {
            GL_BYTE => 1,
            GL_UNSIGNED_SHORT | GL_SHORT | GL_HALF_FLOAT => 2,
            GL_UNSIGNED_INT | GL_INT | GL_FLOAT => 4,
            _ => return None,
        })
    }
}

impl Upload {
    /// One channel of an INTEGER texel, as the raw integer value truncated to the 8-bit plane. Signed
    /// sources keep their two's-complement byte, so a `GL_BYTE` value of -120 stores as `0x88` and reads
    /// back as -120 through an `R8Sint`/`Rgba8Sint` view.
    fn integer_channel(self, pixel: &[u8], channel: usize) -> u8 {
        let width = match self.type_ {
            GL_UNSIGNED_BYTE | GL_BYTE => 1,
            GL_UNSIGNED_SHORT | GL_SHORT => 2,
            GL_UNSIGNED_INT | GL_INT => 4,
            // A packed integer type (`GL_UNSIGNED_INT_2_10_10_10_REV` with `GL_RGBA_INTEGER`) has no
            // per-channel byte; the packed decoder already produced its components.
            _ => return self.packed(pixel).map(|texel| texel[channel]).unwrap_or(0),
        };
        // The low byte of the component: an integer wider than the plane truncates rather than scaling.
        pixel.get(channel * width).copied().unwrap_or(0)
    }
}

/// The texel encoding of a floating-point destination plane: how many channels it stores and at what
/// width. One place so the plane's byte size and the bytes written into it cannot come apart, which is
/// the disagreement the whole shadow change exists to remove.
#[derive(Clone, Copy, Debug)]
enum FloatTexel {
    /// `Rgba16Float` — four IEEE 754 binary16 channels.
    Half4,
    /// `Rgba32Float` — four IEEE 754 binary32 channels.
    Single4,
    /// `R32Float` — one binary32 channel. The plane's four bytes are one float, not four unsigned bytes;
    /// filling it with RGBA8 is how a `GL_R32F` texture came to hold colour bytes read as a float.
    Single1,
}

impl FloatTexel {
    fn bytes(self) -> usize {
        match self {
            Self::Half4 => 8,
            Self::Single4 => 16,
            Self::Single1 => 4,
        }
    }

    fn encode(self, rgba: [f32; 4], out: &mut Vec<u8>) {
        match self {
            Self::Half4 => {
                for channel in rgba {
                    out.extend_from_slice(
                        &hl_gpu::protocol::model::half::from_f32(channel).to_le_bytes(),
                    );
                }
            }
            Self::Single4 => {
                for channel in rgba {
                    out.extend_from_slice(&channel.to_le_bytes());
                }
            }
            Self::Single1 => out.extend_from_slice(&rgba[0].to_le_bytes()),
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
