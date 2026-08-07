use std::{error::Error, fmt};

use super::{atomic, system};
use crate::{
    Aarch64BranchCondition, Aarch64Instruction, Aarch64Ir, aarch64_fp::Aarch64FpDecoder,
    aarch64_memory::Aarch64MemoryDecoder, aarch64_simd_decode::Aarch64SimdDecoder,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DecodeError {
    Reserved,
    Unsupported,
}
pub type Aarch64DecodeError = DecodeError;

impl fmt::Display for Aarch64DecodeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reserved => formatter.write_str("reserved AArch64 encoding"),
            Self::Unsupported => formatter.write_str("unsupported AArch64 instruction"),
        }
    }
}

impl Error for Aarch64DecodeError {}

pub struct Decoder;
pub type Aarch64Decoder = Decoder;

impl Aarch64Decoder {
    // Field masks are written as the manual writes them.
    #[allow(clippy::verbose_bit_mask)]
    pub fn decode(word: u32) -> Result<Aarch64Ir, Aarch64DecodeError> {
        let wide = word >> 31 != 0;
        if word >> 25 & 0xf == 0 {
            return Ok(Self::ir(word, Aarch64Instruction::Undefined));
        }
        if word & 0x1f00_0000 == 0x1000_0000 {
            return Ok(Self::ir(word, Self::address(word)));
        }
        if word & 0x1f00_0000 == 0x1100_0000 {
            return Ok(Self::ir(word, Self::add_immediate(word)));
        }
        if word & 0x1f80_0000 == 0x1200_0000 {
            return Ok(Self::ir(word, Self::logical_immediate(word, wide)?));
        }
        if word & 0x1f80_0000 == 0x1280_0000 {
            return Ok(Self::ir(word, Self::move_wide(word, wide)?));
        }
        if word & 0x1f80_0000 == 0x1300_0000 {
            return Ok(Self::ir(word, Self::bitfield(word, wide)?));
        }
        if word & 0x7fa0_0000 == 0x1380_0000 {
            return Ok(Self::ir(word, Self::extract(word, wide)?));
        }
        if word & 0x1f20_0000 == 0x0b00_0000 {
            return Ok(Self::ir(word, Self::add_shifted(word, wide)?));
        }
        if word & 0x1f20_0000 == 0x0b20_0000 {
            return Ok(Self::ir(word, Self::add_extended(word, wide)?));
        }
        if word & 0x1fe0_fc00 == 0x1a00_0000 {
            return Ok(Self::ir(
                word,
                Aarch64Instruction::AddCarry {
                    subtract: word >> 30 & 1 != 0,
                    set_flags: word >> 29 & 1 != 0,
                    left: u8::try_from(word >> 5 & 31).expect("masked register field fits u8"),
                    right: u8::try_from(word >> 16 & 31).expect("masked register field fits u8"),
                    destination: u8::try_from(word & 31).expect("masked register field fits u8"),
                },
            ));
        }
        if word & 0x1f00_0000 == 0x0a00_0000 {
            return Ok(Self::ir(word, Self::logical_shifted(word, wide)?));
        }
        if word & 0x1f00_0000 == 0x1b00_0000 {
            return Ok(Self::ir(word, Self::multiply(word, wide)?));
        }
        if word & 0x7fff_fc00 == 0x5ac0_0000 {
            return Ok(Self::ir(
                word,
                Aarch64Instruction::BitReverse {
                    source: u8::try_from(word >> 5 & 31).expect("masked register field fits u8"),
                    destination: u8::try_from(word & 31).expect("masked register field fits u8"),
                },
            ));
        }
        if word & 0x7fff_0000 == 0x5ac0_0000 && matches!(word >> 10 & 0x3f, 1..=3) {
            return Ok(Self::ir(word, Self::byte_reverse(word, wide)?));
        }
        if word & 0x7fff_fc00 == 0x5ac0_1000 {
            return Ok(Self::ir(
                word,
                Aarch64Instruction::CountLeadingZero {
                    source: u8::try_from(word >> 5 & 31).expect("masked register field fits u8"),
                    destination: u8::try_from(word & 31).expect("masked register field fits u8"),
                },
            ));
        }
        if word & 0x1fe0_f000 == 0x1ac0_2000 {
            return Ok(Self::ir(word, Self::variable_shift(word)?));
        }
        if word & 0x1fe0_f800 == 0x1ac0_0800 {
            return Ok(Self::ir(word, Self::divide(word)?));
        }
        if let Some(decoded) = super::crc32::Crc::decode(word) {
            return Ok(Self::ir(word, decoded));
        }
        if word & 0x1fe0_0000 == 0x1a40_0000 {
            return Ok(Self::ir(word, Self::conditional_compare(word)?));
        }
        // The top-level A64 encoding map makes the load/store group disjoint
        // from scalar FP and SIMD. Route it before those larger decoder trees;
        // libc-heavy guests execute far more memory operations than vectors.
        if matches!(word >> 25 & 0xf, 4 | 6 | 12 | 14) {
            if let Some(decoded) = atomic::Decoder::decode(word) {
                return Ok(Self::ir(word, decoded?));
            }
            return Ok(Self::ir(word, Aarch64MemoryDecoder::decode(word)?));
        }
        if word >> 25 & 0x7 == 0x7 {
            if let Some(decoded) = Aarch64FpDecoder::decode(word) {
                return Ok(Self::ir(word, decoded?));
            }
            if let Some(decoded) = Aarch64SimdDecoder::decode(word) {
                return Ok(Self::ir(word, decoded?));
            }
        }
        if let Some(decoded) = system::Decoder::decode(word) {
            return Ok(Self::ir(word, decoded?));
        }
        if word & 0x7c00_0000 == 0x1400_0000 {
            return Ok(Self::ir(
                word,
                Aarch64Instruction::BranchImmediate {
                    displacement: Self::sign_extend(u64::from(word & 0x03ff_ffff), 26) << 2,
                    link: word >> 31 != 0,
                },
            ));
        }
        if word & 0xff00_0000 == 0x5400_0000 {
            return Ok(Self::ir(
                word,
                Aarch64Instruction::BranchConditional {
                    condition: Aarch64BranchCondition(u8::try_from(word & 15).expect("masked condition field fits u8")),
                    displacement: Self::sign_extend(u64::from(word >> 5 & 0x7ffff), 19) << 2,
                },
            ));
        }
        if word & 0x7e00_0000 == 0x3400_0000 {
            return Ok(Self::ir(
                word,
                Aarch64Instruction::CompareBranch {
                    source: u8::try_from(word & 31).expect("masked register field fits u8"),
                    nonzero: word >> 24 & 1 != 0,
                    displacement: Self::sign_extend(u64::from(word >> 5 & 0x7ffff), 19) << 2,
                },
            ));
        }
        if word & 0x7e00_0000 == 0x3600_0000 {
            return Ok(Self::ir(
                word,
                Aarch64Instruction::TestBranch {
                    source: u8::try_from(word & 31).expect("masked register field fits u8"),
                    bit: u8::try_from(((word >> 31 & 1) << 5) | (word >> 19 & 31)).expect("six-bit test field fits u8"),
                    nonzero: word >> 24 & 1 != 0,
                    displacement: Self::sign_extend(u64::from(word >> 5 & 0x3fff), 14) << 2,
                },
            ));
        }
        if word & 0x3fe0_0800 == 0x1a80_0000 {
            return Ok(Self::ir(
                word,
                Aarch64Instruction::ConditionalSelect {
                    source: u8::try_from(word >> 5 & 31).expect("masked register field fits u8"),
                    alternate: u8::try_from(word >> 16 & 31).expect("masked register field fits u8"),
                    destination: u8::try_from(word & 31).expect("masked register field fits u8"),
                    condition: Aarch64BranchCondition(
                        u8::try_from(word >> 12 & 15).expect("masked condition field fits u8"),
                    ),
                    invert: word >> 30 & 1 != 0,
                    increment: word >> 10 & 1 != 0,
                },
            ));
        }
        if word & 0xfe00_0000 == 0xd600_0000 {
            return Ok(Self::ir(word, Self::branch_register(word)?));
        }
        if word & 0xffe0_001f == 0xd400_0001 {
            return Ok(Self::ir(
                word,
                Aarch64Instruction::SupervisorCall {
                    immediate: u16::try_from(word >> 5 & 0xffff).expect("masked supervisor-call immediate fits u16"),
                },
            ));
        }
        if word & 0xffe0_001f == 0xd420_0000 {
            return Ok(Self::ir(
                word,
                Aarch64Instruction::Breakpoint {
                    immediate: u16::try_from(word >> 5 & 0xffff).expect("masked breakpoint immediate fits u16"),
                },
            ));
        }
        Err(Aarch64DecodeError::Unsupported)
    }
}
