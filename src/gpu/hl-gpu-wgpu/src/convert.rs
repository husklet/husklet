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
            TextureFormat::R32Uint => W::R32Uint,
            TextureFormat::R32Sint => W::R32Sint,
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
            TextureFormat::Rgba32Uint => W::Rgba32Uint,
            TextureFormat::Rgba32Sint => W::Rgba32Sint,
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

    /// Pack a normalized clear colour into one texel, or a typed refusal naming THIS backend.
    ///
    /// The rule is the format's own ([`TextureFormat::clear_texel`]) rather than a second copy that claims
    /// in a comment to match the oracle's. The copy this replaces did not match it: every channel was
    /// quantized linearly, so a `ClearRect` into an sRGB target stored 128 for linear 0.5 where the oracle
    /// and the hardware ROP store 188. Nothing caught it, because the differential's sRGB clear case goes
    /// through the render-pass load op, which is the hardware ROP and never reaches here.
    pub fn clear_texel(self, color: [f32; 4]) -> Result<Vec<u8>> {
        self.0
            .clear_texel(color)
            .ok_or(GpuError::Unsupported("wgpu: ClearRect for this format"))
    }

    pub fn clear_texel_f64(self, color: [f64; 4]) -> Result<Vec<u8>> {
        self.0
            .clear_texel_f64(color)
            .ok_or(GpuError::Unsupported("wgpu: ClearRect for this format"))
    }
}

#[cfg(test)]
mod clear_texel_tests {
    use super::Format;
    use hl_gpu::protocol::model::capability::{COLOR_FORMATS, INTEGER_FORMATS};

    const COLORS: &[[f32; 4]] = &[
        [0.0, 0.0, 0.0, 0.0],
        [1.0, 1.0, 1.0, 1.0],
        [0.5, 0.25, 0.75, 0.125],
        [4.0, -2.5, 65504.0, 1.0e-9],
        [-1.0, 2.0, 0.0, 1.0],
    ];

    /// The differential the pixel-level one cannot run: this backend and the software oracle must pack a
    /// clear colour into IDENTICAL bytes, for every format the capability set promises.
    ///
    /// It lives here, as a unit test, because it needs no GPU adapter and because the rendered-pixel
    /// differential is structurally blind to the question. Its sRGB clear case goes through the
    /// render-pass load op — the hardware ROP, which never calls `clear_texel` — and its oracle cannot
    /// draw into or clear any target the software rasterizer does not model. So `Enc::ClearRect`, the one
    /// path where this backend EMULATES a clear by uploading packed bytes because wgpu has no
    /// fixed-function equivalent, had no comparison against the oracle at all. It had also drifted: every
    /// channel was quantized linearly here, so a `ClearRect` into an sRGB target stored 128 for linear 0.5
    /// where the oracle and the ROP both store 188, while both copies carried a comment claiming to match
    /// the other.
    ///
    /// Agreement is now structural — both call one rule on `TextureFormat` — so what this defends is that
    /// the wgpu wrapper keeps DELEGATING rather than growing a copy again.
    #[test]
    fn this_backend_packs_the_same_bytes_as_the_shared_rule() {
        for &format in COLOR_FORMATS {
            for &color in COLORS {
                let shared = format
                    .clear_texel(color)
                    .expect("promised by COLOR_FORMATS");
                let mine = Format::from(format)
                    .clear_texel(color)
                    .expect("this backend must serve every promised format");
                assert_eq!(
                    mine, shared,
                    "{format:?} clear {color:?} must pack identically on both backends"
                );
            }
        }
    }

    /// An integer target packs here exactly as it does in the shared rule, and the refusal that remains
    /// still carries THIS backend's message so it says who refused.
    ///
    /// This asserted the opposite an hour ago. Refusing an integer clear here while `LoadOp::Clear` hands
    /// the same `[f32; 4]` to `wgpu::Color` — which a Uint/Sint target reads as the integer — left one
    /// driver's two clear routes disagreeing, and the GL layer had been reporting integer colour
    /// attachments COMPLETE the whole time.
    #[test]
    fn an_integer_target_packs_here_as_it_does_in_the_shared_rule() {
        for &format in INTEGER_FORMATS {
            let shared = format
                .clear_texel([200.0, 1.0, 0.0, 255.0])
                .expect("an integer target is clearable");
            assert_eq!(
                Format::from(format)
                    .clear_texel([200.0, 1.0, 0.0, 255.0])
                    .expect("and this backend must serve it"),
                shared,
                "{format:?} must pack identically on both backends"
            );
        }

        // The refusal that remains, with this backend's own message, so a caller can still tell who said
        // no — and so the agreement above is not mistaken for "everything packs".
        assert!(
            Format::from(hl_gpu::protocol::model::enums::TextureFormat::Depth32Float)
                .clear_texel([0.0; 4])
                .is_err(),
            "a format with no plain-colour texel is still refused"
        );
    }
}
