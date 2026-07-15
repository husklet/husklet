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
    } "TextureFormat"
);

impl TextureFormat {
    /// Bytes per texel for the color formats the software backend can materialize.
    pub fn bytes_per_texel(self) -> Option<usize> {
        Some(match self {
            TextureFormat::R8Unorm => 1,
            TextureFormat::Rg8Unorm => 2,
            TextureFormat::Rgba8Unorm
            | TextureFormat::Bgra8Unorm
            | TextureFormat::Rgba8Srgb
            | TextureFormat::Bgra8Srgb
            | TextureFormat::R32Float => 4,
            TextureFormat::Rgba16Float => 8,
            TextureFormat::Rgba32Float => 16,
            // depth/stencil are not plain-color; software backend won't clear-fill these
            TextureFormat::Depth32Float | TextureFormat::Depth24PlusStencil8 => return None,
        })
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

/// Depth/stencil compare functions. `DepthState::depth_compare` carries an opaque WebGPU compare-function
/// value on the wire; the software oracle interprets it with these stable constants (Vulkan `VkCompareOp`
/// ordering). `passes(compare, frag, stored)` is the per-fragment test the depth rasterizer runs.
pub mod compare {
    pub const NEVER: u32 = 0;
    pub const LESS: u32 = 1;
    pub const EQUAL: u32 = 2;
    pub const LESS_EQUAL: u32 = 3;
    pub const GREATER: u32 = 4;
    pub const NOT_EQUAL: u32 = 5;
    pub const GREATER_EQUAL: u32 = 6;
    pub const ALWAYS: u32 = 7;

    /// Evaluate `frag <compare> stored`. An unrecognized opaque value is treated as `ALWAYS` so an
    /// honest bring-up never hard-fails a draw on a compare code it does not model.
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
/// carry one of these opaque values on the wire, mirroring how [`compare`] numbers the compare functions
/// (Vulkan `VkStencilOp` ordering). The executor maps each to the matching `wgpu::StencilOperation`; an
/// unmodeled value falls back to `KEEP` so an honest bring-up never hard-fails a draw on a code it does not
/// model (the stencil analogue of `compare::passes`'s `ALWAYS` fallback).
pub mod stencil_op {
    pub const KEEP: u32 = 0;
    pub const ZERO: u32 = 1;
    pub const REPLACE: u32 = 2;
    pub const INCREMENT_CLAMP: u32 = 3;
    pub const DECREMENT_CLAMP: u32 = 4;
    pub const INVERT: u32 = 5;
    pub const INCREMENT_WRAP: u32 = 6;
    pub const DECREMENT_WRAP: u32 = 7;
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
