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
    pub fn is_shadow(self) -> bool {
        matches!(
            self.0,
            TextureFormat::R10x6g10x6b10x6a10x6Unorm
        )
    }

    pub fn needs_transfer_conversion(self) -> bool {
        self.is_shadow()
            || matches!(
                self.0,
                TextureFormat::R4g4b4a4Unorm
                    | TextureFormat::R5g5b5a1Unorm
                    | TextureFormat::A4r4g4b4Unorm
                    | TextureFormat::A4b4g4r4Unorm
            )
    }

    fn physical_protocol_format(self) -> TextureFormat {
        match self.0 {
            TextureFormat::R4g4b4a4Unorm
            | TextureFormat::A4r4g4b4Unorm
            | TextureFormat::A4b4g4r4Unorm => TextureFormat::B4g4r4a4Unorm,
            TextureFormat::R5g5b5a1Unorm => TextureFormat::A1r5g5b5Unorm,
            // R10X6 is normalized fixed point.  A normalized backing preserves its clamp, interpolation,
            // and sampling domain; the previous half-float backing introduced a second, unrelated rounding
            // system.  This still is not sufficient to advertise render-target support: fixed-function
            // blending stores 16 bits between draws, while Vulkan requires ten (see the semantic proofs
            // below).
            _ => TextureFormat::Rgba16Unorm,
        }
    }

    pub fn native_texel_bytes(self) -> Result<usize> {
        if self.needs_transfer_conversion() {
            self.physical_protocol_format()
                .bytes_per_texel()
                .ok_or(GpuError::Unsupported("wgpu: native texel layout"))
        } else {
            self.texel_bytes()
        }
    }

    pub fn logical_to_native(self, logical: &[u8]) -> Result<Vec<u8>> {
        let logical_bytes = self.texel_bytes()?;
        if !self.needs_transfer_conversion() {
            return Ok(logical.to_vec());
        }
        if !logical.len().is_multiple_of(logical_bytes) {
            return Err(GpuError::OutOfBounds);
        }
        let physical = self.physical_protocol_format();
        let physical_bytes = physical.bytes_per_texel().ok_or(GpuError::Unsupported(
            "wgpu: no packed physical texel layout",
        ))?;
        let mut native = Vec::with_capacity(logical.len() / logical_bytes * physical_bytes);
        for texel in logical.chunks_exact(logical_bytes) {
            let color = self
                .0
                .texel_to_f32(texel)
                .ok_or(GpuError::Unsupported("wgpu: shadow decode"))?;
            native.extend(
                physical
                    .clear_texel(color)
                    .ok_or(GpuError::Unsupported("wgpu: native packed encode"))?,
            );
        }
        Ok(native)
    }

    pub fn native_to_logical(self, native: &[u8]) -> Result<Vec<u8>> {
        if !self.needs_transfer_conversion() {
            return Ok(native.to_vec());
        }
        let physical = self.physical_protocol_format();
        let physical_bytes = physical.bytes_per_texel().ok_or(GpuError::Unsupported(
            "wgpu: no packed physical texel layout",
        ))?;
        if !native.len().is_multiple_of(physical_bytes) {
            return Err(GpuError::OutOfBounds);
        }
        let mut logical = Vec::with_capacity(native.len() / physical_bytes * self.texel_bytes()?);
        for texel in native.chunks_exact(physical_bytes) {
            let color = physical
                .texel_to_f32(texel)
                .ok_or(GpuError::Unsupported("wgpu: native packed decode"))?;
            logical.extend(
                self.0
                    .clear_texel(color)
                    .ok_or(GpuError::Unsupported("wgpu: shadow encode"))?,
            );
        }
        Ok(logical)
    }

    /// Return the native allocation format.
    pub fn native(self) -> wgpu::TextureFormat {
        use wgpu::TextureFormat as W;
        match self.0 {
            TextureFormat::Rgba8Unorm => W::Rgba8Unorm,
            TextureFormat::Bgra8Unorm => W::Bgra8Unorm,
            TextureFormat::Rgba8Srgb => W::Rgba8UnormSrgb,
            TextureFormat::Bgra8Srgb => W::Bgra8UnormSrgb,
            TextureFormat::R8Unorm => W::R8Unorm,
            TextureFormat::R8Snorm => W::R8Snorm,
            TextureFormat::Rg8Unorm => W::Rg8Unorm,
            TextureFormat::Rg8Snorm => W::Rg8Snorm,
            TextureFormat::Rgba8Snorm => W::Rgba8Snorm,
            TextureFormat::Rg16Float => W::Rg16Float,
            TextureFormat::Rg16Unorm => W::Rg16Unorm,
            TextureFormat::Rg16Snorm => W::Rg16Snorm,
            TextureFormat::Rgba16Snorm => W::Rgba16Snorm,
            TextureFormat::R16Unorm => W::R16Unorm,
            TextureFormat::R16Snorm => W::R16Snorm,
            TextureFormat::R16Float => W::R16Float,
            TextureFormat::R16Uint => W::R16Uint,
            TextureFormat::R16Sint => W::R16Sint,
            TextureFormat::Rg16Uint => W::Rg16Uint,
            TextureFormat::Rg16Sint => W::Rg16Sint,
            TextureFormat::Rgba16Uint => W::Rgba16Uint,
            TextureFormat::Rgba16Sint => W::Rgba16Sint,
            TextureFormat::Rg32Float => W::Rg32Float,
            TextureFormat::Rg32Uint => W::Rg32Uint,
            TextureFormat::Rg32Sint => W::Rg32Sint,
            TextureFormat::Rgb9e5Ufloat => W::Rgb9e5Ufloat,
            TextureFormat::Rgb10a2Unorm => W::Rgb10a2Unorm,
            TextureFormat::Rgb10a2Uint => W::Rgb10a2Uint,
            TextureFormat::Rg11b10Ufloat => W::Rg11b10Ufloat,
            TextureFormat::R5g6b5Unorm => W::R5g6b5Unorm,
            TextureFormat::A1r5g5b5Unorm => W::A1r5g5b5Unorm,
            TextureFormat::B4g4r4a4Unorm => W::B4g4r4a4Unorm,
            TextureFormat::R4g4b4a4Unorm
            | TextureFormat::A4r4g4b4Unorm
            | TextureFormat::A4b4g4r4Unorm => W::B4g4r4a4Unorm,
            TextureFormat::R5g5b5a1Unorm => W::A1r5g5b5Unorm,
            TextureFormat::R10x6g10x6b10x6a10x6Unorm => W::Rgba16Unorm,
            TextureFormat::Rgba16Float => W::Rgba16Float,
            TextureFormat::Rgba16Unorm => W::Rgba16Unorm,
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
            TextureFormat::Etc2Rgb8Unorm => W::Etc2Rgb8Unorm,
            TextureFormat::Etc2Rgb8Srgb => W::Etc2Rgb8UnormSrgb,
            TextureFormat::Etc2Rgb8A1Unorm => W::Etc2Rgb8A1Unorm,
            TextureFormat::Etc2Rgb8A1Srgb => W::Etc2Rgb8A1UnormSrgb,
            TextureFormat::Etc2Rgba8Unorm => W::Etc2Rgba8Unorm,
            TextureFormat::Etc2Rgba8Srgb => W::Etc2Rgba8UnormSrgb,
            TextureFormat::EacR11Unorm => W::EacR11Unorm,
            TextureFormat::EacR11Snorm => W::EacR11Snorm,
            TextureFormat::EacRg11Unorm => W::EacRg11Unorm,
            TextureFormat::EacRg11Snorm => W::EacRg11Snorm,
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
        // Unlike packed depth/stencil, Depth32Float has one copyable aspect whose texels are exactly
        // one IEEE-754 f32 each. Keep this backend-specific: the shared colour-layout rule correctly
        // refuses depth formats, while wgpu legally accepts buffer↔texture copies of the D32 depth
        // aspect. Depth24PlusStencil8 remains refused because neither its `All` aspect nor its depth
        // plane has a portable packed buffer representation.
        if self.0 == TextureFormat::Depth32Float {
            return width
                .checked_mul(core::mem::size_of::<f32>() as u32)
                .map(|bytes_per_row| (bytes_per_row, height))
                .ok_or(GpuError::OutOfBounds);
        }
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

#[cfg(test)]
mod depth_copy_layout_tests {
    use super::Format;
    use hl_gpu::protocol::model::enums::TextureFormat;
    use hl_gpu::GpuError;

    #[test]
    fn d32_has_the_legal_single_aspect_copy_layout() {
        assert_eq!(
            Format::from(TextureFormat::Depth32Float).copy_layout(7, 3),
            Ok((28, 3))
        );
        assert!(matches!(
            Format::from(TextureFormat::Depth32Float).copy_layout(u32::MAX, 1),
            Err(GpuError::OutOfBounds)
        ));
    }

    #[test]
    fn packed_depth_stencil_still_has_no_buffer_copy_layout() {
        assert!(matches!(
            Format::from(TextureFormat::Depth24PlusStencil8).copy_layout(7, 3),
            Err(GpuError::Unsupported("wgpu: texture copy layout"))
        ));
    }
}

#[cfg(test)]
mod packed_format_tests {
    use super::Format;
    use hl_gpu::protocol::model::enums::TextureFormat;

    #[test]
    fn logical_native_roundtrip_preserves_every_packed_encoding() {
        let cases: &[(TextureFormat, &[u8], wgpu::TextureFormat, usize)] = &[
            (
                TextureFormat::R4g4b4a4Unorm,
                &[0x5a, 0xc3],
                wgpu::TextureFormat::B4g4r4a4Unorm,
                2,
            ),
            (
                TextureFormat::R5g5b5a1Unorm,
                &[0x5b, 0xc3],
                wgpu::TextureFormat::A1r5g5b5Unorm,
                2,
            ),
            (
                TextureFormat::A4r4g4b4Unorm,
                &[0x5a, 0xc3],
                wgpu::TextureFormat::B4g4r4a4Unorm,
                2,
            ),
            (
                TextureFormat::A4b4g4r4Unorm,
                &[0x5a, 0xc3],
                wgpu::TextureFormat::B4g4r4a4Unorm,
                2,
            ),
            (
                TextureFormat::R10x6g10x6b10x6a10x6Unorm,
                &[0xc0, 0x55, 0x80, 0xaa, 0x40, 0x33, 0x00, 0xff],
                wgpu::TextureFormat::Rgba16Unorm,
                8,
            ),
            (
                TextureFormat::Rg11b10Ufloat,
                &[0xc0, 0x03, 0x1e, 0x78],
                wgpu::TextureFormat::Rg11b10Ufloat,
                4,
            ),
            (
                TextureFormat::Rgb9e5Ufloat,
                &[0x00, 0x01, 0x02, 0x84],
                wgpu::TextureFormat::Rgb9e5Ufloat,
                4,
            ),
        ];
        for &(logical, bytes, native_format, native_len) in cases {
            let format = Format::from(logical);
            assert_eq!(format.native(), native_format);
            let native = format.logical_to_native(bytes).expect("logical decode");
            assert_eq!(native.len(), native_len);
            assert_eq!(
                format.native_to_logical(&native).expect("logical encode"),
                bytes,
                "{logical:?}"
            );
        }
    }

    #[test]
    fn rgb9e5_uses_its_exact_native_layout() {
        let format = Format::from(TextureFormat::Rgb9e5Ufloat);
        assert_eq!(format.native(), wgpu::TextureFormat::Rgb9e5Ufloat);
        assert_eq!(format.native_texel_bytes().unwrap(), 4);
        assert!(!format.needs_transfer_conversion());
        assert_eq!(format.native().target_pixel_byte_cost(), Some(8));
        assert_eq!(format.native().target_component_alignment(), Some(4));
    }

    fn quantize(value: f64, levels: f64) -> f64 {
        (value.clamp(0.0, 1.0) * levels).round() / levels
    }

    #[test]
    fn r10x6_transfer_uses_normalized_storage_and_roundtrips_every_code() {
        let format = Format::from(TextureFormat::R10x6g10x6b10x6a10x6Unorm);
        assert_eq!(format.native(), wgpu::TextureFormat::Rgba16Unorm);
        for code in 0_u16..=1023 {
            let word = (code << 6).to_le_bytes();
            let logical = [word, word, word, word].concat();
            let native = format.logical_to_native(&logical).unwrap();
            assert_eq!(native.len(), 8);
            assert_eq!(format.native_to_logical(&native).unwrap(), logical, "code {code}");
        }
    }

    #[test]
    fn r10x6_clear_and_single_store_have_a_lossless_ten_bit_projection() {
        // A clear or isolated fragment store may land in 16-bit UNORM and then be projected to ten bits:
        // every value already representable by the logical attachment survives that projection exactly.
        for code in 0_u16..=1023 {
            let logical = f64::from(code) / 1023.0;
            let native = quantize(logical, 65535.0);
            assert_eq!(quantize(native, 1023.0), logical, "code {code}");
        }
    }

    #[test]
    fn r10x6_blend_requires_quantization_after_each_draw() {
        // Draw one stores `destination`; draw two additively blends `source`.  A genuine R10X6 attachment
        // quantizes the destination after draw one.  An RGBA16Unorm shadow retains six extra bits, and even
        // quantizing at pass end cannot recover the required result.
        let destination = 0.29552093575793315;
        let source = 0.5116272819654833;
        let required = quantize(quantize(destination, 1023.0) + source, 1023.0);
        let shadow = quantize(quantize(quantize(destination, 65535.0) + source, 65535.0), 1023.0);
        assert_eq!(required, 825.0 / 1023.0);
        assert_eq!(shadow, 826.0 / 1023.0);
        assert_ne!(shadow, required, "RGBA16Unorm alone must never justify attachment advertisement");
    }

    #[test]
    fn r10x6_same_frame_sampling_requires_pass_end_quantization() {
        let fragment = 0.29552093575793315;
        let required_sample = quantize(fragment, 1023.0);
        let shadow_sample = quantize(fragment, 65535.0);
        assert_ne!(shadow_sample, required_sample);
        assert_eq!(quantize(shadow_sample, 1023.0), required_sample);
    }

    #[test]
    fn rgb9e5_transfer_bytes_are_never_repacked() {
        let format = Format::from(TextureFormat::Rgb9e5Ufloat);
        let texels = [
            [0x00, 0x00, 0x00, 0x00],
            [0x00, 0x01, 0x00, 0x80], // (1, 0, 0), shared exponent 16
            [0x00, 0x01, 0x02, 0x84], // (1, 1, 1), shared exponent 16
            [0xff, 0xff, 0xff, 0xff],
        ]
        .concat();
        assert_eq!(format.logical_to_native(&texels).unwrap(), texels);
        assert_eq!(format.native_to_logical(&texels).unwrap(), texels);
    }
}
