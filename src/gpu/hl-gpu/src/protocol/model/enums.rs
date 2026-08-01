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
    } "TextureFormat"
);

impl TextureFormat {
    /// Bytes per texel for the color formats the software backend can materialize.
    pub fn bytes_per_texel(self) -> Option<usize> {
        Some(match self {
            TextureFormat::R8Unorm | TextureFormat::R8Uint | TextureFormat::R8Sint => 1,
            TextureFormat::Rg8Unorm | TextureFormat::Rg8Uint | TextureFormat::Rg8Sint => 2,
            TextureFormat::Rgba8Uint
            | TextureFormat::Rgba8Sint
            | TextureFormat::Rgba8Unorm
            | TextureFormat::Bgra8Unorm
            | TextureFormat::Rgba8Srgb
            | TextureFormat::Bgra8Srgb
            | TextureFormat::R32Float => 4,
            TextureFormat::Rgba16Float => 8,
            TextureFormat::Rgba32Float => 16,
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
            | TextureFormat::Bc7RgbaSrgb => return None,
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
    /// * An INTEGER format answers `None` on purpose, not by omission. Its texels are raw numbers with no
    ///   normalized reading, so a `[f32; 4]` has no defined mapping onto them; inventing one would write
    ///   plausible wrong pixels. See `INTEGER_FORMATS` in `protocol::model::capability`, which keeps them
    ///   out of the set every backend claims to materialize for exactly this reason.
    pub fn clear_texel(self, color: [f32; 4]) -> Option<Vec<u8>> {
        let unorm = |value: f32| (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        let channel = |value: f32| {
            if self.is_srgb() {
                Self::srgb_encode(value)
            } else {
                unorm(value)
            }
        };
        let half = |value: f32| crate::protocol::model::half::from_f32(value).to_le_bytes();
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
            TextureFormat::R32Float => color[0].to_le_bytes().to_vec(),
            TextureFormat::Rgba16Float => color.iter().flat_map(|v| half(*v)).collect(),
            TextureFormat::Rgba32Float => {
                color.iter().flat_map(|v| v.to_le_bytes()).collect()
            }
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

    /// The integer formats answer "no packing" rather than inventing one, and that is a deliberate
    /// refusal rather than an omission: their texels are raw numbers with no normalized reading, so a
    /// `[f32; 4]` has no defined mapping onto them. The refusal is asserted together with the capability
    /// set that justifies it, and the coverage test above is the positive control proving the path serves
    /// everything that IS promised — without it, a rule that refused everything would pass this.
    #[test]
    fn the_integer_formats_are_refused_and_promised_by_neither() {
        for &format in INTEGER_FORMATS {
            assert!(
                format.clear_texel([1.0, 0.0, 0.0, 1.0]).is_none(),
                "{format:?} has no normalized clear packing and must not invent one"
            );
            assert!(
                !COLOR_FORMATS.contains(&format),
                "{format:?} must not be promised as a format every backend materializes"
            );
        }
    }
}
