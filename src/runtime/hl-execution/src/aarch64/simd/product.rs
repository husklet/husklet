use crate::{Aarch64DecodeError, Aarch64Instruction, SimdLaneOperation};

pub(crate) struct IntegerProduct;

impl IntegerProduct {
    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        let operation = match word & 0x9f00_f400 {
            0x0f00_8000 => SimdLaneOperation::Multiply,
            0x0f00_0000 => SimdLaneOperation::MultiplyAccumulate { subtract: false },
            0x0f00_4000 => SimdLaneOperation::MultiplyAccumulate { subtract: true },
            _ => return None,
        };
        let size = (word >> 22 & 3) as u8;
        if !matches!(size, 1 | 2) {
            return Some(Err(Aarch64DecodeError::Reserved));
        }
        let lane_bits = 8 << size;
        let (right, index) = if lane_bits == 16 {
            (
                (word >> 16 & 15) as u8,
                ((word >> 20 & 1) | (word >> 20 & 2) | (word >> 9 & 4)) as u8,
            )
        } else {
            ((word >> 16 & 31) as u8, ((word >> 21 & 1) | (word >> 10 & 2)) as u8)
        };
        Some(Ok(Aarch64Instruction::SimdElementProduct {
            operation,
            lane_bits,
            left: (word >> 5 & 31) as u8,
            right,
            index,
            destination: (word & 31) as u8,
            wide: word >> 30 & 1 != 0,
        }))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        Aarch64CpuState, Aarch64Decoder, Aarch64ExecutionExit, Aarch64Interpreter, Aarch64Ir, Nzcv, PcCoordinatePort,
    };

    struct Coordinates;
    impl PcCoordinatePort for Coordinates {
        fn architectural_pc(&self, pc: u64) -> u64 {
            pc
        }
    }

    #[test]
    fn encodings() {
        for (base, operation) in [
            (0x0e20_9c00, SimdLaneOperation::Multiply),
            (0x0e20_9400, SimdLaneOperation::MultiplyAccumulate { subtract: false }),
            (0x2e20_9400, SimdLaneOperation::MultiplyAccumulate { subtract: true }),
        ] {
            for (shape, lane_bits, wide) in [
                (0, 8, false),
                (1 << 30, 8, true),
                (1 << 22, 16, false),
                (1 << 30 | 1 << 22, 16, true),
                (2 << 22, 32, false),
                (1 << 30 | 2 << 22, 32, true),
            ] {
                vector_encodings(base | shape, operation, lane_bits, wide);
            }
        }
        for (base, operation) in [
            (0x0f00_8000, SimdLaneOperation::Multiply),
            (0x2f00_0000, SimdLaneOperation::MultiplyAccumulate { subtract: false }),
            (0x2f00_4000, SimdLaneOperation::MultiplyAccumulate { subtract: true }),
        ] {
            for wide in [false, true] {
                indexed_encodings(base, operation, 16, wide, 16, 8);
            }
            for wide in [false, true] {
                indexed_encodings(base, operation, 32, wide, 32, 4);
            }
        }
        for word in [0x0f00_8000, 0x0fc0_8000, 0x2f00_0000, 0x2fc0_4000] {
            assert_eq!(IntegerProduct::decode(word), Some(Err(Aarch64DecodeError::Reserved)));
        }
    }

    fn vector_encodings(base: u32, operation: SimdLaneOperation, lane_bits: u8, wide: bool) {
        for encoded in 0_u32..32 * 32 * 32 {
            let left = encoded / 1024;
            let right = encoded / 32 % 32;
            let destination = encoded % 32;
            let word = base | right << 16 | left << 5 | destination;
            assert_eq!(
                Aarch64Decoder::decode(word),
                Ok(Aarch64Ir {
                    word,
                    wide: false,
                    instruction: Aarch64Instruction::SimdLane {
                        operation,
                        lane_bits,
                        left: left as u8,
                        right: right as u8,
                        destination: destination as u8,
                        wide
                    }
                })
            );
        }
    }

    fn indexed_encodings(
        base: u32,
        operation: SimdLaneOperation,
        lane_bits: u8,
        wide: bool,
        registers: u8,
        indexes: u8,
    ) {
        let shape = u32::from(wide) << 30 | if lane_bits == 16 { 1 << 22 } else { 2 << 22 };
        for combined in 0_u32..u32::from(registers) * u32::from(indexes) * 1024 {
            let right = (combined / (u32::from(indexes) * 1024)) as u8;
            let index = (combined / 1024 % u32::from(indexes)) as u8;
            let encoded = combined % 1024;
            let left = encoded / 32;
            let destination = encoded % 32;
            let index_bits = encode(lane_bits, index);
            let word = base | shape | u32::from(right) << 16 | index_bits | left << 5 | destination;
            assert_eq!(
                IntegerProduct::decode(word),
                Some(Ok(Aarch64Instruction::SimdElementProduct {
                    operation,
                    lane_bits,
                    left: left as u8,
                    right,
                    index,
                    destination: destination as u8,
                    wide
                }))
            );
        }
    }

    fn encode(lane_bits: u8, index: u8) -> u32 {
        if lane_bits == 16 {
            u32::from(index & 1) << 20 | u32::from(index & 2) << 20 | u32::from(index & 4) << 9
        } else {
            u32::from(index & 1) << 21 | u32::from(index & 2) << 10
        }
    }

    #[test]
    fn lanes_aliases() {
        let mut cpu = Aarch64CpuState {
            pc: 0x900,
            nzcv: Nzcv::from_bits(0xa000_0000),
            fpsr: 0x123,
            ..Default::default()
        };
        cpu.set_vector(30, 0x1000_0008_1000_0007_1000_0006_1000_0005);
        cpu.set_vector(31, 0x0008_0007_0006_0005_0004_0003_0002_0001);
        cpu.set_vector(14, 1);
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4f4e_83fe),
            Aarch64ExecutionExit::Continue
        );
        for lane in 0..8 {
            assert_eq!(cpu.vector_lane(30, 16, lane), u64::from(lane + 1));
        }
        assert_eq!(cpu.pc, 0x904);
        assert_eq!(cpu.nzcv.bits(), 0xa000_0000);
        assert_eq!(cpu.fpsr, 0x123);

        cpu.set_vector(0, 0xffff_ffff_ffff_ffff_ffff_ffff_ffff_ffff);
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x0f40_8000),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector(0), 0x0001_0001_0001_0001); // Four wrapping 16-bit products; upper half clears.
    }

    #[test]
    fn random_reference() {
        let mut state = 0x9e37_79b9_u32;
        for lane_bits in [8, 16, 32] {
            for wide in [false, true] {
                check_random(lane_bits, wide, &mut state);
            }
        }
    }

    fn check_random(lane_bits: u8, wide: bool, state: &mut u32) {
        let mut cpu = Aarch64CpuState::default();
        for _ in 0..2_000 {
            cpu.set_vector(0, random128(state));
            cpu.set_vector(1, random128(state));
            cpu.set_vector(2, random128(state));
            for operation in [
                SimdLaneOperation::Multiply,
                SimdLaneOperation::MultiplyAccumulate { subtract: false },
                SimdLaneOperation::MultiplyAccumulate { subtract: true },
            ] {
                let expected = reference(&cpu, operation, lane_bits, 1, 2, 0, wide);
                assert_eq!(
                    crate::aarch64_simd_lane_interpreter::execute(&cpu, operation, lane_bits, 1, 2, 0, wide),
                    expected
                );
            }
        }
    }

    fn reference(
        cpu: &Aarch64CpuState,
        operation: SimdLaneOperation,
        bits: u8,
        left: u8,
        right: u8,
        destination: u8,
        wide: bool,
    ) -> u128 {
        let lanes = if wide { 128 } else { 64 } / bits;
        let mask = (1_u64 << bits) - 1;
        let mut result = 0_u128;
        for lane in 0..lanes {
            let product = cpu
                .vector_lane(left, bits, lane)
                .wrapping_mul(cpu.vector_lane(right, bits, lane))
                & mask;
            let base = cpu.vector_lane(destination, bits, lane);
            let value = match operation {
                SimdLaneOperation::Multiply => product,
                SimdLaneOperation::MultiplyAccumulate { subtract: false } => base.wrapping_add(product) & mask,
                SimdLaneOperation::MultiplyAccumulate { subtract: true } => base.wrapping_sub(product) & mask,
                _ => unreachable!(),
            };
            result |= u128::from(value) << (u32::from(lane) * u32::from(bits));
        }
        result
    }

    fn random128(state: &mut u32) -> u128 {
        (0..4).fold(0, |value, lane| {
            *state ^= *state << 13;
            *state ^= *state >> 17;
            *state ^= *state << 5;
            value | u128::from(*state) << (lane * 32)
        })
    }
}
