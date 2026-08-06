use crate::{
    Aarch64CpuState, Aarch64DecodeError, Aarch64Instruction, FpArithmetic, FpArithmeticPort, FpBinaryOperation,
    FpFormat, FpRequest,
};

/// Register-register floating-point arithmetic from the FP16 three-same box.
pub(crate) struct Binary;

impl Binary {
    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        let (format, lanes, operation) = if word & 0xbf60_fc00 == 0x0e40_1400 {
            (
                FpFormat::Half,
                if word & 1 << 30 == 0 { 4 } else { 8 },
                if word & 1 << 23 == 0 {
                    FpBinaryOperation::Add
                } else {
                    FpBinaryOperation::Subtract
                },
            )
        } else if word & 0xbf20_fc00 == 0x0e20_d400 {
            let size = word >> 22 & 3;
            let format = if size & 1 == 0 {
                FpFormat::Single
            } else {
                FpFormat::Double
            };
            let wide = word & 1 << 30 != 0;
            if format == FpFormat::Double && !wide {
                return Some(Err(Aarch64DecodeError::Reserved));
            }
            (
                format,
                if format == FpFormat::Double {
                    2
                } else if wide {
                    4
                } else {
                    2
                },
                if size >> 1 == 0 {
                    FpBinaryOperation::Add
                } else {
                    FpBinaryOperation::Subtract
                },
            )
        } else if word & 0xbfa0_fc00 == 0x2e20_fc00 {
            let format = if word & 1 << 22 == 0 {
                FpFormat::Single
            } else {
                FpFormat::Double
            };
            let wide = word & 1 << 30 != 0;
            if format == FpFormat::Double && !wide {
                return Some(Err(Aarch64DecodeError::Reserved));
            }
            (
                format,
                if format == FpFormat::Double {
                    2
                } else if wide {
                    4
                } else {
                    2
                },
                FpBinaryOperation::Divide,
            )
        } else {
            return None;
        };
        Some(Ok(Aarch64Instruction::SimdFpBinary {
            operation,
            format,
            lanes,
            left: (word >> 5 & 31) as u8,
            right: (word >> 16 & 31) as u8,
            destination: (word & 31) as u8,
        }))
    }

    pub(crate) fn execute<P: FpArithmeticPort>(
        cpu: &mut Aarch64CpuState,
        port: &mut P,
        operation: FpBinaryOperation,
        format: FpFormat,
        lanes: u8,
        left: u8,
        right: u8,
        destination: u8,
    ) {
        let mut value = 0_u128;
        for lane in 0..lanes {
            let result = port.evaluate(FpRequest {
                operation: FpArithmetic::Binary(operation),
                format,
                left: cpu.vector_lane(left, format.bits(), lane),
                right: cpu.vector_lane(right, format.bits(), lane),
                addend: 0,
                fpcr: cpu.fpcr as u32,
            });
            cpu.fpsr |= u64::from(result.exceptions);
            value |= u128::from(result.value) << (u32::from(lane) * u32::from(format.bits()));
        }
        cpu.set_vector(destination, value);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        Aarch64CpuState, Aarch64ExecutionExit, Aarch64FpExecutor, Aarch64Ir, Aarch64SoftFloat, FPSR_DIVIDE_BY_ZERO,
        FPSR_INEXACT, FPSR_INVALID, FPSR_OVERFLOW, FPSR_UNDERFLOW, FpArithmetic, FpArithmeticPort, FpRequest, Nzcv,
    };

    #[test]
    fn encodings() {
        for (base, operation, lanes) in [
            (0x0e40_1400, FpBinaryOperation::Add, 4),
            (0x4e40_1400, FpBinaryOperation::Add, 8),
            (0x0ec0_1400, FpBinaryOperation::Subtract, 4),
            (0x4ec0_1400, FpBinaryOperation::Subtract, 8),
        ] {
            for encoded in 0_u32..32 * 32 * 32 {
                let left = encoded / 1024;
                let right = encoded / 32 % 32;
                let destination = encoded % 32;
                assert_eq!(
                    Binary::decode(base | right << 16 | left << 5 | destination),
                    Some(Ok(Aarch64Instruction::SimdFpBinary {
                        operation,
                        format: FpFormat::Half,
                        lanes,
                        left: left as u8,
                        right: right as u8,
                        destination: destination as u8
                    }))
                );
            }
        }
        for word in [0x2e40_1400, 0x6e40_1400, 0x0e40_1000, 0x0e40_1c00] {
            assert_eq!(Binary::decode(word), None);
        }
        for (base, format, lanes, operation) in [
            (0x0e20_d400, FpFormat::Single, 2, FpBinaryOperation::Add),
            (0x4e20_d400, FpFormat::Single, 4, FpBinaryOperation::Add),
            (0x4e60_d400, FpFormat::Double, 2, FpBinaryOperation::Add),
            (0x0ea0_d400, FpFormat::Single, 2, FpBinaryOperation::Subtract),
            (0x4ea0_d400, FpFormat::Single, 4, FpBinaryOperation::Subtract),
            (0x4ee0_d400, FpFormat::Double, 2, FpBinaryOperation::Subtract),
        ] {
            for encoded in 0_u32..32 * 32 * 32 {
                let left = encoded / 1024;
                let right = encoded / 32 % 32;
                let destination = encoded % 32;
                assert_eq!(
                    Binary::decode(base | right << 16 | left << 5 | destination),
                    Some(Ok(Aarch64Instruction::SimdFpBinary {
                        operation,
                        format,
                        lanes,
                        left: left as u8,
                        right: right as u8,
                        destination: destination as u8
                    }))
                );
            }
        }
        for word in [0x0e60_d400, 0x0ee0_d400] {
            assert_eq!(Binary::decode(word), Some(Err(Aarch64DecodeError::Reserved)));
        }
        for (base, format, lanes) in [
            (0x2e20_fc00, FpFormat::Single, 2),
            (0x6e20_fc00, FpFormat::Single, 4),
            (0x6e60_fc00, FpFormat::Double, 2),
        ] {
            for encoded in 0_u32..32 * 32 * 32 {
                let left = encoded / 1024;
                let right = encoded / 32 % 32;
                let destination = encoded % 32;
                assert_eq!(
                    Binary::decode(base | right << 16 | left << 5 | destination),
                    Some(Ok(Aarch64Instruction::SimdFpBinary {
                        operation: FpBinaryOperation::Divide,
                        format,
                        lanes,
                        left: left as u8,
                        right: right as u8,
                        destination: destination as u8
                    }))
                );
            }
        }
        assert_eq!(Binary::decode(0x2e60_fc00), Some(Err(Aarch64DecodeError::Reserved)));
    }

    #[test]
    fn frontier() {
        let mut cpu = Aarch64CpuState {
            pc: 0x400d24,
            nzcv: Nzcv::from_bits(0x9000_0000),
            ..Default::default()
        };
        cpu.set_vector(1, lanes([0x3c00, 0x4000, 0x4200, 0x4400, 0xbc00, 0x7bff, 1, 0]));
        cpu.set_vector(2, lanes([0x3c00, 0x3c00, 0x4000, 0xc000, 0x3c00, 0x3c00, 1, 0x3c00]));
        execute(&mut cpu, 0x4e42_1420);
        assert_eq!(
            cpu.vector(0),
            lanes([0x4000, 0x4200, 0x4500, 0x4000, 0, 0x7bff, 2, 0x3c00])
        );
        assert_eq!(cpu.pc, 0x400d28);
        assert_eq!(cpu.nzcv.bits(), 0x9000_0000);
    }

    #[test]
    fn aliases_and_width() {
        let mut cpu = Aarch64CpuState {
            fpsr: 1 << 27,
            ..Default::default()
        };
        cpu.set_vector(0, lanes([0x4000; 8]));
        cpu.set_vector(2, lanes([0x3c00; 8]));
        execute(&mut cpu, 0x4ec0_1400 | 2 << 16);
        assert_eq!(cpu.vector(0), lanes([0x3c00; 8]));
        cpu.pc = 0;
        cpu.set_vector(0, u128::MAX);
        cpu.set_vector(1, lanes([0x4000; 8]));
        cpu.set_vector(2, lanes([0x3c00; 8]));
        execute(&mut cpu, 0x0e42_1420);
        assert_eq!(cpu.vector(0), lanes([0x4200, 0x4200, 0x4200, 0x4200, 0, 0, 0, 0]));
        assert_eq!(cpu.fpsr, 1 << 27);
    }

    #[test]
    fn single_double_frontier() {
        let mut cpu = Aarch64CpuState {
            pc: 0x4005_5c,
            nzcv: Nzcv::from_bits(0xa000_0000),
            ..Default::default()
        };
        cpu.set_vector(
            21,
            u128::from(0x3ff0_0000_0000_0000_u64) | u128::from(0xbff0_0000_0000_0000_u64) << 64,
        );
        cpu.set_vector(
            23,
            u128::from(0x4000_0000_0000_0000_u64) | u128::from(0x3ff0_0000_0000_0000_u64) << 64,
        );
        execute(&mut cpu, 0x4e77_d6b5);
        assert_eq!(cpu.vector_lane(21, 64, 0), 0x4008_0000_0000_0000);
        assert_eq!(cpu.vector_lane(21, 64, 1), 0);
        assert_eq!(cpu.nzcv.bits(), 0xa000_0000);
        assert_eq!(cpu.pc, 0x4005_60);

        cpu.pc = 0;
        cpu.fpsr = 0;
        cpu.set_vector(1, pack32([0x4040_0000, 0xc000_0000, 0x7f80_0000, 1]));
        cpu.set_vector(2, pack32([0x3f80_0000, 0x3f80_0000, 0x7f80_0000, 1]));
        execute(&mut cpu, 0x4ea2_d420);
        assert_eq!(cpu.vector(0), pack32([0x4000_0000, 0xc040_0000, 0x7fc0_0000, 0]));
        assert_ne!(cpu.fpsr & u64::from(FPSR_INVALID), 0);

        cpu.pc = 0;
        cpu.fpsr = 0;
        cpu.set_vector(1, pack32([0x4080_0000, 0x3f80_0000, 0xc080_0000, 0]));
        cpu.set_vector(2, pack32([0x4000_0000, 0, 0x4000_0000, 0]));
        execute(&mut cpu, 0x6e22_fc20);
        assert_eq!(
            cpu.vector(0),
            pack32([0x4000_0000, 0x7f80_0000, 0xc000_0000, 0x7fc0_0000])
        );
        assert_ne!(cpu.fpsr & u64::from(FPSR_DIVIDE_BY_ZERO | FPSR_INVALID), 0);
    }

    #[test]
    fn ieee_edges() {
        let cases = [
            (0x7c00, 0xfc00, FpBinaryOperation::Add),
            (0x7d01, 0x3c00, FpBinaryOperation::Add),
            (0x7e55, 0x3c00, FpBinaryOperation::Subtract),
            (0x7bff, 0x7bff, FpBinaryOperation::Add),
            (0x0001, 0x0001, FpBinaryOperation::Add),
            (0x3c00, 0x0001, FpBinaryOperation::Add),
        ];
        let mut seen = 0;
        for fpcr in [0, 1 << 19, 1 << 25, 1 << 22, 2 << 22, 3 << 22] {
            for (left, right, operation) in cases {
                seen |= check_edge(fpcr, left, right, operation);
            }
        }
        assert_eq!(
            seen & (FPSR_INVALID | FPSR_OVERFLOW | FPSR_INEXACT),
            FPSR_INVALID | FPSR_OVERFLOW | FPSR_INEXACT
        );
        // Same-format binary addition cannot create an inexact tiny result:
        // cancellation is exact and same-sign addition cannot shrink magnitude.
        assert_eq!(seen & FPSR_UNDERFLOW, 0);
    }

    fn check_edge(fpcr: u32, left: u64, right: u64, operation: FpBinaryOperation) -> u32 {
        let expected = Aarch64SoftFloat.evaluate(FpRequest {
            operation: FpArithmetic::Binary(operation),
            format: FpFormat::Half,
            left,
            right,
            addend: 0,
            fpcr,
        });
        let mut cpu = Aarch64CpuState {
            fpcr: u64::from(fpcr),
            fpsr: 1 << 27,
            ..Default::default()
        };
        cpu.set_vector(1, lanes([left as u16; 8]));
        cpu.set_vector(2, lanes([right as u16; 8]));
        let base = if operation == FpBinaryOperation::Add {
            0x4e40_1400
        } else {
            0x4ec0_1400
        };
        execute(&mut cpu, base | 2 << 16 | 1 << 5);
        assert_eq!(cpu.vector_lane(0, 16, 0), expected.value);
        assert_eq!(cpu.fpsr, 1 << 27 | u64::from(expected.exceptions));
        expected.exceptions
    }

    fn execute(cpu: &mut Aarch64CpuState, word: u32) {
        let instruction = Binary::decode(word)
            .expect("FP binary encoding")
            .expect("allocated encoding");
        assert_eq!(
            Aarch64FpExecutor::execute(
                cpu,
                &mut Aarch64SoftFloat,
                Aarch64Ir {
                    word,
                    wide: false,
                    instruction
                }
            ),
            Aarch64ExecutionExit::Continue
        );
    }

    fn lanes(values: [u16; 8]) -> u128 {
        values
            .into_iter()
            .enumerate()
            .fold(0, |bits, (lane, value)| bits | u128::from(value) << (lane * 16))
    }

    fn pack32(values: [u32; 4]) -> u128 {
        values
            .into_iter()
            .enumerate()
            .fold(0, |bits, (lane, value)| bits | u128::from(value) << (lane * 32))
    }
}
