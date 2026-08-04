use crate::{Aarch64DecodeError, Aarch64Instruction, SimdReduceOperation};

pub(crate) struct Pair;

impl Pair {
    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        if word & 0x9f20_0c00 != 0x1e20_0800 || word >> 12 & 31 != 0x1b {
            return None;
        }
        if word & 0xffff_fc00 != 0x5ef1_b800 {
            return Some(Err(Aarch64DecodeError::Reserved));
        }
        Some(Ok(Aarch64Instruction::SimdReduce {
            operation: SimdReduceOperation::Add,
            lane_bits: 64,
            source: (word >> 5 & 31) as u8,
            destination: (word & 31) as u8,
            wide: true,
        }))
    }
}
