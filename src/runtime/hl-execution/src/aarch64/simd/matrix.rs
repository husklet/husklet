use crate::{Aarch64CpuState, Aarch64Instruction, SimdMatrixSignedness};

pub(crate) struct MatrixProduct;

impl MatrixProduct {
    pub(crate) fn decode(word: u32) -> Option<Aarch64Instruction> {
        let signedness = match word & 0xffe0_fc00 {
            0x4e80_a400 => SimdMatrixSignedness::Signed,
            0x6e80_a400 => SimdMatrixSignedness::Unsigned,
            0x4e80_ac00 => SimdMatrixSignedness::UnsignedSigned,
            _ => return None,
        };
        Some(Aarch64Instruction::SimdMatrix {
            signedness,
            left: (word >> 5 & 31) as u8,
            right: (word >> 16 & 31) as u8,
            destination: (word & 31) as u8,
        })
    }

    pub(crate) fn execute(
        cpu: &Aarch64CpuState,
        signedness: SimdMatrixSignedness,
        left: u8,
        right: u8,
        destination: u8,
    ) -> u128 {
        let mut result = 0_u128;
        for row in 0_u8..2 {
            for column in 0_u8..2 {
                let lane = row * 2 + column;
                let value = Self::lane(cpu, signedness, left, right, destination, row, column);
                result |= u128::from(value) << (u32::from(lane) * 32);
            }
        }
        result
    }

    fn lane(
        cpu: &Aarch64CpuState,
        signedness: SimdMatrixSignedness,
        left: u8,
        right: u8,
        destination: u8,
        row: u8,
        column: u8,
    ) -> u32 {
        let mut value = cpu.vector_lane(destination, 32, row * 2 + column) as u32;
        for index in 0_u8..8 {
            let a = Self::byte(cpu, left, row * 8 + index, signedness == SimdMatrixSignedness::Signed);
            let b = Self::byte(
                cpu,
                right,
                column * 8 + index,
                signedness != SimdMatrixSignedness::Unsigned,
            );
            value = value.wrapping_add((a * b) as u32);
        }
        value
    }

    fn byte(cpu: &Aarch64CpuState, register: u8, lane: u8, signed: bool) -> i32 {
        let value = cpu.vector_lane(register, 8, lane) as u8;
        if signed {
            i32::from(value as i8)
        } else {
            i32::from(value)
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Aarch64Decoder, Aarch64ExecutionExit, Aarch64Interpreter, Nzcv, PcCoordinatePort};

    struct Coordinates;
    impl PcCoordinatePort for Coordinates {
        fn architectural_pc(&self, pc: u64) -> u64 {
            pc
        }
    }

    #[test]
    fn encodings() {
        for (base, signedness) in [
            (0x4e80_a400, SimdMatrixSignedness::Signed),
            (0x6e80_a400, SimdMatrixSignedness::Unsigned),
            (0x4e80_ac00, SimdMatrixSignedness::UnsignedSigned),
        ] {
            for encoded in 0_u32..32 * 32 * 32 {
                let left = encoded / 1024;
                let right = encoded / 32 % 32;
                let destination = encoded % 32;
                assert_eq!(
                    MatrixProduct::decode(base | right << 16 | left << 5 | destination),
                    Some(Aarch64Instruction::SimdMatrix {
                        signedness,
                        left: left as u8,
                        right: right as u8,
                        destination: destination as u8
                    })
                );
            }
        }
        for word in [0x0e80_a400, 0x4e80_a000, 0x6e80_ac00] {
            assert_eq!(MatrixProduct::decode(word), None);
        }
    }

    #[test]
    fn reference() {
        let mut random = 0xbb67_ae85_u32;
        for signedness in [
            SimdMatrixSignedness::Signed,
            SimdMatrixSignedness::Unsigned,
            SimdMatrixSignedness::UnsignedSigned,
        ] {
            for _ in 0..10_000 {
                reference_case(&mut random, signedness);
            }
        }
    }

    fn reference_case(random: &mut u32, signedness: SimdMatrixSignedness) {
        let mut cpu = Aarch64CpuState {
            pc: 0x400828,
            nzcv: Nzcv::from_bits(0xa000_0000),
            fpsr: 0x91,
            ..Default::default()
        };
        cpu.set_vector(0, random128(random));
        cpu.set_vector(1, random128(random));
        cpu.set_vector(2, random128(random));
        let expected = reference_value(&cpu, signedness, 1, 2, 0);
        let base = match signedness {
            SimdMatrixSignedness::Signed => 0x4e80_a400,
            SimdMatrixSignedness::Unsigned => 0x6e80_a400,
            SimdMatrixSignedness::UnsignedSigned => 0x4e80_ac00,
        };
        let ir = Aarch64Decoder::decode(base | 2 << 16 | 1 << 5).unwrap();
        assert_eq!(
            Aarch64Interpreter::execute(&mut cpu, &Coordinates, ir),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector(0), expected);
        assert_eq!(cpu.nzcv.bits(), 0xa000_0000);
        assert_eq!(cpu.fpsr, 0x91);

        cpu.pc = 0;
        cpu.set_vector(0, random128(random));
        let expected = reference_value(&cpu, signedness, 0, 0, 0);
        let ir = Aarch64Decoder::decode(base).unwrap();
        assert_eq!(
            Aarch64Interpreter::execute(&mut cpu, &Coordinates, ir),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector(0), expected);
    }

    fn reference_value(
        cpu: &Aarch64CpuState,
        signedness: SimdMatrixSignedness,
        left: u8,
        right: u8,
        destination: u8,
    ) -> u128 {
        let mut result = 0_u128;
        for row in 0_u8..2 {
            for column in 0_u8..2 {
                let lane = row * 2 + column;
                let value = reference_lane(cpu, signedness, left, right, destination, row, column);
                result |= u128::from(value) << (u32::from(lane) * 32);
            }
        }
        result
    }

    fn reference_lane(
        cpu: &Aarch64CpuState,
        signedness: SimdMatrixSignedness,
        left: u8,
        right: u8,
        destination: u8,
        row: u8,
        column: u8,
    ) -> u32 {
        let mut value = cpu.vector_lane(destination, 32, row * 2 + column) as u32;
        for index in 0_u8..8 {
            let a = reference_byte(cpu, left, row * 8 + index, signedness == SimdMatrixSignedness::Signed);
            let b = reference_byte(
                cpu,
                right,
                column * 8 + index,
                signedness != SimdMatrixSignedness::Unsigned,
            );
            value = value.wrapping_add((a * b) as u32);
        }
        value
    }

    fn reference_byte(cpu: &Aarch64CpuState, register: u8, lane: u8, signed: bool) -> i32 {
        let value = cpu.vector_lane(register, 8, lane) as u8;
        if signed {
            i32::from(value as i8)
        } else {
            i32::from(value)
        }
    }
    fn next(state: &mut u32) -> u32 {
        *state ^= *state << 13;
        *state ^= *state >> 17;
        *state ^= *state << 5;
        *state
    }
    fn random128(state: &mut u32) -> u128 {
        (0..4).fold(0, |value, lane| value | u128::from(next(state)) << (lane * 32))
    }
}
