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
