use crate::{Aarch64Instruction, FpBinaryOperation};

pub(crate) struct FpReduce;

impl FpReduce {
    pub(crate) fn decode(word: u32) -> Option<Aarch64Instruction> {
        let operation = match word & 0xffff_fc00 {
            0x6e30_f800 => FpBinaryOperation::Maximum,
            0x6eb0_f800 => FpBinaryOperation::Minimum,
            0x6e30_c800 => FpBinaryOperation::MaximumNumber,
            0x6eb0_c800 => FpBinaryOperation::MinimumNumber,
            _ => return None,
        };
        Some(Aarch64Instruction::SimdFpReduce {
            operation,
            source: (word >> 5 & 31) as u8,
            destination: (word & 31) as u8,
        })
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        Aarch64CpuState, Aarch64ExecutionExit, Aarch64FpExecutor, Aarch64SoftFloat, FPSR_INPUT_DENORMAL, FPSR_INVALID,
        FpArithmetic, FpArithmeticPort, FpFormat, FpRequest, Nzcv,
    };

    #[test]
    fn encodings() {
        for (base, operation) in [
            (0x6e30_f800, FpBinaryOperation::Maximum),
            (0x6eb0_f800, FpBinaryOperation::Minimum),
            (0x6e30_c800, FpBinaryOperation::MaximumNumber),
            (0x6eb0_c800, FpBinaryOperation::MinimumNumber),
        ] {
            for encoded in 0_u32..32 * 32 {
                let source = encoded / 32;
                let destination = encoded % 32;
                assert_eq!(
                    FpReduce::decode(base | source << 5 | destination),
                    Some(Aarch64Instruction::SimdFpReduce {
                        operation,
                        source: source as u8,
                        destination: destination as u8
                    })
                );
            }
        }
        for word in [0x2e30_f800, 0x6e30_f400, 0x6e20_f800] {
            assert_eq!(FpReduce::decode(word), None);
        }
    }

    #[test]
    fn ordering() {
        for operation in operations() {
            for lanes in [
                [0x7fc0_0011, 0x3f80_0000, 0x7fc0_0022, 0x4000_0000],
                [0x8000_0000, 0, 0x8000_0000, 0],
                [0x7f80_0001, 1, 0x7fc0_0022, 0x7f80_0000],
                [1, 0x8000_0001, 0x0080_0000, 0x8080_0000],
            ] {
                check(lanes, operation, 0);
                check(lanes, operation, 1 << 25);
                check(lanes, operation, 1 << 24);
            }
        }
    }

    #[test]
    fn random() {
        let mut random = 0xa54f_f53a_u32;
        let mut seen = 0_u32;
        for fpcr in [0, 1 << 24, 1 << 25, 1 << 22, 2 << 22, 3 << 22] {
            for operation in operations() {
                seen |= samples(&mut random, operation, fpcr);
            }
        }
        assert_eq!(
            seen & (FPSR_INVALID | FPSR_INPUT_DENORMAL),
            FPSR_INVALID | FPSR_INPUT_DENORMAL
        );
    }

    fn samples(random: &mut u32, operation: FpBinaryOperation, fpcr: u32) -> u32 {
        let mut seen = 0;
        for _ in 0..5_000 {
            let lanes = [next(random), next(random), next(random), next(random)];
            seen |= check(lanes, operation, fpcr);
        }
        seen
    }

    fn check(lanes: [u32; 4], operation: FpBinaryOperation, fpcr: u32) -> u32 {
        let (expected, exceptions) = reference(lanes, operation, fpcr);
        let mut cpu = Aarch64CpuState {
            pc: 0x4004fc,
            fpcr: u64::from(fpcr),
            fpsr: 1 << 27,
            nzcv: Nzcv::from_bits(0x6000_0000),
            ..Default::default()
        };
        let packed = lanes
            .iter()
            .enumerate()
            .fold(0_u128, |value, (lane, bits)| value | u128::from(*bits) << (lane * 32));
        cpu.set_vector(0, packed);
        let word = base(operation);
        assert_eq!(
            Aarch64FpExecutor::execute_word(&mut cpu, &mut Aarch64SoftFloat, word),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector(0), u128::from(expected));
        assert_eq!(cpu.fpsr, 1 << 27 | u64::from(exceptions));
        assert_eq!(cpu.nzcv.bits(), 0x6000_0000);
        assert_eq!(cpu.pc, 0x400500);
        exceptions
    }

    fn reference(lanes: [u32; 4], operation: FpBinaryOperation, fpcr: u32) -> (u64, u32) {
        let mut value = u64::from(lanes[0]);
        let mut exceptions = 0;
        for lane in lanes.into_iter().skip(1) {
            let result = Aarch64SoftFloat.evaluate(FpRequest {
                operation: FpArithmetic::Binary(operation),
                format: FpFormat::Single,
                left: value,
                right: u64::from(lane),
                addend: 0,
                fpcr,
            });
            value = result.value;
            exceptions |= result.exceptions;
        }
        (value, exceptions)
    }

    fn operations() -> [FpBinaryOperation; 4] {
        [
            FpBinaryOperation::Maximum,
            FpBinaryOperation::Minimum,
            FpBinaryOperation::MaximumNumber,
            FpBinaryOperation::MinimumNumber,
        ]
    }
    fn base(operation: FpBinaryOperation) -> u32 {
        match operation {
            FpBinaryOperation::Maximum => 0x6e30_f800,
            FpBinaryOperation::Minimum => 0x6eb0_f800,
            FpBinaryOperation::MaximumNumber => 0x6e30_c800,
            FpBinaryOperation::MinimumNumber => 0x6eb0_c800,
            _ => unreachable!(),
        }
    }
    fn next(state: &mut u32) -> u32 {
        *state ^= *state << 13;
        *state ^= *state >> 17;
        *state ^= *state << 5;
        *state
    }
}
