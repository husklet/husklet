//! Exact decoding support for Vulkan's opaque BC1 RGB spellings.
//!
//! WebGPU exposes BC1 only with RGBA semantics: selector 3 in a three-colour block has alpha zero.
//! Vulkan's BC1 RGB formats use the same RGB palette but require that selector to be opaque.  Therefore
//! the compressed bytes can be preserved for transfers, but sampled backing must be decoded with alpha 1.

fn rgb565(value: u16) -> [u8; 3] {
    let r = ((value >> 11) & 31) as u8;
    let g = ((value >> 5) & 63) as u8;
    let b = (value & 31) as u8;
    [
        (u16::from(r) * 255 / 31) as u8,
        (u16::from(g) * 255 / 63) as u8,
        (u16::from(b) * 255 / 31) as u8,
    ]
}

/// Decode one BC1 RGB block to sixteen opaque RGBA8 texels in row-major order.
pub(crate) fn decode_opaque(block: [u8; 8]) -> [[u8; 4]; 16] {
    let c0 = u16::from_le_bytes([block[0], block[1]]);
    let c1 = u16::from_le_bytes([block[2], block[3]]);
    let a = rgb565(c0);
    let b = rgb565(c1);
    let mut palette = [[0u8; 4]; 4];
    palette[0] = [a[0], a[1], a[2], 255];
    palette[1] = [b[0], b[1], b[2], 255];
    if c0 > c1 {
        for channel in 0..3 {
            palette[2][channel] = ((2 * u16::from(a[channel]) + u16::from(b[channel])) / 3) as u8;
            palette[3][channel] = ((u16::from(a[channel]) + 2 * u16::from(b[channel])) / 3) as u8;
        }
    } else {
        for channel in 0..3 {
            palette[2][channel] = ((u16::from(a[channel]) + u16::from(b[channel])) / 2) as u8;
        }
        // Selector 3 is opaque black for BC1_RGB, unlike BC1_RGBA's transparent black.
        palette[3] = [0, 0, 0, 255];
    }
    palette[2][3] = 255;
    palette[3][3] = 255;

    let selectors = u32::from_le_bytes([block[4], block[5], block[6], block[7]]);
    core::array::from_fn(|i| palette[((selectors >> (i * 2)) & 3) as usize])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(c0: u16, c1: u16) -> [u8; 8] {
        // Repeat selectors 0,1,2,3 across every row.
        let selectors = 0b11_10_01_00u32 * 0x0101_0101;
        let mut out = [0; 8];
        out[..2].copy_from_slice(&c0.to_le_bytes());
        out[2..4].copy_from_slice(&c1.to_le_bytes());
        out[4..].copy_from_slice(&selectors.to_le_bytes());
        out
    }

    #[test]
    fn four_colour_block_keeps_weighted_palette_and_is_opaque() {
        let pixels = decode_opaque(block(0xf800, 0x001f));
        assert_eq!(
            &pixels[..4],
            &[
                [255, 0, 0, 255],
                [0, 0, 255, 255],
                [170, 0, 85, 255],
                [85, 0, 170, 255]
            ]
        );
    }

    #[test]
    fn three_colour_selector_three_is_opaque_black() {
        let pixels = decode_opaque(block(0x001f, 0xf800));
        assert_eq!(
            &pixels[..4],
            &[
                [0, 0, 255, 255],
                [255, 0, 0, 255],
                [127, 0, 127, 255],
                [0, 0, 0, 255]
            ]
        );
    }

    #[test]
    fn equal_endpoints_still_make_selector_three_opaque_black() {
        let pixels = decode_opaque(block(0x07e0, 0x07e0));
        assert_eq!(pixels[0], [0, 255, 0, 255]);
        assert_eq!(pixels[2], [0, 255, 0, 255]);
        assert_eq!(pixels[3], [0, 0, 0, 255]);
    }
}
