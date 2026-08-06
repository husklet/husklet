use super::decode::Aarch64SimdDecoder;
use crate::Aarch64DecodeError;

impl Aarch64SimdDecoder {
    pub(super) fn element(imm5: u8) -> Result<(u8, u8), Aarch64DecodeError> {
        if imm5 == 0 {
            return Err(Aarch64DecodeError::Reserved);
        }
        let size = imm5.trailing_zeros() as u8;
        if size > 3 {
            return Err(Aarch64DecodeError::Reserved);
        }
        Ok((8 << size, imm5 >> (size + 1)))
    }

    pub(super) fn expand_immediate(op: u32, cmode: u32, o2: u32, wide: bool, imm8: u64) -> Option<u64> {
        let selector = cmode >> 1 & 7;
        let low = cmode & 1;
        if selector != 7 && o2 != 0 {
            return None;
        }
        if selector == 7 && (low == 0 || op != 0) && o2 != 0 {
            return None;
        }
        if selector == 7 {
            return Self::expand_selector_seven(op, low, o2, wide, imm8);
        }
        let pattern = if selector <= 3 {
            let narrow = (imm8 << (8 * selector)) as u32;
            u64::from(narrow) << 32 | u64::from(narrow)
        } else if selector <= 5 {
            let narrow = (imm8 << (8 * (selector & 1))) as u16;
            u64::from(narrow) * 0x0001_0001_0001_0001
        } else if selector == 6 {
            let narrow = Self::moving_ones(low, imm8);
            narrow << 32 | narrow
        } else {
            unreachable!("selectors zero through six were exhaustively decoded")
        };
        Some(pattern)
    }

    pub(super) fn byte_mask(imm8: u64) -> u64 {
        let mut value = 0;
        for byte in 0..8 {
            value |= (((imm8 >> byte) & 1) * 0xff) << (8 * byte);
        }
        value
    }

    pub(super) fn moving_ones(high: u32, imm8: u64) -> u64 {
        if high != 0 {
            (imm8 << 16) | 0xffff
        } else {
            (imm8 << 8) | 0xff
        }
    }

    pub(super) fn expand_double_immediate(wide: bool, imm8: u64) -> Option<u64> {
        if !wide {
            return None;
        }
        Some(crate::aarch64_fp::ImmediateEncoding::splat(
            crate::FpFormat::Double,
            imm8 as u8,
        ))
    }

    pub(super) fn expand_selector_seven(operation: u32, low: u32, o2: u32, wide: bool, imm8: u64) -> Option<u64> {
        match (low, operation) {
            (0, 0) => Some(imm8 * 0x0101_0101_0101_0101),
            (0, _) => Some(Self::byte_mask(imm8)),
            (_, 0) => Some(crate::aarch64_fp::ImmediateEncoding::splat(
                if o2 == 0 {
                    crate::FpFormat::Single
                } else {
                    crate::FpFormat::Half
                },
                imm8 as u8,
            )),
            _ => Self::expand_double_immediate(wide, imm8),
        }
    }
}
