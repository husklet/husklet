//! IR → wgpu enum/format maps. Pure, side-effect-free lookups the rest of the crate shares.
//!
//! The protocol's [`TextureFormat`] is the neutral wire enum; wgpu has its own. This module is the single
//! place that bridges the two (and the texel-byte footprint the tight-packing readback path needs), so a
//! format added to the protocol is wired in exactly once.

use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::{GpuError, Result};

/// Map a protocol [`TextureFormat`] to the wgpu format wgpu allocates the texture as. Depth formats map
/// to their wgpu depth equivalents; unsupported formats surface a typed error rather than a silent alias.
pub fn texture_format(f: TextureFormat) -> Result<wgpu::TextureFormat> {
    use wgpu::TextureFormat as W;
    Ok(match f {
        TextureFormat::Rgba8Unorm => W::Rgba8Unorm,
        TextureFormat::Bgra8Unorm => W::Bgra8Unorm,
        TextureFormat::Rgba8Srgb => W::Rgba8UnormSrgb,
        TextureFormat::Bgra8Srgb => W::Bgra8UnormSrgb,
        TextureFormat::R8Unorm => W::R8Unorm,
        TextureFormat::Rg8Unorm => W::Rg8Unorm,
        TextureFormat::Rgba16Float => W::Rgba16Float,
        TextureFormat::Rgba32Float => W::Rgba32Float,
        TextureFormat::R32Float => W::R32Float,
        TextureFormat::Depth32Float => W::Depth32Float,
        TextureFormat::Depth24PlusStencil8 => W::Depth24PlusStencil8,
    })
}

/// Bytes per texel of a color format — the tight-packed row stride the readback path repacks into, and
/// the byte footprint `ClearRect`/buffer-copies compute. Depth/stencil formats have no plain-color texel
/// layout the software readback path can pack, so they are rejected here (matching the CPU oracle, which
/// materializes color only for copy/readback).
pub fn texel_bytes(f: TextureFormat) -> Result<usize> {
    f.bytes_per_texel().ok_or(GpuError::Unsupported(
        "wgpu: no packed texel layout for this format",
    ))
}

/// Pack a normalized clear color to a format's texel bytes (round-half-up), matching the CPU oracle's
/// `clear_texel` byte-for-byte — the `ClearRect` fill path uploads these bytes directly (the sub-rectangle
/// clear wgpu has no fixed-function equivalent for).
pub fn clear_texel(fmt: TextureFormat, c: [f32; 4]) -> Result<Vec<u8>> {
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
        _ => return Err(GpuError::Unsupported("wgpu: ClearRect for this format")),
    })
}
