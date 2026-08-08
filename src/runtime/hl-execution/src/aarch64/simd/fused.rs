use crate::{Aarch64DecodeError, Aarch64Instruction, FpFormat};

pub(crate) struct FusedAccumulator;

impl FusedAccumulator {
    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        let vector = match word & 0x9fe0_fc00 {
            0x0e40_0c00 | 0x0ec0_0c00 => return Some(Ok(Self::half(word, word >> 23 & 1 != 0, None, false))),
            0x0e20_cc00 | 0x0e60_cc00 => Some(false),
            0x0ea0_cc00 | 0x0ee0_cc00 => Some(true),
            _ => None,
        };
        if let Some(subtract) = vector {
            return Some(Self::instruction(word, subtract, None, false));
        }
        let scalar = word & 0x5000_0000 == 0x5000_0000;
        let indexed = match word & 0xdf80_f400 {
            0x0f00_1000 | 0x4f00_1000 | 0x5f00_1000 => {
                return Some(Ok(Self::half(word, false, Some(Self::half_index(word)), scalar)));
            }
            0x0f00_5000 | 0x4f00_5000 | 0x5f00_5000 => {
                return Some(Ok(Self::half(word, true, Some(Self::half_index(word)), scalar)));
            }
            0x0f80_1000 | 0x4f80_1000 | 0x5f80_1000 => Some(false),
            0x0f80_5000 | 0x4f80_5000 | 0x5f80_5000 => Some(true),
            _ => None,
        }?;
        let double = word & 1 << 22 != 0;
        let index = if double {
            (word >> 11 & 1) as u8
        } else {
            ((word >> 21 & 1) | (word >> 10 & 2)) as u8
        };
        Some(Self::instruction(word, indexed, Some(index), scalar))
    }

    fn half(word: u32, subtract: bool, index: Option<u8>, scalar: bool) -> Aarch64Instruction {
        Aarch64Instruction::SimdFpFused {
            format: FpFormat::Half,
            lanes: if scalar {
                1
            } else if word >> 30 & 1 != 0 {
                8
            } else {
                4
            },
            subtract,
            left: (word >> 5 & 31) as u8,
            right: if index.is_some() {
                (word >> 16 & 15) as u8
            } else {
                (word >> 16 & 31) as u8
            },
            index,
            destination: (word & 31) as u8,
            scalar,
        }
    }

    fn half_index(word: u32) -> u8 {
        ((word >> 11 & 1) | (word >> 20 & 2) | (word >> 18 & 4)) as u8
    }

    fn instruction(
        word: u32,
        subtract: bool,
        index: Option<u8>,
        scalar: bool,
    ) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let double = word & 1 << 22 != 0;
        let wide = word & 1 << 30 != 0;
        if double && !wide {
            return Err(Aarch64DecodeError::Reserved);
        }
        Ok(Aarch64Instruction::SimdFpFused {
            format: if double { FpFormat::Double } else { FpFormat::Single },
            lanes: if scalar {
                1
            } else if double {
                2
            } else if wide {
                4
            } else {
                2
            },
            subtract,
            left: (word >> 5 & 31) as u8,
            right: (word >> 16 & 31) as u8,
            index,
            destination: (word & 31) as u8,
            scalar,
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        Aarch64CpuState, Aarch64ExecutionExit, Aarch64FpExecutor, Aarch64Ir, Aarch64SoftFloat, FPSR_INEXACT,
        FPSR_INPUT_DENORMAL, FPSR_INVALID, FPSR_OVERFLOW, FPSR_UNDERFLOW, FpArithmetic, FpArithmeticPort, FpRequest,
        Nzcv,
    };

    #[test]
    fn encodings() {
        for (base, lanes) in [(0x0e40_0c00, 4), (0x4e40_0c00, 8)] {
            for subtract in [false, true] {
                vector_encodings(base, FpFormat::Half, lanes, subtract);
            }
        }
        for (base, lanes, scalar) in [(0x0f00_1000, 4, false), (0x4f00_1000, 8, false), (0x5f00_1000, 1, true)] {
            for subtract in [false, true] {
                half_indexed_encodings(base, lanes, scalar, subtract);
            }
        }
        for (base, format, lanes) in [
            (0x0e20_cc00, FpFormat::Single, 2),
            (0x4e20_cc00, FpFormat::Single, 4),
            (0x4e60_cc00, FpFormat::Double, 2),
        ] {
            for subtract in [false, true] {
                vector_encodings(base, format, lanes, subtract);
            }
        }
        for (base, format, lanes, indexes) in [
            (0x0f80_1000, FpFormat::Single, 2, 4),
            (0x4f80_1000, FpFormat::Single, 4, 4),
            (0x4fc0_1000, FpFormat::Double, 2, 2),
        ] {
            for subtract in [false, true] {
                indexed_encodings(base, format, lanes, indexes, subtract, false);
            }
        }
        for (base, format, indexes) in [(0x5f80_1000, FpFormat::Single, 4), (0x5fc0_1000, FpFormat::Double, 2)] {
            for subtract in [false, true] {
                indexed_encodings(base, format, 1, indexes, subtract, true);
            }
        }
        for word in [0x0e60_cc00, 0x0ee0_cc00, 0x0fc0_1000, 0x0fc0_5000] {
            assert_eq!(FusedAccumulator::decode(word), Some(Err(Aarch64DecodeError::Reserved)));
        }
        assert_eq!(FusedAccumulator::decode(0x4e20_dc00), None);
    }

    fn vector_encodings(base: u32, format: FpFormat, lanes: u8, subtract: bool) {
        let base = base | u32::from(subtract) << 23;
        for encoded in 0_u32..32 * 32 * 32 {
            let left = encoded / 1024;
            let right = encoded / 32 % 32;
            let destination = encoded % 32;
            assert_eq!(
                FusedAccumulator::decode(base | right << 16 | left << 5 | destination),
                Some(Ok(Aarch64Instruction::SimdFpFused {
                    format,
                    lanes,
                    subtract,
                    left: left as u8,
                    right: right as u8,
                    index: None,
                    destination: destination as u8,
                    scalar: false
                }))
            );
        }
    }

    fn indexed_encodings(base: u32, format: FpFormat, lanes: u8, indexes: u8, subtract: bool, scalar: bool) {
        let base = base | u32::from(subtract) << 14;
        for index in 0..indexes {
            for encoded in 0_u32..32 * 32 * 32 {
                let left = encoded / 1024;
                let right = encoded / 32 % 32;
                let destination = encoded % 32;
                let index_bits = encode_index(format, index);
                assert_eq!(
                    FusedAccumulator::decode(base | index_bits | right << 16 | left << 5 | destination),
                    Some(Ok(Aarch64Instruction::SimdFpFused {
                        format,
                        lanes,
                        subtract,
                        left: left as u8,
                        right: right as u8,
                        index: Some(index),
                        destination: destination as u8,
                        scalar
                    }))
                );
            }
        }
    }

    fn encode_index(format: FpFormat, index: u8) -> u32 {
        if format == FpFormat::Double {
            u32::from(index) << 11
        } else {
            u32::from(index & 1) << 21 | u32::from(index >> 1) << 11
        }
    }

    fn half_indexed_encodings(base: u32, lanes: u8, scalar: bool, subtract: bool) {
        for index in 0_u32..8 {
            for encoded in 0_u32..32 * 16 * 32 {
                let left = encoded / 512;
                let right = encoded / 32 % 16;
                let destination = encoded % 32;
                let index_bits = (index & 1) << 11 | (index & 2) << 20 | (index & 4) << 18;
                let word = base | u32::from(subtract) << 14 | index_bits | right << 16 | left << 5 | destination;
                assert_eq!(
                    FusedAccumulator::decode(word),
                    Some(Ok(Aarch64Instruction::SimdFpFused {
                        format: FpFormat::Half,
                        lanes,
                        subtract,
                        left: left as u8,
                        right: right as u8,
                        index: Some(index as u8),
                        destination: destination as u8,
                        scalar
                    }))
                );
            }
        }
    }

    #[test]
    fn fused_rounding() {
        let a = 0x3f80_0001_u32;
        let b = 0x3f80_0003_u32;
        let c = 0xbf80_0000_u32;
        let fused = evaluate(
            FpArithmetic::FusedMultiplyAdd,
            FpFormat::Single,
            u64::from(a),
            u64::from(b),
            u64::from(c),
            0,
        );
        let product = evaluate(
            FpArithmetic::Binary(crate::FpBinaryOperation::Multiply),
            FpFormat::Single,
            u64::from(a),
            u64::from(b),
            0,
            0,
        );
        let split = evaluate(
            FpArithmetic::Binary(crate::FpBinaryOperation::Add),
            FpFormat::Single,
            product.value,
            u64::from(c),
            0,
            0,
        );
        assert_ne!(fused.value, split.value);

        let mut cpu = Aarch64CpuState {
            pc: 0x800,
            nzcv: Nzcv::from_bits(0x9000_0000),
            ..Default::default()
        };
        cpu.set_vector(0, repeat32(c));
        cpu.set_vector(1, repeat32(a));
        cpu.set_vector(2, repeat32(b));
        execute(&mut cpu, 0x4e22_cc20);
        assert_eq!(cpu.vector_lane(0, 32, 0), fused.value);
        assert_eq!(cpu.vector_lane(0, 32, 3), fused.value);
        assert_eq!(cpu.pc, 0x804);
        assert_eq!(cpu.nzcv.bits(), 0x9000_0000);

        let subtracted = evaluate(
            FpArithmetic::FusedMultiplyAdd,
            FpFormat::Single,
            u64::from(a ^ 1 << 31),
            u64::from(b),
            u64::from(c),
            0,
        );
        cpu.set_vector(0, repeat32(c));
        execute(&mut cpu, 0x4ea2_cc20);
        assert_eq!(cpu.vector_lane(0, 32, 0), subtracted.value);
        cpu.set_vector(0, u128::MAX);
        cpu.set_vector(2, repeat32(b));
        execute(&mut cpu, 0x0f82_1820);
        let indexed = evaluate(
            FpArithmetic::FusedMultiplyAdd,
            FpFormat::Single,
            u64::from(a),
            u64::from(b),
            u64::from(u32::MAX),
            0,
        );
        assert_eq!(cpu.vector_lane(0, 32, 0), indexed.value);
        assert_eq!(cpu.vector(0) >> 64, 0);
    }

    #[test]
    fn half_fused() {
        let (left, right, expected) = find_discriminator();
        let mut cpu = Aarch64CpuState::default();
        cpu.set_vector(0, repeat16(0xbc00));
        cpu.set_vector(1, repeat16(left as u16));
        cpu.set_vector(2, repeat16(right as u16));
        execute(&mut cpu, 0x4e42_0c20);
        assert_eq!(cpu.vector_lane(0, 16, 0), expected.value);
        cpu.pc = 0;
        cpu.set_vector(0, u128::MAX << 16 | 0xbc00);
        cpu.set_vector(1, u128::from(left));
        cpu.set_vector(2, u128::from(right) << 112);
        execute(&mut cpu, 0x5f32_1820);
        assert_eq!(cpu.vector(0), u128::from(expected.value));
    }

    #[test]
    fn half_random() {
        let mut random = 0x85a3_08d3_u32;
        let mut seen = 0;
        for fpcr in [0, 1 << 19, 1 << 25, 1 << 22, 2 << 22, 3 << 22, 2] {
            seen |= half_samples(&mut random, fpcr, false);
            seen |= half_samples(&mut random, fpcr, true);
        }
        assert_ne!(seen & (FPSR_INVALID | FPSR_OVERFLOW | FPSR_UNDERFLOW | FPSR_INEXACT), 0);
    }

    #[test]
    fn half_ah() {
        let mut cpu = Aarch64CpuState::default();
        cpu.set_vector(0, repeat16(0x7e55));
        cpu.set_vector(1, 0);
        cpu.set_vector(2, repeat16(0x7c00));
        execute(&mut cpu, 0x4e42_0c20);
        assert_eq!(cpu.vector_lane(0, 16, 0), 0x7e00);
        assert_ne!(cpu.fpsr & u64::from(FPSR_INVALID), 0);
        cpu.pc = 0;
        cpu.fpcr = 2;
        cpu.fpsr = 0;
        cpu.set_vector(0, repeat16(0x7e55));
        execute(&mut cpu, 0x4e42_0c20);
        assert_eq!(cpu.vector_lane(0, 16, 0), 0x7e55);
        assert_eq!(cpu.fpsr & u64::from(FPSR_INVALID), 0);
    }

    #[test]
    fn scalar_semantics() {
        let a = 0x3f80_0001_u32;
        let b = 0x3f80_0003_u32;
        let c = 0xbf80_0000_u32;
        let expected = evaluate(
            FpArithmetic::FusedMultiplyAdd,
            FpFormat::Single,
            u64::from(a),
            u64::from(b),
            u64::from(c),
            0,
        );
        let mut cpu = Aarch64CpuState {
            pc: 0x4035_0c,
            nzcv: Nzcv::from_bits(0x6000_0000),
            fpsr: 1 << 27,
            ..Default::default()
        };
        cpu.set_vector(29, u128::from(a) | u128::from(b) << 96);
        cpu.set_vector(31, u128::from(c) | u128::MAX << 32);
        execute(&mut cpu, 0x5fbd_1bbf);
        assert_eq!(cpu.vector(31), u128::from(expected.value));
        assert_eq!(cpu.fpsr, 1 << 27 | u64::from(expected.exceptions));
        assert_eq!(cpu.pc, 0x4035_10);
        assert_eq!(cpu.nzcv.bits(), 0x6000_0000);

        let left = 0x3ff0_0000_0000_0001_u64;
        let right = 0x3ff0_0000_0000_0003_u64;
        let addend = 0xbff0_0000_0000_0000_u64;
        let expected = evaluate(
            FpArithmetic::FusedMultiplyAdd,
            FpFormat::Double,
            left ^ 1 << 63,
            right,
            addend,
            0,
        );
        cpu.pc = 0;
        cpu.set_vector(9, u128::from(left));
        cpu.set_vector(10, u128::from(right) << 64);
        cpu.set_vector(8, u128::from(addend) | u128::MAX << 64);
        execute(&mut cpu, 0x5fca_5928);
        assert_eq!(cpu.vector(8), u128::from(expected.value));
        assert_eq!(cpu.pc, 4);
    }

    #[test]
    fn aliases_flags_random() {
        let mut random = 0x243f_6a88_u32;
        let mut seen = 0_u32;
        for fpcr in [0, 1 << 24, 1 << 25, 1 << 22, 2 << 22, 3 << 22] {
            for _ in 0..2_000 {
                let values = [next(&mut random), next(&mut random), next(&mut random)];
                let expected = evaluate(
                    FpArithmetic::FusedMultiplyAdd,
                    FpFormat::Single,
                    u64::from(values[0]),
                    u64::from(values[1]),
                    u64::from(values[2]),
                    fpcr,
                );
                seen |= expected.exceptions;
                let mut cpu = Aarch64CpuState {
                    fpcr: u64::from(fpcr),
                    fpsr: 1 << 27,
                    ..Default::default()
                };
                cpu.set_vector(0, repeat32(values[2]));
                cpu.set_vector(1, repeat32(values[0]));
                cpu.set_vector(2, repeat32(values[1]));
                execute(&mut cpu, 0x4e22_cc20);
                assert_eq!(cpu.vector_lane(0, 32, 0), expected.value);
                assert_eq!(cpu.fpsr, 1 << 27 | u64::from(expected.exceptions));
                cpu.pc = 0;
                cpu.set_vector(0, repeat32(values[0]));
                let alias = evaluate(
                    FpArithmetic::FusedMultiplyAdd,
                    FpFormat::Single,
                    u64::from(values[0]),
                    u64::from(values[0]),
                    u64::from(values[0]),
                    fpcr,
                );
                execute(&mut cpu, 0x4e20_cc00);
                assert_eq!(cpu.vector_lane(0, 32, 0), alias.value);
            }
        }
        assert_eq!(
            seen & (FPSR_INVALID | FPSR_OVERFLOW | FPSR_UNDERFLOW | FPSR_INEXACT | FPSR_INPUT_DENORMAL),
            FPSR_INVALID | FPSR_OVERFLOW | FPSR_UNDERFLOW | FPSR_INEXACT | FPSR_INPUT_DENORMAL
        );
    }

    #[test]
    fn scalar_random_reference() {
        let mut random = 0x1319_8a2e_u32;
        for fpcr in [0, 1 << 24, 1 << 25, 1 << 22, 2 << 22, 3 << 22] {
            for subtract in [false, true] {
                scalar_samples(&mut random, FpFormat::Single, 3, subtract, fpcr);
                scalar_samples(&mut random, FpFormat::Double, 1, subtract, fpcr);
            }
        }
    }

    fn scalar_samples(random: &mut u32, format: FpFormat, index: u8, subtract: bool, fpcr: u32) {
        for _ in 0..500 {
            scalar_case(random, format, index, subtract, fpcr);
        }
    }

    fn find_discriminator() -> (u64, u64, crate::FpResult) {
        for left in (0x0400_u64..0x7c00).step_by(37) {
            if let Some(found) = discriminator_for(left) {
                return found;
            }
        }
        panic!("half fused discriminator")
    }

    fn discriminator_for(left: u64) -> Option<(u64, u64, crate::FpResult)> {
        for right in (0x0400_u64..0x7c00).step_by(53) {
            let fused = evaluate(FpArithmetic::FusedMultiplyAdd, FpFormat::Half, left, right, 0xbc00, 0);
            let product = evaluate(
                FpArithmetic::Binary(crate::FpBinaryOperation::Multiply),
                FpFormat::Half,
                left,
                right,
                0,
                0,
            );
            let split = evaluate(
                FpArithmetic::Binary(crate::FpBinaryOperation::Add),
                FpFormat::Half,
                product.value,
                0xbc00,
                0,
                0,
            );
            if fused.value != split.value {
                return Some((left, right, fused));
            }
        }
        None
    }

    fn half_samples(random: &mut u32, fpcr: u32, subtract: bool) -> u32 {
        let mut seen = 0;
        for _ in 0..5_000 {
            let left = (next(random) & 0xffff) as u64;
            let right = (next(random) & 0xffff) as u64;
            let addend = (next(random) & 0xffff) as u64;
            let signed = if subtract { left ^ 0x8000 } else { left };
            let expected = evaluate(
                FpArithmetic::FusedMultiplyAdd,
                FpFormat::Half,
                signed,
                right,
                addend,
                fpcr,
            );
            seen |= expected.exceptions;
            let mut cpu = Aarch64CpuState {
                fpcr: u64::from(fpcr),
                fpsr: 1 << 27,
                ..Default::default()
            };
            cpu.set_vector(0, repeat16(addend as u16));
            cpu.set_vector(1, repeat16(left as u16));
            cpu.set_vector(2, repeat16(right as u16));
            execute(&mut cpu, 0x4e42_0c20 | u32::from(subtract) << 23);
            assert_eq!(cpu.vector_lane(0, 16, 0), expected.value);
            assert_eq!(cpu.fpsr, 1 << 27 | u64::from(expected.exceptions));
        }
        seen
    }

    fn scalar_case(random: &mut u32, format: FpFormat, index: u8, subtract: bool, fpcr: u32) {
        let raw = |state: &mut u32| u64::from(next(state)) | u64::from(next(state)) << 32;
        let mask = if format == FpFormat::Single {
            u64::from(u32::MAX)
        } else {
            u64::MAX
        };
        let left = raw(random) & mask;
        let right = raw(random) & mask;
        let addend = raw(random) & mask;
        let signed_left = if subtract {
            left ^ 1_u64 << (format.bits() - 1)
        } else {
            left
        };
        let expected = evaluate(FpArithmetic::FusedMultiplyAdd, format, signed_left, right, addend, fpcr);
        let shift = u32::from(index) * u32::from(format.bits());
        let mut cpu = Aarch64CpuState {
            fpcr: u64::from(fpcr),
            fpsr: 1 << 27,
            ..Default::default()
        };
        cpu.set_vector(0, u128::from(addend) | u128::MAX << format.bits());
        cpu.set_vector(1, u128::from(left));
        cpu.set_vector(2, u128::from(right) << shift);
        let base = if format == FpFormat::Single {
            0x5f80_1000
        } else {
            0x5fc0_1000
        };
        execute(
            &mut cpu,
            base | u32::from(subtract) << 14 | encode_index(format, index) | 2 << 16 | 1 << 5,
        );
        assert_eq!(cpu.vector(0), u128::from(expected.value));
        assert_eq!(cpu.fpsr, 1 << 27 | u64::from(expected.exceptions));
    }

    fn next(state: &mut u32) -> u32 {
        *state ^= *state << 13;
        *state ^= *state >> 17;
        *state ^= *state << 5;
        *state
    }

    fn evaluate(
        operation: FpArithmetic,
        format: FpFormat,
        left: u64,
        right: u64,
        addend: u64,
        fpcr: u32,
    ) -> crate::FpResult {
        Aarch64SoftFloat.evaluate(FpRequest {
            operation,
            format,
            left,
            right,
            addend,
            fpcr,
        })
    }

    fn execute(cpu: &mut Aarch64CpuState, word: u32) {
        let instruction = FusedAccumulator::decode(word).unwrap().unwrap();
        assert_eq!(
            Aarch64FpExecutor::execute(
                cpu,
                &mut Aarch64SoftFloat,
                &Aarch64Ir {
                    word,
                    wide: word >> 31 != 0,
                    instruction
                }
            ),
            Aarch64ExecutionExit::Continue
        );
    }

    fn repeat32(value: u32) -> u128 {
        u128::from(value) * 0x0000_0001_0000_0001_0000_0001_0000_0001
    }
    fn repeat16(value: u16) -> u128 {
        u128::from(value) * 0x0001_0001_0001_0001_0001_0001_0001_0001
    }
}
