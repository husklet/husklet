//! IR → wgpu enum/format maps. Pure, side-effect-free lookups the rest of the crate shares.
//!
//! The protocol's [`TextureFormat`] is the neutral wire enum; wgpu has its own. This module is the single
//! place that bridges the two (and the texel-byte footprint the tight-packing readback path needs), so a
//! format added to the protocol is wired in exactly once.

use hl_gpu::protocol::model::enums::TextureFormat;
use hl_gpu::{GpuError, Result};

/// A neutral texture format viewed through this backend's representation rules.
#[derive(Clone, Copy, Debug)]
pub struct Format(TextureFormat);

impl From<TextureFormat> for Format {
    fn from(format: TextureFormat) -> Self {
        Self(format)
    }
}

impl Format {
    /// Return the native allocation format.
    pub fn native(self) -> wgpu::TextureFormat {
        use wgpu::TextureFormat as W;
        match self.0 {
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
            TextureFormat::Bc1RgbaUnorm => W::Bc1RgbaUnorm,
            TextureFormat::Bc1RgbaSrgb => W::Bc1RgbaUnormSrgb,
            TextureFormat::Bc2RgbaUnorm => W::Bc2RgbaUnorm,
            TextureFormat::Bc2RgbaSrgb => W::Bc2RgbaUnormSrgb,
            TextureFormat::Bc3RgbaUnorm => W::Bc3RgbaUnorm,
            TextureFormat::Bc3RgbaSrgb => W::Bc3RgbaUnormSrgb,
            TextureFormat::Bc4RUnorm => W::Bc4RUnorm,
            TextureFormat::Bc4RSnorm => W::Bc4RSnorm,
            TextureFormat::Bc5RgUnorm => W::Bc5RgUnorm,
            TextureFormat::Bc5RgSnorm => W::Bc5RgSnorm,
            TextureFormat::Bc6hRgbUfloat => W::Bc6hRgbUfloat,
            TextureFormat::Bc6hRgbFloat => W::Bc6hRgbFloat,
            TextureFormat::Bc7RgbaUnorm => W::Bc7RgbaUnorm,
            TextureFormat::Bc7RgbaSrgb => W::Bc7RgbaUnormSrgb,
            // Integer color: the storage a `usampler2D`/`isampler2D` reads. wgpu's own integer formats are
            // exact one-to-one counterparts — unfilterable and unblendable on both sides, with raw integer
            // texels — so nothing is normalized or reinterpreted across this map.
            TextureFormat::Rgba8Uint => W::Rgba8Uint,
            TextureFormat::Rgba8Sint => W::Rgba8Sint,
            TextureFormat::R8Uint => W::R8Uint,
            TextureFormat::R8Sint => W::R8Sint,
            TextureFormat::Rg8Uint => W::Rg8Uint,
            TextureFormat::Rg8Sint => W::Rg8Sint,
        }
    }

    /// Bytes per texel of a color format — the tight-packed row stride the readback path repacks into, and
    /// the byte footprint `ClearRect`/buffer-copies compute. Depth/stencil formats have no plain-color texel
    /// layout the software readback path can pack, so they are rejected here (matching the CPU oracle, which
    /// materializes color only for copy/readback).
    pub fn texel_bytes(self) -> Result<usize> {
        self.0.bytes_per_texel().ok_or(GpuError::Unsupported(
            "wgpu: no packed texel layout for this format",
        ))
    }

    pub fn copy_layout(self, width: u32, height: u32) -> Result<(u32, u32)> {
        self.0
            .copy_layout(width, height)
            .ok_or(GpuError::Unsupported("wgpu: texture copy layout"))
    }

    /// Pack a normalized clear color to a format's texel bytes (round-half-up), matching the CPU oracle's
    /// `clear_texel` byte-for-byte — the `ClearRect` fill path uploads these bytes directly (the sub-rectangle
    /// clear wgpu has no fixed-function equivalent for).
    pub fn clear_texel(self, color: [f32; 4]) -> Result<Vec<u8>> {
        let to_u8 = |value: f32| (value.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        Ok(match self.0 {
            TextureFormat::Rgba8Unorm | TextureFormat::Rgba8Srgb => {
                vec![
                    to_u8(color[0]),
                    to_u8(color[1]),
                    to_u8(color[2]),
                    to_u8(color[3]),
                ]
            }
            TextureFormat::Bgra8Unorm | TextureFormat::Bgra8Srgb => {
                vec![
                    to_u8(color[2]),
                    to_u8(color[1]),
                    to_u8(color[0]),
                    to_u8(color[3]),
                ]
            }
            TextureFormat::R8Unorm => vec![to_u8(color[0])],
            TextureFormat::Rg8Unorm => vec![to_u8(color[0]), to_u8(color[1])],
            _ => return Err(GpuError::Unsupported("wgpu: ClearRect for this format")),
        })
    }
}
