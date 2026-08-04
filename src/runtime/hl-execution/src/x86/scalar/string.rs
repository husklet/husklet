use crate::{
    CpuState, DecodedInstruction, ExecutionExit, Flag, FlagState, GuestOperandMemory, ScalarInstruction, ScalarIrError,
    ScalarRegister, ScalarWidth, VectorSource,
};

pub(crate) struct PackedString;

impl PackedString {
    pub(crate) fn decode(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if !decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        let wide_lengths = matches!(decoded.encoding, crate::Encoding::Legacy { rex: Some(rex), .. } if rex.w);
        Ok(ScalarInstruction::PackedString {
            left: decoded.register.ok_or(ScalarIrError::Invalid)?,
            right: super::Decoder::vector_source(decoded)?,
            control: decoded.immediate.ok_or(ScalarIrError::Invalid)?.0 as u8,
            explicit: decoded.opcode <= 0x61,
            mask: decoded.opcode & 1 == 0,
            wide_lengths,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        left: u8,
        right: VectorSource,
        control: u8,
        explicit: bool,
        mask: bool,
        wide_lengths: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let right = match crate::x86::VectorLane::read(cpu, memory, right, next, instruction) {
            Ok(v) => v,
            Err(e) => return e,
        };
        let left = cpu.vectors[usize::from(left)];
        let words = control & 1 != 0;
        let count = if words { 8 } else { 16 };
        let (la, lb) = if explicit {
            (
                Self::explicit_length(cpu.registers[0], wide_lengths, count),
                Self::explicit_length(cpu.registers[2], wide_lengths, count),
            )
        } else {
            (Self::length(left, words, count), Self::length(right, words, count))
        };
        let result = Self::result(left, right, la, lb, control, count);
        let index = if result == 0 {
            count
        } else if control & 0x40 != 0 {
            31 - result.leading_zeros()
        } else {
            result.trailing_zeros()
        };
        let mut staged = cpu.clone();
        staged.rip = next;
        if mask {
            staged.vectors[0] = Self::mask(result, control, count);
        } else {
            staged.write_register(ScalarRegister::General(1), ScalarWidth::Dword, u64::from(index));
        }
        staged.flags = FlagState::default()
            .with(Flag::Carry, result != 0)
            .with(Flag::Zero, lb < count)
            .with(Flag::Sign, la < count)
            .with(Flag::Overflow, result & 1 != 0);
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn explicit_length(value: u64, wide: bool, count: u32) -> u32 {
        let magnitude = if wide {
            (value as i64).unsigned_abs()
        } else {
            u64::from((value as i32).unsigned_abs())
        };
        magnitude.min(u64::from(count)) as u32
    }

    fn mask(result: u32, control: u8, count: u32) -> u128 {
        if control & 0x40 == 0 {
            return u128::from(result);
        }
        let words = control & 1 != 0;
        let bits = if words { 16 } else { 8 };
        let mut mask = 0_u128;
        for lane in 0..count {
            if result & (1 << lane) != 0 {
                mask |= ((1_u128 << bits) - 1) << (lane * bits);
            }
        }
        mask
    }

    fn length(vector: u128, words: bool, count: u32) -> u32 {
        (0..count)
            .find(|&i| Self::element(vector, i, words, false) == 0)
            .unwrap_or(count)
    }

    fn result(a: u128, b: u128, la: u32, lb: u32, control: u8, count: u32) -> u32 {
        let words = control & 1 != 0;
        let signed = control & 2 != 0;
        let aggregation = control >> 2 & 3;
        let mut result = 0_u32;
        for i in 0..count {
            let matched = match aggregation {
                0 => {
                    i < lb && (0..la).any(|j| Self::element(a, j, words, signed) == Self::element(b, i, words, signed))
                }
                1 => {
                    i < lb
                        && (0..la / 2).any(|j| {
                            let v = Self::element(b, i, words, signed);
                            Self::element(a, j * 2, words, signed) <= v
                                && v <= Self::element(a, j * 2 + 1, words, signed)
                        })
                }
                2 => {
                    i < la && i < lb && Self::element(a, i, words, signed) == Self::element(b, i, words, signed)
                        || i >= la && i >= lb
                }
                _ => (0..count - i).all(|j| {
                    j >= la
                        || i + j < lb && Self::element(a, j, words, signed) == Self::element(b, i + j, words, signed)
                }),
            };
            if matched {
                result |= 1 << i;
            }
        }
        if control & 0x10 != 0 {
            result ^= if control & 0x20 != 0 {
                (1 << lb) - 1
            } else {
                (1 << count) - 1
            };
        }
        result
    }

    fn element(vector: u128, index: u32, words: bool, signed: bool) -> i64 {
        if words {
            let v = (vector >> (index * 16)) as u16;
            if signed { i64::from(v as i16) } else { i64::from(v) }
        } else {
            let v = (vector >> (index * 8)) as u8;
            if signed { i64::from(v as i8) } else { i64::from(v) }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::{ScalarInstruction, X86ScalarDecoder};

    #[test]
    fn packed_string_family_decodes_output_and_length_forms() {
        for (opcode, explicit, mask) in [(0x60, true, true), (0x61, true, false), (0x62, false, true), (0x63, false, false)] {
            let decoded = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x3a, opcode, 0xc1, 0x08], 0).unwrap();
            assert!(matches!(
                decoded.instruction,
                ScalarInstruction::PackedString {
                    explicit: actual_explicit,
                    mask: actual_mask,
                    ..
                } if actual_explicit == explicit && actual_mask == mask
            ));
        }
    }

    #[test]
    fn packed_string_mask_and_explicit_lengths_match_sse42() {
        assert_eq!(super::PackedString::explicit_length(u64::from(u32::MAX - 2), false, 16), 3);
        assert_eq!(super::PackedString::explicit_length(u64::MAX - 2, true, 16), 3);
        assert_eq!(super::PackedString::mask(0b1001, 0, 16), 0b1001);
        assert_eq!(super::PackedString::mask(0b0101, 0x40, 16) & u128::from(u32::MAX), 0x00ff_00ff);
    }
}
