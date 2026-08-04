use crate::{Aarch64DecodeError, Aarch64Instruction, FpFormat};

pub(crate) struct FpProduct;

impl FpProduct {
    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        let register = match word & 0xffe0_fc00 {
            0x2e20_dc00 | 0x6e20_dc00 | 0x6e60_dc00 => Some((false, false)),
            0x0e20_dc00 | 0x4e20_dc00 | 0x4e60_dc00 => Some((false, true)),
            0x5e20_dc00 | 0x5e60_dc00 => Some((true, true)),
            _ => None,
        };
        if let Some((scalar, extended)) = register {
            return Some(Self::instruction(word, scalar, extended, None));
        }
        if !matches!(word & 0xdf80_f400, 0x0f80_9000 | 0x4f80_9000 | 0x5f80_9000) {
            return None;
        }
        let scalar = word & 0x5000_0000 == 0x5000_0000;
        let double = word & 1 << 22 != 0;
        let index = if double {
            (word >> 11 & 1) as u8
        } else {
            ((word >> 21 & 1) | (word >> 10 & 2)) as u8
        };
        Some(Self::instruction(word, scalar, word & 1 << 29 != 0, Some(index)))
    }

    fn instruction(
        word: u32,
        scalar: bool,
        extended: bool,
        index: Option<u8>,
    ) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let double = word & 1 << 22 != 0;
        let wide = word & 1 << 30 != 0;
        if double && !wide {
            return Err(Aarch64DecodeError::Reserved);
        }
        Ok(Aarch64Instruction::SimdFpProduct {
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
            extended,
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
        Aarch64CpuState, Aarch64ExecutionExit, Aarch64FpExecutor, Aarch64Ir, Aarch64SoftFloat, FpArithmetic,
        FpArithmeticPort, FpBinaryOperation, FpRequest, Nzcv,
    };

    #[test]
    fn encodings() {
        for (base, format, lanes, indexes, scalar) in [
            (0x0f80_9000, FpFormat::Single, 2, 4, false),
            (0x4f80_9000, FpFormat::Single, 4, 4, false),
            (0x4fc0_9000, FpFormat::Double, 2, 2, false),
            (0x5f80_9000, FpFormat::Single, 1, 4, true),
            (0x5fc0_9000, FpFormat::Double, 1, 2, true),
        ] {
            for extended in [false, true] {
                encoding_shape(
                    base | u32::from(extended) << 29,
                    format,
                    lanes,
                    indexes,
                    scalar,
                    extended,
                );
            }
        }
        for word in [0x0fc0_9000, 0x2fc0_9000] {
            assert_eq!(FpProduct::decode(word), Some(Err(Aarch64DecodeError::Reserved)));
        }
        for word in [0x1e2b_0949, 0x7e22_dc20, 0x7e68_dce6] {
            assert_eq!(FpProduct::decode(word), None);
        }
        for (base, format, lanes, scalar, extended) in [
            (0x2e20_dc00, FpFormat::Single, 2, false, false),
            (0x6e20_dc00, FpFormat::Single, 4, false, false),
            (0x6e60_dc00, FpFormat::Double, 2, false, false),
            (0x0e20_dc00, FpFormat::Single, 2, false, true),
            (0x4e20_dc00, FpFormat::Single, 4, false, true),
            (0x4e60_dc00, FpFormat::Double, 2, false, true),
            (0x5e20_dc00, FpFormat::Single, 1, true, true),
            (0x5e60_dc00, FpFormat::Double, 1, true, true),
        ] {
            register_shape(base, format, lanes, scalar, extended);
        }
    }

    fn encoding_shape(base: u32, format: FpFormat, lanes: u8, indexes: u8, scalar: bool, extended: bool) {
        for index in 0..indexes {
            for encoded in 0_u32..32 * 32 * 32 {
                let left = encoded / 1024;
                let right = encoded / 32 % 32;
                let destination = encoded % 32;
                let word = base | encode_index(format, index) | right << 16 | left << 5 | destination;
                assert_eq!(
                    FpProduct::decode(word),
                    Some(Ok(Aarch64Instruction::SimdFpProduct {
                        format,
                        lanes,
                        extended,
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

    fn register_shape(base: u32, format: FpFormat, lanes: u8, scalar: bool, extended: bool) {
        for encoded in 0_u32..32 * 32 * 32 {
            let left = encoded / 1024;
            let right = encoded / 32 % 32;
            let destination = encoded % 32;
            assert_eq!(
                FpProduct::decode(base | right << 16 | left << 5 | destination),
                Some(Ok(Aarch64Instruction::SimdFpProduct {
                    format,
                    lanes,
                    extended,
                    left: left as u8,
                    right: right as u8,
                    index: None,
                    destination: destination as u8,
                    scalar
                }))
            );
        }
    }

    #[test]
    fn reference() {
        let mut random = 0x9e37_79b9_u32;
        for fpcr in [0, 1 << 24, 1 << 25, 1 << 22, 2 << 22, 3 << 22] {
            for extended in [false, true] {
                samples(&mut random, FpFormat::Single, false, extended, fpcr);
                samples(&mut random, FpFormat::Double, false, extended, fpcr);
                samples(&mut random, FpFormat::Single, true, extended, fpcr);
                samples(&mut random, FpFormat::Double, true, extended, fpcr);
            }
        }
    }

    #[test]
    fn frontier_alias() {
        let mut cpu = Aarch64CpuState {
            pc: 0x4020_bc,
            nzcv: Nzcv::from_bits(0x6000_0000),
            ..Default::default()
        };
        cpu.set_vector(
            31,
            u128::from(0x4000_0000_u32) * 0x0000_0001_0000_0001_0000_0001_0000_0001,
        );
        cpu.set_vector(29, u128::from(0x4040_0000_u32));
        execute(&mut cpu, 0x4f9d_93fe);
        assert_eq!(
            cpu.vector(30),
            u128::from(0x40c0_0000_u32) * 0x0000_0001_0000_0001_0000_0001_0000_0001
        );
        assert_eq!(cpu.pc, 0x4020_c0);
        assert_eq!(cpu.nzcv.bits(), 0x6000_0000);

        cpu.pc = 0;
        cpu.set_vector(0, u128::from(0x4000_0000_u32) | u128::MAX << 32);
        cpu.set_vector(2, u128::from(0x4040_0000_u32) << 96);
        execute(&mut cpu, 0x5fa2_9800);
        assert_eq!(cpu.vector(0), u128::from(0x40c0_0000_u32));
    }

    #[test]
    fn extended_semantics() {
        let mut cpu = Aarch64CpuState {
            pc: 0x4024_38,
            fpsr: 1 << 27,
            ..Default::default()
        };
        cpu.set_vector(7, u128::from(0x7f80_0000_u32) << 32);
        execute(&mut cpu, 0x6fa7_90fe);
        let expected = u128::from(0x4000_0000_u32)
            | u128::from(0x7f80_0000_u32) << 32
            | u128::from(0x4000_0000_u32) << 64
            | u128::from(0x4000_0000_u32) << 96;
        assert_eq!(cpu.vector(30), expected);
        assert_eq!(cpu.fpsr, 1 << 27);
        assert_eq!(cpu.pc, 0x4024_3c);

        cpu.pc = 0;
        cpu.fpcr = 1 << 24;
        cpu.set_vector(0, u128::from(1_u32) | u128::MAX << 32);
        cpu.set_vector(2, u128::from(0xff80_0000_u32) << 96);
        execute(&mut cpu, 0x7fa2_9800);
        assert_eq!(cpu.vector(0), u128::from(0xc000_0000_u32));
        assert_ne!(cpu.fpsr & u64::from(crate::FPSR_INPUT_DENORMAL), 0);

        let normal = Aarch64SoftFloat.evaluate(FpRequest {
            operation: FpArithmetic::Binary(FpBinaryOperation::Multiply),
            format: FpFormat::Single,
            left: 0,
            right: 0x7f80_0000,
            addend: 0,
            fpcr: 0,
        });
        assert_ne!(normal.value, 0x4000_0000);
        assert_ne!(normal.exceptions & crate::FPSR_INVALID, 0);
    }

    #[test]
    fn register_reference() {
        let mut cpu = Aarch64CpuState {
            pc: 0x4024_84,
            fpsr: 1 << 27,
            nzcv: Nzcv::from_bits(0x5000_0000),
            ..Default::default()
        };
        cpu.set_vector(7, u128::from(0_u32));
        cpu.set_vector(
            15,
            u128::from(0xff80_0000_u32) * 0x0000_0001_0000_0001_0000_0001_0000_0001,
        );
        execute(&mut cpu, 0x4e2f_dcff);
        assert_eq!(
            cpu.vector(31),
            u128::from(0xc000_0000_u32) * 0x0000_0001_0000_0001_0000_0001_0000_0001
        );
        assert_eq!(cpu.fpsr, 1 << 27);
        assert_eq!(cpu.nzcv.bits(), 0x5000_0000);

        let mut random = 0xa409_3822_u32;
        for fpcr in [0, 1 << 24, 1 << 25, 1 << 22, 2 << 22, 3 << 22] {
            for extended in [false, true] {
                register_samples(&mut random, FpFormat::Single, false, extended, fpcr);
                register_samples(&mut random, FpFormat::Double, false, extended, fpcr);
            }
            register_samples(&mut random, FpFormat::Single, true, true, fpcr);
            register_samples(&mut random, FpFormat::Double, true, true, fpcr);
        }
    }

    fn register_samples(random: &mut u32, format: FpFormat, scalar: bool, extended: bool, fpcr: u32) {
        for _ in 0..500 {
            register_case(random, format, scalar, extended, fpcr);
        }
    }

    fn register_case(random: &mut u32, format: FpFormat, scalar: bool, extended: bool, fpcr: u32) {
        let raw = |state: &mut u32| u64::from(next(state)) | u64::from(next(state)) << 32;
        let mask = if format == FpFormat::Single {
            u64::from(u32::MAX)
        } else {
            u64::MAX
        };
        let left = raw(random) & mask;
        let right = raw(random) & mask;
        let expected = Aarch64SoftFloat.evaluate(FpRequest {
            operation: FpArithmetic::Binary(if extended {
                FpBinaryOperation::MultiplyExtended
            } else {
                FpBinaryOperation::Multiply
            }),
            format,
            left,
            right,
            addend: 0,
            fpcr,
        });
        let (base, lanes) = match (scalar, format) {
            (true, FpFormat::Single) => (0x5e20_dc00, 1),
            (true, FpFormat::Double) => (0x5e60_dc00, 1),
            (false, FpFormat::Single) => (if extended { 0x4e20_dc00 } else { 0x6e20_dc00 }, 4),
            (false, FpFormat::Double) => (if extended { 0x4e60_dc00 } else { 0x6e60_dc00 }, 2),
            (_, FpFormat::Half) => unreachable!(),
        };
        let repeated_left = repeat(left, format, lanes);
        let repeated_right = repeat(right, format, lanes);
        let mut cpu = Aarch64CpuState {
            fpcr: u64::from(fpcr),
            fpsr: 1 << 27,
            ..Default::default()
        };
        cpu.set_vector(1, repeated_left);
        cpu.set_vector(2, repeated_right);
        execute(&mut cpu, base | 2 << 16 | 1 << 5 | 1);
        assert_eq!(cpu.vector(1), repeat(expected.value, format, lanes));
        assert_eq!(cpu.fpsr, 1 << 27 | u64::from(expected.exceptions));
    }

    fn repeat(value: u64, format: FpFormat, lanes: u8) -> u128 {
        let mut result = 0_u128;
        for lane in 0..lanes {
            result |= u128::from(value) << (u32::from(lane) * u32::from(format.bits()));
        }
        result
    }

    fn samples(random: &mut u32, format: FpFormat, scalar: bool, extended: bool, fpcr: u32) {
        for _ in 0..500 {
            reference_case(random, format, scalar, extended, fpcr);
        }
    }

    fn reference_case(random: &mut u32, format: FpFormat, scalar: bool, extended: bool, fpcr: u32) {
        let raw = |state: &mut u32| u64::from(next(state)) | u64::from(next(state)) << 32;
        let mask = if format == FpFormat::Single {
            u64::from(u32::MAX)
        } else {
            u64::MAX
        };
        let left = raw(random) & mask;
        let right = raw(random) & mask;
        let operation = if extended {
            FpBinaryOperation::MultiplyExtended
        } else {
            FpBinaryOperation::Multiply
        };
        let expected = Aarch64SoftFloat.evaluate(FpRequest {
            operation: FpArithmetic::Binary(operation),
            format,
            left,
            right,
            addend: 0,
            fpcr,
        });
        let index = if format == FpFormat::Single { 3 } else { 1 };
        let base = match (scalar, format) {
            (true, FpFormat::Single) => 0x5f80_9000,
            (true, FpFormat::Double) => 0x5fc0_9000,
            (false, FpFormat::Single) => 0x0f80_9000,
            (false, FpFormat::Double) => 0x4fc0_9000,
            (_, FpFormat::Half) => unreachable!(),
        };
        let mut cpu = Aarch64CpuState {
            fpcr: u64::from(fpcr),
            fpsr: 1 << 27,
            nzcv: Nzcv::from_bits(0x9000_0000),
            ..Default::default()
        };
        let repeated = if format == FpFormat::Single {
            u128::from(left) * 0x0000_0001_0000_0001_0000_0001_0000_0001
        } else {
            u128::from(left) | u128::from(left) << 64
        };
        cpu.set_vector(1, repeated);
        cpu.set_vector(2, u128::from(right) << (u32::from(index) * u32::from(format.bits())));
        execute(
            &mut cpu,
            base | u32::from(extended) << 29 | encode_index(format, index) | 2 << 16 | 1 << 5,
        );
        assert_eq!(cpu.vector_lane(0, format.bits(), 0), expected.value);
        assert_eq!(cpu.fpsr, 1 << 27 | u64::from(expected.exceptions));
        assert_eq!(cpu.nzcv.bits(), 0x9000_0000);
        if scalar || format == FpFormat::Single {
            assert_eq!(
                cpu.vector(0) >> (u32::from(format.bits()) * if scalar { 1 } else { 2 }),
                0
            );
        }
    }

    fn execute(cpu: &mut Aarch64CpuState, word: u32) {
        let instruction = FpProduct::decode(word).unwrap().unwrap();
        assert_eq!(
            Aarch64FpExecutor::execute(
                cpu,
                &mut Aarch64SoftFloat,
                Aarch64Ir {
                    word,
                    wide: word >> 31 != 0,
                    instruction
                }
            ),
            Aarch64ExecutionExit::Continue
        );
    }

    fn encode_index(format: FpFormat, index: u8) -> u32 {
        if format == FpFormat::Double {
            u32::from(index) << 11
        } else {
            u32::from(index & 1) << 21 | u32::from(index >> 1) << 11
        }
    }

    fn next(state: &mut u32) -> u32 {
        *state ^= *state << 13;
        *state ^= *state >> 17;
        *state ^= *state << 5;
        *state
    }
}
