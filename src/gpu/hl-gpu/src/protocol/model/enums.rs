//! The repr-`u32` wire enums (stable numeric constants) and the hand-rolled `u32` usage bitflags.
//!
//! Enum numbering for `Topology` / `LoadOp` / `Filter` follows WebGPU where noted; every value here is a
//! stable wire constant — changing one is a wire break. `from_u32` rejects an out-of-range value with a
//! typed [`GpuError::BadEnum`] so a malformed guest stream never aliases onto a valid variant.

use crate::protocol::model::error::{GpuError, Result};

// ---------------------------------------------------------------------------------------------------
// small helper for repr-u32 enums
// ---------------------------------------------------------------------------------------------------

macro_rules! u32_enum {
    ($(#[$m:meta])* $name:ident { $($variant:ident = $val:expr),+ $(,)? } $what:literal) => {
        $(#[$m])*
        #[derive(Clone, Copy, PartialEq, Eq, Debug)]
        #[repr(u32)]
        pub enum $name { $($variant = $val),+ }
        impl $name {
            pub fn to_u32(self) -> u32 { self as u32 }
            pub fn from_u32(v: u32) -> Result<Self> {
                match v { $($val => Ok($name::$variant),)+ _ => Err(GpuError::BadEnum { what: $what, val: v }) }
            }
        }
    };
}

u32_enum!(
    /// Pixel format for textures / render targets / swapchain images.
    TextureFormat {
        Rgba8Unorm = 1, Bgra8Unorm = 2, Rgba8Srgb = 3, Bgra8Srgb = 4,
        R8Unorm = 5, Rg8Unorm = 6, Rgba16Float = 7, Rgba32Float = 8,
        R32Float = 9, Depth32Float = 10, Depth24PlusStencil8 = 11,
        Bc1RgbaUnorm = 12, Bc1RgbaSrgb = 13, Bc2RgbaUnorm = 14, Bc2RgbaSrgb = 15,
        Bc3RgbaUnorm = 16, Bc3RgbaSrgb = 17, Bc4RUnorm = 18, Bc4RSnorm = 19,
        Bc5RgUnorm = 20, Bc5RgSnorm = 21, Bc6hRgbUfloat = 22, Bc6hRgbFloat = 23,
        Bc7RgbaUnorm = 24, Bc7RgbaSrgb = 25,
        // Integer color formats (v-next, additive). A GL texture declared `GL_RGBA_INTEGER` / `GL_RED_INTEGER`
        // / `GL_RG_INTEGER` — the storage a `usampler2D`/`isampler2D` reads — had NO representation in this
        // enum at all, so the whole integer-texture family was unexpressible rather than merely unsupported.
        // These are UNFILTERABLE and UNBLENDABLE by specification: a sampler reads them only through
        // `texelFetch` (WGSL `textureLoad`), and their texels are raw integers, never normalized.
        Rgba8Uint = 26, Rgba8Sint = 27, R8Uint = 28, R8Sint = 29, Rg8Uint = 30, Rg8Sint = 31,
        Rgba32Uint = 32, Rgba32Sint = 33, R32Uint = 34, R32Sint = 35,
        // Exact native Metal/WebGPU formats used by Vulkan. Keep these neutral: their numbers are
        // protocol values, not VkFormat or wgpu discriminants.
        Rg8Snorm = 36, Rgba8Snorm = 37, Rg16Float = 38,
        R16Float = 39, R16Uint = 40, R16Sint = 41, Rg16Uint = 42, Rg16Sint = 43,
        Rgba16Uint = 44, Rgba16Sint = 45, Rg32Float = 46, Rg32Uint = 47, Rg32Sint = 48,
        R8Snorm = 49,
        Etc2Rgb8Unorm = 50, Etc2Rgb8Srgb = 51, Etc2Rgb8A1Unorm = 52, Etc2Rgb8A1Srgb = 53,
        Etc2Rgba8Unorm = 54, Etc2Rgba8Srgb = 55, EacR11Unorm = 56, EacR11Snorm = 57,
        EacRg11Unorm = 58, EacRg11Snorm = 59,
        Rgb9e5Ufloat = 60,
        Rgb10a2Unorm = 61, Rgb10a2Uint = 62, Rg11b10Ufloat = 63,
        R5g6b5Unorm = 64, A1r5g5b5Unorm = 65, B4g4r4a4Unorm = 66,
        Rgba16Unorm = 67, Rg16Unorm = 68,
    } "TextureFormat"
);

/// Numeric class used by operations whose source and destination formats must agree.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TextureNumericClass {
    Float,
    Uint,
    Sint,
}

impl TextureFormat {
    pub fn numeric_class(self) -> TextureNumericClass {
        match self {
            TextureFormat::Rgba8Uint
            | TextureFormat::R8Uint
            | TextureFormat::Rg8Uint
            | TextureFormat::R32Uint
            | TextureFormat::R16Uint
            | TextureFormat::Rg16Uint
            | TextureFormat::Rgba16Uint
            | TextureFormat::Rg32Uint
            | TextureFormat::Rgba32Uint => TextureNumericClass::Uint,
            TextureFormat::Rgb10a2Uint => TextureNumericClass::Uint,
            TextureFormat::Rgba8Sint
            | TextureFormat::R8Sint
            | TextureFormat::Rg8Sint
            | TextureFormat::R32Sint
            | TextureFormat::R16Sint
            | TextureFormat::Rg16Sint
            | TextureFormat::Rgba16Sint
            | TextureFormat::Rg32Sint
            | TextureFormat::Rgba32Sint => TextureNumericClass::Sint,
            _ => TextureNumericClass::Float,
        }
    }

    /// Bytes per texel for the color formats the software backend can materialize.
    pub fn bytes_per_texel(self) -> Option<usize> {
        Some(match self {
            TextureFormat::R8Unorm
            | TextureFormat::R8Snorm
            | TextureFormat::R8Uint
            | TextureFormat::R8Sint => 1,
            TextureFormat::R16Float
            | TextureFormat::R16Uint
            | TextureFormat::R16Sint
            | TextureFormat::R5g6b5Unorm
            | TextureFormat::A1r5g5b5Unorm
            | TextureFormat::B4g4r4a4Unorm => 2,
            TextureFormat::Rg8Unorm
            | TextureFormat::Rg8Snorm
            | TextureFormat::Rg8Uint
            | TextureFormat::Rg8Sint => 2,
            TextureFormat::Rgba8Uint
            | TextureFormat::Rgba8Sint
            | TextureFormat::Rgba8Unorm
            | TextureFormat::Bgra8Unorm
            | TextureFormat::Rgba8Srgb
            | TextureFormat::Bgra8Srgb
            | TextureFormat::R32Float
            | TextureFormat::R32Uint
            | TextureFormat::R32Sint => 4,
            TextureFormat::Rgb9e5Ufloat
            | TextureFormat::Rgb10a2Unorm
            | TextureFormat::Rgb10a2Uint
            | TextureFormat::Rg11b10Ufloat => 4,
            TextureFormat::Rgba8Snorm
            | TextureFormat::Rg16Float
            | TextureFormat::Rg16Unorm
            | TextureFormat::Rg16Uint
            | TextureFormat::Rg16Sint => 4,
            TextureFormat::Rgba16Float
            | TextureFormat::Rgba16Unorm
            | TextureFormat::Rgba16Uint
            | TextureFormat::Rgba16Sint
            | TextureFormat::Rg32Float
            | TextureFormat::Rg32Uint
            | TextureFormat::Rg32Sint => 8,
            TextureFormat::Rgba32Float | TextureFormat::Rgba32Uint | TextureFormat::Rgba32Sint => {
                16
            }
            // depth/stencil are not plain-color; software backend won't clear-fill these
            TextureFormat::Depth32Float | TextureFormat::Depth24PlusStencil8 => return None,
            TextureFormat::Bc1RgbaUnorm
            | TextureFormat::Bc1RgbaSrgb
            | TextureFormat::Bc2RgbaUnorm
            | TextureFormat::Bc2RgbaSrgb
            | TextureFormat::Bc3RgbaUnorm
            | TextureFormat::Bc3RgbaSrgb
            | TextureFormat::Bc4RUnorm
            | TextureFormat::Bc4RSnorm
            | TextureFormat::Bc5RgUnorm
            | TextureFormat::Bc5RgSnorm
            | TextureFormat::Bc6hRgbUfloat
            | TextureFormat::Bc6hRgbFloat
            | TextureFormat::Bc7RgbaUnorm
            | TextureFormat::Bc7RgbaSrgb
            | TextureFormat::Etc2Rgb8Unorm
            | TextureFormat::Etc2Rgb8Srgb
            | TextureFormat::Etc2Rgb8A1Unorm
            | TextureFormat::Etc2Rgb8A1Srgb
            | TextureFormat::Etc2Rgba8Unorm
            | TextureFormat::Etc2Rgba8Srgb
            | TextureFormat::EacR11Unorm
            | TextureFormat::EacR11Snorm
            | TextureFormat::EacRg11Unorm
            | TextureFormat::EacRg11Snorm => return None,
        })
    }

    /// Whether this format's COLOUR channels are stored gamma-encoded. Alpha never is.
    pub fn is_srgb(self) -> bool {
        matches!(self, TextureFormat::Rgba8Srgb | TextureFormat::Bgra8Srgb)
    }

    /// The IEC 61966-2-1 sRGB OETF (linear-light optical → electrical), quantized to 8 bits.
    pub fn srgb_encode(value: f32) -> u8 {
        let x = value.clamp(0.0, 1.0);
        let y = if x <= 0.0031308 {
            x * 12.92
        } else {
            1.055 * x.powf(1.0 / 2.4) - 0.055
        };
        (y * 255.0 + 0.5) as u8
    }

    /// The IEC 61966-2-1 sRGB EOTF on a normalized value (electrical → linear-light optical).
    pub fn srgb_to_linear(x: f32) -> f32 {
        if x <= 0.04045 {
            x / 12.92
        } else {
            ((x + 0.055) / 1.055).powf(2.4)
        }
    }

    /// Pack a linear-light clear colour into ONE texel of this format, or `None` for a format that has no
    /// defined packing of a normalized colour.
    ///
    /// This is a property of the format — a constant table, with one right answer per format — and it
    /// lives here because it had been written out twice, once in the software oracle and once in the wgpu
    /// backend, each claiming in a comment to match the other. They did not. The wgpu copy quantized every
    /// channel linearly, so a `ClearRect` into an sRGB target stored 128 for linear 0.5 where the oracle
    /// and the hardware ROP both store 188 — sixty unorm steps apart, and invisible to the differential
    /// because its sRGB clear case takes the render-pass load op rather than `ClearRect`. Two hand-written
    /// copies of a constant table do not buy independence, they buy drift.
    ///
    /// The genuine independence in the differential is untouched: wgpu's `LoadOp::Clear` is the hardware
    /// ROP and never calls this. This serves `ClearRect` only, which wgpu has no fixed-function equivalent
    /// for and must emulate by uploading bytes — there is no second implementation there to disagree with.
    ///
    /// Rules, and why each is what it is:
    /// * A colour channel of an sRGB format is gamma-ENCODED; alpha is always a plain linear quantize.
    /// * An 8-bit unorm channel is clamped to `0..=1` then quantized round-half-up, because that is the
    ///   range its storage can represent.
    /// * A FLOAT format is stored unclamped and ungamma'd. Clamping here would defeat the reason a float
    ///   target exists, and would disagree with the load-op clear, which carries the value through.
    /// * An INTEGER format takes the colour as RAW VALUES, truncated toward zero and clamped to the
    ///   channel's range — not as a normalized colour scaled by 255. This is not a mapping invented here:
    ///   it is what the hardware clear already does. `LoadOp::Clear` hands this same `[f32; 4]` straight
    ///   to `wgpu::Color`, which WebGPU interprets as the integer clear value for a Uint/Sint target, so
    ///   packing it any other way would make the emulated rectangle clear disagree with the load-op clear
    ///   inside one driver — the exact two-routes-disagree defect that the sRGB drift was.
    ///
    /// The integer formats stay out of `COLOR_FORMATS` (see `protocol::model::capability`) even so. That
    /// set is what every backend can fully materialize, and the software oracle's SAMPLE and BLEND paths
    /// are still defined on normalized channels. Being clearable is not being renderable-in-full, and the
    /// capability set says the stronger thing.
    pub fn clear_texel(self, color: [f32; 4]) -> Option<Vec<u8>> {
        self.clear_texel_f64(color.map(f64::from))
    }

    /// Pack a clear without narrowing full-width integer channels through `f32`.
    pub fn clear_texel_f64(self, color: [f64; 4]) -> Option<Vec<u8>> {
        let unorm = |value: f64| (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let channel = |value: f64| {
            if self.is_srgb() {
                Self::srgb_encode(value as f32)
            } else {
                unorm(value)
            }
        };
        let half = |value: f64| crate::protocol::model::half::from_f32(value as f32).to_le_bytes();
        let unorm_bits =
            |value: f64, max: u16| (value.clamp(0.0, 1.0) * f64::from(max) + 0.5) as u16;
        Some(match self {
            TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Srgb => vec![
                channel(color[0]),
                channel(color[1]),
                channel(color[2]),
                unorm(color[3]),
            ],
            TextureFormat::Bgra8Unorm | TextureFormat::Bgra8Srgb => vec![
                channel(color[2]),
                channel(color[1]),
                channel(color[0]),
                unorm(color[3]),
            ],
            TextureFormat::R8Unorm => vec![unorm(color[0])],
            TextureFormat::Rg8Unorm => vec![unorm(color[0]), unorm(color[1])],
            TextureFormat::R32Float => (color[0] as f32).to_le_bytes().to_vec(),
            TextureFormat::Rgba16Unorm => color
                .iter()
                .flat_map(|v| unorm_bits(*v, u16::MAX).to_le_bytes())
                .collect(),
            TextureFormat::Rg16Unorm => color[..2]
                .iter()
                .flat_map(|v| unorm_bits(*v, u16::MAX).to_le_bytes())
                .collect(),
            TextureFormat::Rg32Float => color[..2]
                .iter()
                .flat_map(|v| (*v as f32).to_le_bytes())
                .collect(),
            TextureFormat::Rgba16Float => color.iter().flat_map(|v| half(*v)).collect(),
            TextureFormat::Rgba32Float => color
                .iter()
                .flat_map(|v| (*v as f32).to_le_bytes())
                .collect(),
            // Raw integer values, one byte a channel. `as` on a float already truncates toward zero and
            // saturates at the integer type's bounds in Rust, which is the same clamp the hardware applies.
            TextureFormat::R8Uint => vec![color[0] as u8],
            TextureFormat::Rg8Uint => vec![color[0] as u8, color[1] as u8],
            TextureFormat::Rgba8Uint => color.iter().map(|v| *v as u8).collect(),
            TextureFormat::R8Sint => vec![color[0] as i8 as u8],
            TextureFormat::Rg8Sint => vec![color[0] as i8 as u8, color[1] as i8 as u8],
            TextureFormat::Rgba8Sint => color.iter().map(|v| *v as i8 as u8).collect(),
            TextureFormat::Rgba32Uint => color
                .iter()
                .flat_map(|v| (*v as u32).to_le_bytes())
                .collect(),
            TextureFormat::Rgba32Sint => color
                .iter()
                .flat_map(|v| (*v as i32).to_le_bytes())
                .collect(),
            TextureFormat::R32Uint => (color[0] as u32).to_le_bytes().to_vec(),
            TextureFormat::R32Sint => (color[0] as i32).to_le_bytes().to_vec(),
            TextureFormat::R5g6b5Unorm => (unorm_bits(color[2], 31)
                | (unorm_bits(color[1], 63) << 5)
                | (unorm_bits(color[0], 31) << 11))
                .to_le_bytes()
                .to_vec(),
            TextureFormat::A1r5g5b5Unorm => (unorm_bits(color[2], 31)
                | (unorm_bits(color[1], 31) << 5)
                | (unorm_bits(color[0], 31) << 10)
                | (unorm_bits(color[3], 1) << 15))
                .to_le_bytes()
                .to_vec(),
            TextureFormat::B4g4r4a4Unorm => (unorm_bits(color[3], 15)
                | (unorm_bits(color[0], 15) << 4)
                | (unorm_bits(color[1], 15) << 8)
                | (unorm_bits(color[2], 15) << 12))
                .to_le_bytes()
                .to_vec(),
            _ => return None,
        })
    }

    /// Decode ONE texel of this format back to linear-light RGBA — the inverse of [`Self::clear_texel`],
    /// and the operation any code that must INTERPOLATE texels needs.
    ///
    /// Kept beside its inverse so the two cannot drift, and round-tripped against it by test. Without it,
    /// the software bilinear sampler read every plane as unsigned bytes and averaged them: blitting a
    /// half-float 2x1 down to 1x1 with a linear filter averaged the BYTES of the two encodings, so
    /// (1.0, 0, 0, 1) and (0, 0, 1.0, 1) produced 0.0059 where the answer is 0.5 — the mean of the high
    /// bytes `0x3C` and `0x00` reassembled as `0x1E00`. Its alpha came out right, because both texels
    /// happened to carry identical alpha bytes, which is exactly the kind of channel that makes a wrong
    /// path look correct.
    ///
    /// Channels the format does not carry read as zero, and an absent alpha reads as one, matching the
    /// rule a sampler applies. Returns `None` for a format with no plain-colour texel — the depth/stencil
    /// and block-compressed ones — for the same reason [`Self::clear_texel`] does.
    pub fn texel_to_f32(self, texel: &[u8]) -> Option<[f32; 4]> {
        let byte = |index: usize| texel.get(index).copied().unwrap_or(0);
        // An sRGB colour channel is stored gamma-encoded, so decoding it is where the transfer function
        // is undone; alpha is linear in both directions.
        let colour = |index: usize| {
            if self.is_srgb() {
                Self::srgb_to_linear(byte(index) as f32 / 255.0)
            } else {
                byte(index) as f32 / 255.0
            }
        };
        let unorm = |index: usize| byte(index) as f32 / 255.0;
        let single = |index: usize| {
            texel
                .get(index * 4..index * 4 + 4)
                .and_then(|b| b.try_into().ok())
                .map_or(0.0, f32::from_le_bytes)
        };
        let half = |index: usize| {
            texel
                .get(index * 2..index * 2 + 2)
                .and_then(|b| b.try_into().ok())
                .map_or(0.0, |b| super::half::to_f32(u16::from_le_bytes(b)))
        };
        let unorm16 = |index: usize| {
            texel
                .get(index * 2..index * 2 + 2)
                .and_then(|b| b.try_into().ok())
                .map_or(0.0, |b| u16::from_le_bytes(b) as f32 / u16::MAX as f32)
        };
        Some(match self {
            TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Srgb => {
                [colour(0), colour(1), colour(2), unorm(3)]
            }
            TextureFormat::Bgra8Unorm | TextureFormat::Bgra8Srgb => {
                [colour(2), colour(1), colour(0), unorm(3)]
            }
            TextureFormat::R8Unorm => [unorm(0), 0.0, 0.0, 1.0],
            TextureFormat::Rg8Unorm => [unorm(0), unorm(1), 0.0, 1.0],
            TextureFormat::R32Float => [single(0), 0.0, 0.0, 1.0],
            TextureFormat::Rgba16Unorm => [unorm16(0), unorm16(1), unorm16(2), unorm16(3)],
            TextureFormat::Rg16Unorm => [unorm16(0), unorm16(1), 0.0, 1.0],
            TextureFormat::Rg32Float => [single(0), single(1), 0.0, 1.0],
            TextureFormat::Rgba16Float => [half(0), half(1), half(2), half(3)],
            TextureFormat::Rgba32Float => [single(0), single(1), single(2), single(3)],
            // Raw integer values, matching the direction `clear_texel` packs them.
            TextureFormat::R8Uint => [byte(0) as f32, 0.0, 0.0, 1.0],
            TextureFormat::Rg8Uint => [byte(0) as f32, byte(1) as f32, 0.0, 1.0],
            TextureFormat::Rgba8Uint => [
                byte(0) as f32,
                byte(1) as f32,
                byte(2) as f32,
                byte(3) as f32,
            ],
            TextureFormat::R8Sint => [byte(0) as i8 as f32, 0.0, 0.0, 1.0],
            TextureFormat::Rg8Sint => [byte(0) as i8 as f32, byte(1) as i8 as f32, 0.0, 1.0],
            TextureFormat::Rgba8Sint => [
                byte(0) as i8 as f32,
                byte(1) as i8 as f32,
                byte(2) as i8 as f32,
                byte(3) as i8 as f32,
            ],
            _ => return None,
        })
    }

    /// Bytes per texel to charge for *residency accounting*, for every format including the ones
    /// [`Self::bytes_per_texel`] cannot describe.
    ///
    /// `bytes_per_texel` answers `None` for depth/stencil and for the block-compressed formats, because
    /// it exists to tell the software backend how to clear-fill a plain colour texel. Accounting has a
    /// different question — how much the executor keeps resident — and every format has an answer to
    /// that one, so this never returns an Option that a caller has to invent a value for. Both the
    /// texture and the surface charge paths call it, so a format cannot be charged two different ways
    /// depending on which one created it, which is exactly what happened before it existed.
    ///
    /// Known conservative inaccuracy, unchanged from the previous behaviour and safe in direction: the
    /// block-compressed formats are charged as 4-byte texels although BC1/BC4 occupy half a byte per
    /// texel and the rest one byte. That overcharges, so a limit trips early rather than late. Fixing it
    /// needs block-geometry arithmetic in the footprint walk rather than a per-texel constant, and this
    /// is now the single place to do it.
    pub fn footprint_bytes_per_texel(self) -> usize {
        match self {
            // Materialized by the CPU executor as an 8-byte depth+stencil plane
            // (`cpu::executor::resource`); charging the 4-byte default undercounted it by 2x.
            TextureFormat::Depth24PlusStencil8 => 8,
            format => format.bytes_per_texel().unwrap_or(4),
        }
    }

    pub fn block_geometry(self) -> Option<(u32, u32, u32)> {
        Some(match self {
            TextureFormat::Bc1RgbaUnorm
            | TextureFormat::Bc1RgbaSrgb
            | TextureFormat::Bc4RUnorm
            | TextureFormat::Bc4RSnorm => (4, 4, 8),
            TextureFormat::Etc2Rgb8Unorm
            | TextureFormat::Etc2Rgb8Srgb
            | TextureFormat::Etc2Rgb8A1Unorm
            | TextureFormat::Etc2Rgb8A1Srgb
            | TextureFormat::EacR11Unorm
            | TextureFormat::EacR11Snorm => (4, 4, 8),
            TextureFormat::Bc2RgbaUnorm
            | TextureFormat::Bc2RgbaSrgb
            | TextureFormat::Bc3RgbaUnorm
            | TextureFormat::Bc3RgbaSrgb
            | TextureFormat::Bc5RgUnorm
            | TextureFormat::Bc5RgSnorm
            | TextureFormat::Bc6hRgbUfloat
            | TextureFormat::Bc6hRgbFloat
            | TextureFormat::Bc7RgbaUnorm
            | TextureFormat::Bc7RgbaSrgb => (4, 4, 16),
            TextureFormat::Etc2Rgba8Unorm
            | TextureFormat::Etc2Rgba8Srgb
            | TextureFormat::EacRg11Unorm
            | TextureFormat::EacRg11Snorm => (4, 4, 16),
            _ => return None,
        })
    }

    pub fn copy_layout(self, width: u32, height: u32) -> Option<(u32, u32)> {
        if let Some((bw, bh, bytes)) = self.block_geometry() {
            Some((width.div_ceil(bw) * bytes, height.div_ceil(bh)))
        } else {
            Some((width.checked_mul(self.bytes_per_texel()? as u32)?, height))
        }
    }
}

u32_enum!(
    /// Texture dimensionality.
    TextureDim { D1 = 1, D2 = 2, D3 = 3, Cube = 4 } "TextureDim"
);

u32_enum!(
    /// Index element size.
    IndexFormat { U16 = 1, U32 = 2 } "IndexFormat"
);

u32_enum!(
    /// Primitive assembly topology (WebGPU numbering).
    Topology { PointList = 0, LineList = 1, LineStrip = 2, TriangleList = 3, TriangleStrip = 4 } "Topology"
);

u32_enum!(
    /// Render-pass attachment load behavior.
    ///
    /// CONTRACT NOTE on `DontCare`: no executor in this workspace honours it as "the prior contents are
    /// undefined". Both the CPU oracle and the wgpu executor lower it as `Load`. That is the only SAFE
    /// lowering — wgpu/WebGPU has no `DontCare`, and lowering it to `Clear` would invent pixels a caller
    /// never asked for — but it means a guest cannot use `DontCare` to skip a load. Treat it as a HINT the
    /// contract currently discards: a producer must not rely on the attachment being undefined afterwards,
    /// and must not rely on saving the load bandwidth either.
    LoadOp { Load = 0, Clear = 1, DontCare = 2 } "LoadOp"
);

u32_enum!(
    /// Texture filtering mode.
    Filter { Nearest = 0, Linear = 1 } "Filter"
);

u32_enum!(
    /// Which plane of a texture a copy/blit or subresource selector addresses (color/depth/stencil).
    /// The software oracle only materializes color (`All`).
    TextureAspect { All = 0, DepthOnly = 1, StencilOnly = 2 } "TextureAspect"
);

u32_enum!(
    /// Sampler address (wrap) mode.
    AddressMode { ClampToEdge = 0, Repeat = 1, MirrorRepeat = 2 } "AddressMode"
);

/// Depth/stencil compare functions. `DepthState::depth_compare` and `StencilFaceState::compare` carry one
/// of THESE codes on the wire — the protocol's own numbering, which follows Vulkan `VkCompareOp` ordering
/// and is NOT WebGPU's 1-based `GPUCompareFunction`. `passes(compare, frag, stored)` is the per-fragment
/// test the depth rasterizer runs.
pub mod compare {
    pub const NEVER: u32 = 0;
    pub const LESS: u32 = 1;
    pub const EQUAL: u32 = 2;
    pub const LESS_EQUAL: u32 = 3;
    pub const GREATER: u32 = 4;
    pub const NOT_EQUAL: u32 = 5;
    pub const GREATER_EQUAL: u32 = 6;
    pub const ALWAYS: u32 = 7;

    /// Evaluate `frag <compare> stored`.
    ///
    /// The trailing arm is DEFENSIVE, not a policy. It used to be documented as deliberate leniency — "an
    /// unrecognized value is treated as `ALWAYS` so an honest bring-up never hard-fails" — and that was a
    /// claim about a path nothing prevented from being taken: `depth_compare` and the stencil face
    /// compares were validated NOWHERE, so a guest code above `ALWAYS` silently disabled the depth test on
    /// this executor and on the wgpu one alike, and the draw reported success. `validate` now rejects any
    /// code above `ALWAYS` on `CreateRenderPipeline`, exactly as it always did for a sampler's compare, so
    /// no such value reaches here from the wire. The arm stays because this is a `u32` in a `pub fn` an
    /// in-process caller can reach directly.
    pub fn passes(compare: u32, frag: f32, stored: f32) -> bool {
        match compare {
            NEVER => false,
            LESS => frag < stored,
            EQUAL => frag == stored,
            LESS_EQUAL => frag <= stored,
            GREATER => frag > stored,
            NOT_EQUAL => frag != stored,
            GREATER_EQUAL => frag >= stored,
            _ => true,
        }
    }
}

/// Stencil operations. A [`super::descriptor::StencilFaceState`]'s `fail_op` / `depth_fail_op` / `pass_op`
/// carry one of these codes on the wire — this protocol's own numbering, mirroring how [`compare`] numbers
/// the compare functions (Vulkan `VkStencilOp` ordering), not a WebGPU enum. The executor maps each to the
/// matching `wgpu::StencilOperation`. A code above `DECREMENT_WRAP` is rejected by `validate` on
/// `CreateRenderPipeline`; the `KEEP` fallback in [`stencil_op::apply`] is defensive rather than a
/// deliberate leniency (see [`compare::passes`], which carried the same false claim).
pub mod stencil_op {
    pub const KEEP: u32 = 0;
    pub const ZERO: u32 = 1;
    pub const REPLACE: u32 = 2;
    pub const INCREMENT_CLAMP: u32 = 3;
    pub const DECREMENT_CLAMP: u32 = 4;
    pub const INVERT: u32 = 5;
    pub const INCREMENT_WRAP: u32 = 6;
    pub const DECREMENT_WRAP: u32 = 7;

    /// Compute the new stencil value an op produces from the currently-`stored` value and the pass
    /// `reference`, for the oracle's 8-bit (`Depth24PlusStencil8`) stencil plane — matching
    /// `wgpu::StencilOperation` byte-for-byte. `*_CLAMP` clamps to the `[0, 255]` representable range of the
    /// 8-bit buffer; `*_WRAP` wraps modulo 256; `INVERT` flips all eight bits. An unmodeled code falls back
    /// to `KEEP`, which is DEFENSIVE — `validate` rejects such a code on `CreateRenderPipeline`. The write mask is
    /// applied by the caller (only bits set in `stencilWriteMask` are updated), so this returns the raw
    /// pre-mask candidate value. `validate` rejects an out-of-range op before it reaches here.
    pub fn apply(op: u32, stored: u8, reference: u8) -> u8 {
        match op {
            ZERO => 0,
            REPLACE => reference,
            INCREMENT_CLAMP => stored.saturating_add(1),
            DECREMENT_CLAMP => stored.saturating_sub(1),
            INVERT => !stored,
            INCREMENT_WRAP => stored.wrapping_add(1),
            DECREMENT_WRAP => stored.wrapping_sub(1),
            // KEEP and any unmodeled code leave the stored value untouched.
            _ => stored,
        }
    }
}

/// Fixed-function blend factors carried by
/// [`super::descriptor::BlendState`]. The numbering is the neutral
/// GL-to-host protocol vocabulary, not a backend enum. Keep this list in sync
/// with every guest API adapter that lowers blend state.
pub mod blend_factor {
    pub const ZERO: u32 = 0;
    pub const ONE: u32 = 1;
    pub const SRC_COLOR: u32 = 2;
    pub const ONE_MINUS_SRC_COLOR: u32 = 3;
    pub const SRC_ALPHA: u32 = 4;
    pub const ONE_MINUS_SRC_ALPHA: u32 = 5;
    pub const DST_COLOR: u32 = 6;
    pub const ONE_MINUS_DST_COLOR: u32 = 7;
    pub const DST_ALPHA: u32 = 8;
    pub const ONE_MINUS_DST_ALPHA: u32 = 9;
    pub const SRC_ALPHA_SATURATE: u32 = 10;
    pub const CONSTANT: u32 = 11;
    pub const ONE_MINUS_CONSTANT: u32 = 12;
    pub const SRC1_COLOR: u32 = 13;
    pub const ONE_MINUS_SRC1_COLOR: u32 = 14;
    pub const SRC1_ALPHA: u32 = 15;
    pub const ONE_MINUS_SRC1_ALPHA: u32 = 16;
}

/// Blend equations carried by [`super::descriptor::BlendState`]'s `op_color` / `op_alpha`. Same neutral
/// GL-to-host vocabulary as [`blend_factor`], not a backend enum; the executor defaults an unmodeled code
/// to `ADD`. These constants existed only as prose in the guest and executor until now, which is how a
/// driver author ends up guessing a foreign enum instead.
pub mod blend_op {
    pub const ADD: u32 = 0;
    pub const SUBTRACT: u32 = 1;
    pub const REVERSE_SUBTRACT: u32 = 2;
    pub const MIN: u32 = 3;
    pub const MAX: u32 = 4;
}

// ---------------------------------------------------------------------------------------------------
// usage bitflags (hand-rolled u32 constants — no `bitflags` dep)
// ---------------------------------------------------------------------------------------------------

/// Buffer usage flags. A device allocation maps to `STORAGE | COPY_SRC | COPY_DST`.
pub mod buffer_usage {
    pub const VERTEX: u32 = 1 << 0;
    pub const INDEX: u32 = 1 << 1;
    pub const UNIFORM: u32 = 1 << 2;
    pub const STORAGE: u32 = 1 << 3;
    pub const COPY_SRC: u32 = 1 << 4;
    pub const COPY_DST: u32 = 1 << 5;
    pub const INDIRECT: u32 = 1 << 6;
    /// Host-visible / mappable (unified memory on integrated GPUs; pinned/managed on discrete).
    pub const MAP: u32 = 1 << 7;
}

/// Texture usage flags.
pub mod texture_usage {
    pub const SAMPLED: u32 = 1 << 0;
    pub const STORAGE: u32 = 1 << 1;
    pub const RENDER_TARGET: u32 = 1 << 2;
    pub const COPY_SRC: u32 = 1 << 3;
    pub const COPY_DST: u32 = 1 << 4;
    /// Presentable to an HLP surface.
    pub const PRESENT: u32 = 1 << 5;
    /// BC1 blocks use Vulkan's opaque RGB interpretation. Internal protocol semantic bit; it does not
    /// grant an operation and must be combined with the ordinary usage flags above.
    pub const OPAQUE_BC1_RGB: u32 = 1 << 31;
}

#[cfg(test)]
mod clear_texel_tests {
    use super::TextureFormat;
    use crate::protocol::model::capability::{COLOR_FORMATS, INTEGER_FORMATS};

    /// Colours chosen to separate wrong arithmetic from right arithmetic. Mid-range values survive almost
    /// any mistake; the endpoints and the out-of-range values do not.
    const COLORS: &[[f32; 4]] = &[
        [0.0, 0.0, 0.0, 0.0],
        [1.0, 1.0, 1.0, 1.0],
        [0.5, 0.25, 0.75, 0.125],
        [4.0, -2.5, 65504.0, 1.0e-9],
        [-1.0, 2.0, 0.0, 1.0],
    ];

    /// Every format the capability set claims EVERY backend can materialize must have a clear packing.
    ///
    /// This is the assertion that would have caught the blocker: `COLOR_FORMATS` has listed the three
    /// float formats all along while both `clear_texel` copies refused them, so a float colour buffer was
    /// promised and could not be cleared. A capability claimed and not honoured is worse than one never
    /// made, and this pins the two against each other so they cannot drift apart again.
    #[test]
    fn every_promised_colour_format_can_be_cleared() {
        for &format in COLOR_FORMATS {
            for &color in COLORS {
                assert!(
                    format.clear_texel(color).is_some(),
                    "{format:?} is in COLOR_FORMATS and must have a clear packing for {color:?}"
                );
            }
        }
    }

    /// The packed texel is exactly as wide as the format says it is. A clear that packs the wrong width
    /// fills a plane at the wrong stride, which is as silent as a readback at the wrong stride.
    #[test]
    fn a_clear_texel_is_exactly_one_texel_wide() {
        for &format in COLOR_FORMATS {
            let texel = format.clear_texel([0.5, 0.5, 0.5, 1.0]).expect("promised");
            assert_eq!(
                Some(texel.len()),
                format.bytes_per_texel(),
                "{format:?} packs a texel of its own declared width"
            );
        }
    }

    /// The values themselves, named. One shared implementation makes the backends agree; it does not make
    /// them right, so the rule's own answers are pinned separately from the agreement.
    #[test]
    fn the_packing_rule_answers_what_each_format_requires() {
        // An sRGB colour channel is gamma-encoded and alpha is not: linear 0.5 stores 188, not 128. This
        // is the value the wgpu copy got wrong, and `hl-gpu-wgpu/tests/srgb_target.rs` independently
        // proves 188 is what the hardware ROP writes.
        assert_eq!(
            TextureFormat::Rgba8Srgb.clear_texel([0.5, 0.5, 0.5, 0.5]),
            Some(vec![188, 188, 188, 128]),
            "an sRGB target gamma-encodes colour and leaves alpha linear"
        );
        assert_eq!(
            TextureFormat::Rgba8Unorm.clear_texel([0.5, 0.5, 0.5, 0.5]),
            Some(vec![128, 128, 128, 128]),
            "a linear target quantizes every channel the same way"
        );
        assert_eq!(
            TextureFormat::Bgra8Srgb.clear_texel([1.0, 0.5, 0.0, 1.0]),
            Some(vec![0, 188, 255, 255]),
            "a BGRA target swaps blue and red, and still gamma-encodes"
        );
        assert_eq!(
            TextureFormat::R8Unorm.clear_texel([1.0, 0.5, 0.5, 0.5]),
            Some(vec![255]),
            "a one-channel target packs one byte"
        );
        assert_eq!(
            TextureFormat::Rgba8Unorm.clear_texel([4.0, -2.5, 0.0, 1.0]),
            Some(vec![255, 0, 0, 255]),
            "out-of-range clamps into a unorm, whose storage cannot hold anything else"
        );

        // A FLOAT target does NOT clamp. Clamping would defeat the reason it exists, and would disagree
        // with the render-pass load op, which carries the value through to the hardware unchanged.
        let half = |v: f32| crate::protocol::model::half::from_f32(v).to_le_bytes();
        let mut expected = Vec::new();
        for value in [4.0f32, -2.5, 65504.0, 1.0] {
            expected.extend_from_slice(&half(value));
        }
        assert_eq!(
            TextureFormat::Rgba16Float.clear_texel([4.0, -2.5, 65504.0, 1.0]),
            Some(expected),
            "a half-float target stores values outside 0..=1 rather than clamping them"
        );
        assert_eq!(
            TextureFormat::Rgba32Float.clear_texel([4.0, -2.5, 0.0, 1.0]),
            Some(
                [4.0f32, -2.5, 0.0, 1.0]
                    .iter()
                    .flat_map(|v| v.to_le_bytes())
                    .collect::<Vec<u8>>()
            ),
            "a float target stores the value it was given"
        );
        assert_eq!(
            TextureFormat::R32Float.clear_texel([4.0, 9.0, 9.0, 9.0]),
            Some(4.0f32.to_le_bytes().to_vec()),
            "a single-channel float target is one float, and ignores the channels it lacks"
        );
        assert!(
            !TextureFormat::Rgba16Float.is_srgb() && !TextureFormat::Rgba32Float.is_srgb(),
            "no float format is sRGB, so a clear into one is never gamma-encoded"
        );
    }

    /// `texel_to_f32` is the inverse of `clear_texel` for every format that has both.
    ///
    /// Round-tripped rather than spot-checked, because the two are a pair and the failure mode is drift
    /// between them — the same failure that put a byte-averaging sampler in the blit path, where nothing
    /// related an encoding to its decoding at all. sRGB is in the loop deliberately: it is the only format
    /// family where the two directions are not each other's obvious mirror, so it is where a mismatched
    /// transfer function would hide.
    #[test]
    fn decoding_a_texel_inverts_packing_it() {
        for &format in COLOR_FORMATS {
            for &color in COLORS {
                let packed = format.clear_texel(color).expect("promised");
                let decoded = format
                    .texel_to_f32(&packed)
                    .expect("its own inverse exists");
                let repacked = format.clear_texel(decoded).expect("promised");
                assert_eq!(
                    repacked, packed,
                    "{format:?} must decode {color:?} back to something that re-packs identically"
                );
            }
        }
    }

    /// Channels a format does not carry decode as zero, and an absent alpha as one — the rule a sampler
    /// applies, and the reason a one-channel plane blitted into does not acquire a green channel.
    #[test]
    fn a_decoded_texel_reports_only_the_channels_the_format_has() {
        assert_eq!(
            TextureFormat::R8Unorm.texel_to_f32(&[255]),
            Some([1.0, 0.0, 0.0, 1.0])
        );
        assert_eq!(
            TextureFormat::Rg8Unorm.texel_to_f32(&[255, 128]),
            Some([1.0, 128.0 / 255.0, 0.0, 1.0])
        );
        // sRGB decodes the colour channels and leaves alpha linear: 188 is the stored form of linear 0.5.
        let srgb = TextureFormat::Rgba8Srgb
            .texel_to_f32(&[188, 188, 188, 128])
            .expect("an sRGB texel decodes");
        assert!(
            (srgb[0] - 0.5).abs() < 0.005,
            "an sRGB colour channel decodes through the transfer function, got {}",
            srgb[0]
        );
        assert!(
            (srgb[3] - 128.0 / 255.0).abs() < 1e-6,
            "and alpha does not, got {}",
            srgb[3]
        );
        // A format with no plain-colour texel has no decoding, exactly as it has no packing.
        assert!(TextureFormat::Depth32Float.texel_to_f32(&[0; 4]).is_none());
        assert!(TextureFormat::Bc1RgbaUnorm.texel_to_f32(&[0; 8]).is_none());
    }

    /// An integer target is cleared from RAW VALUES, and is still not promised as a fully materializable
    /// format.
    ///
    /// This asserted the opposite an hour ago — that an integer format has no clear packing at all — and
    /// that was right about the code and wrong about the driver it sat in. The hardware load-op clear
    /// hands the same `[f32; 4]` to `wgpu::Color`, which a Uint/Sint target reads as the integer, so
    /// refusing here left one driver's two clear routes disagreeing: a clear folded into a render pass
    /// stored the value, and a scissored or clear-only frame refused. That is the same shape as the sRGB
    /// drift, and the same shape as the mismatch this fixes — `colour_renderable` in the GL layer has
    /// always reported an integer colour attachment COMPLETE, so a guest could reach a framebuffer whose
    /// clear failed depending on whether anything was drawn after it.
    ///
    /// The capability set is deliberately NOT widened: being clearable is not being materializable in
    /// full, and the oracle's blend and sample paths still have no normalized reading for these.
    #[test]
    fn an_integer_target_clears_from_raw_values_and_is_still_not_promised() {
        // The value is the number, not a normalized colour: 200 stores 200, not 255.
        assert_eq!(
            TextureFormat::Rgba8Uint.clear_texel([200.0, 1.0, 0.0, 255.0]),
            Some(vec![200, 1, 0, 255]),
            "a Uint target stores the integers it was given"
        );
        assert_eq!(
            TextureFormat::R8Uint.clear_texel([200.0, 0.0, 0.0, 0.0]),
            Some(vec![200]),
            "and only the channels it has"
        );
        assert_eq!(
            TextureFormat::Rg8Sint.clear_texel([-120.0, 7.0, 0.0, 0.0]),
            Some(vec![0x88, 7]),
            "a Sint target keeps its two's-complement byte"
        );
        // Saturating at the channel's bounds, and truncating toward zero, is what a cast to the integer
        // type does and what the hardware clear does.
        assert_eq!(
            TextureFormat::Rgba8Uint.clear_texel([300.0, -5.0, 3.9, -0.9]),
            Some(vec![255, 0, 3, 0]),
            "out of range saturates and a fraction truncates toward zero"
        );
        // The control that separates raw from normalized: 1.0 is ONE in an integer target and 255 in a
        // unorm one. A packing that scaled by 255 would pass every assertion above that used 0 or 255.
        assert_eq!(
            TextureFormat::Rgba8Uint.clear_texel([1.0, 1.0, 1.0, 1.0]),
            Some(vec![1, 1, 1, 1]),
            "a Uint target reads 1.0 as the number one"
        );
        assert_eq!(
            TextureFormat::Rgba8Unorm.clear_texel([1.0, 1.0, 1.0, 1.0]),
            Some(vec![255, 255, 255, 255]),
            "a unorm target reads the same 1.0 as full scale"
        );

        for &format in INTEGER_FORMATS {
            assert!(
                format.clear_texel([1.0, 0.0, 0.0, 1.0]).is_some(),
                "{format:?} is colour-renderable in the GL layer and must be clearable"
            );
            assert_eq!(
                format.clear_texel([1.0, 0.0, 0.0, 1.0]).map(|t| t.len()),
                format.bytes_per_texel(),
                "{format:?} packs a texel of its own width"
            );
            assert!(
                !COLOR_FORMATS.contains(&format),
                "{format:?} is still not a format every backend materializes in full"
            );
        }

        // The refusal that remains, so the positive cases above are not mistaken for "everything packs":
        // a format with no plain-colour texel at all still answers None.
        assert!(TextureFormat::Depth32Float.clear_texel([0.0; 4]).is_none());
        assert!(TextureFormat::Bc1RgbaUnorm.clear_texel([0.0; 4]).is_none());
    }
}
