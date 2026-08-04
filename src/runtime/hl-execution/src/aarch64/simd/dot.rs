use crate::{Aarch64CpuState, Aarch64Instruction};

pub(crate) struct DotProduct;

impl DotProduct {
    pub(crate) fn decode(word: u32) -> Option<Aarch64Instruction> {
        let index = if word & 0x9f00_f400 == 0x0f00_e000 {
            Some(((word >> 21 & 1) | (word >> 10 & 2)) as u8)
        } else if word & 0x9f20_fc00 == 0x0e00_9400 {
            None
        } else {
            return None;
        };
        Some(Aarch64Instruction::SimdDot {
            signed: word & 1 << 29 == 0,
            left: (word >> 5 & 31) as u8,
            right: (word >> 16 & 31) as u8,
            index,
            destination: (word & 31) as u8,
            wide: word & 1 << 30 != 0,
        })
    }

    pub(crate) fn execute(
        cpu: &Aarch64CpuState,
        signed: bool,
        left: u8,
        right: u8,
        index: Option<u8>,
        destination: u8,
        wide: bool,
    ) -> u128 {
        let lanes = if wide { 4_u8 } else { 2_u8 };
        let mut result = 0_u128;
        for lane in 0..lanes {
            let right_group = index.unwrap_or(lane);
            let mut value = cpu.vector_lane(destination, 32, lane) as u32;
            for byte in 0..4 {
                let a = Self::byte(cpu, left, lane * 4 + byte, signed);
                let b = Self::byte(cpu, right, right_group * 4 + byte, signed);
                value = value.wrapping_add((a * b) as u32);
            }
            result |= u128::from(value) << (lane * 32);
        }
        result
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
        for signed in [false, true] {
            for wide in [false, true] {
                vector_encodings(signed, wide);
                indexed_encodings(signed, wide);
            }
        }
        assert_eq!(DotProduct::decode(0x0e00_9000), None);
    }

    fn vector_encodings(signed: bool, wide: bool) {
        let base = 0x0e00_9400 | u32::from(!signed) << 29 | u32::from(wide) << 30;
        for encoded in 0_u32..32 * 32 * 32 {
            let left = encoded / 1024;
            let right = encoded / 32 % 32;
            let destination = encoded % 32;
            let word = base | right << 16 | left << 5 | destination;
            assert_eq!(
                DotProduct::decode(word),
                Some(Aarch64Instruction::SimdDot {
                    signed,
                    left: left as u8,
                    right: right as u8,
                    index: None,
                    destination: destination as u8,
                    wide
                })
            );
        }
    }

    fn indexed_encodings(signed: bool, wide: bool) {
        let base = 0x0f00_e000 | u32::from(!signed) << 29 | u32::from(wide) << 30;
        for index in 0..4 {
            for encoded in 0_u32..32 * 32 * 32 {
                let left = encoded / 1024;
                let right = encoded / 32 % 32;
                let destination = encoded % 32;
                let word = base | encode_index(index) | right << 16 | left << 5 | destination;
                assert_eq!(
                    DotProduct::decode(word),
                    Some(Aarch64Instruction::SimdDot {
                        signed,
                        left: left as u8,
                        right: right as u8,
                        index: Some(index),
                        destination: destination as u8,
                        wide
                    })
                );
            }
        }
    }

    #[test]
    fn reference() {
        let mut random = 0x3c6e_f372_u32;
        for signed in [false, true] {
            for wide in [false, true] {
                samples(&mut random, signed, wide, None);
                indexed_samples(&mut random, signed, wide);
            }
        }
    }

    fn indexed_samples(random: &mut u32, signed: bool, wide: bool) {
        for index in 0..4 {
            samples(random, signed, wide, Some(index));
        }
    }

    #[test]
    fn aliases() {
        for (word, signed, wide, index) in [
            (0x0e00_9400, true, false, None),
            (0x4e00_9400, true, true, None),
            (0x2f00_e000, false, false, Some(0)),
            (0x6f20_e800, false, true, Some(3)),
        ] {
            let mut cpu = Aarch64CpuState {
                pc: 0x400a10,
                nzcv: Nzcv::from_bits(0x6000_0000),
                ..Default::default()
            };
            cpu.set_vector(0, 0xffff_fffe_8080_8080_7fff_ffff_0102_0304);
            let expected = reference_value(&cpu, signed, 0, 0, index, 0, wide);
            let ir = Aarch64Decoder::decode(word).unwrap();
            assert_eq!(
                Aarch64Interpreter::execute(&mut cpu, &Coordinates, ir),
                Aarch64ExecutionExit::Continue
            );
            assert_eq!(cpu.vector(0), expected);
            assert_eq!(cpu.pc, 0x400a14);
            if !wide {
                assert_eq!(cpu.vector(0) >> 64, 0);
            }
        }
    }

    fn samples(random: &mut u32, signed: bool, wide: bool, index: Option<u8>) {
        for _ in 0..2_000 {
            let mut cpu = Aarch64CpuState {
                pc: 0x400a10,
                nzcv: Nzcv::from_bits(0x9000_0000),
                fpsr: 0x81,
                ..Default::default()
            };
            cpu.set_vector(0, random128(random));
            cpu.set_vector(1, random128(random));
            cpu.set_vector(2, random128(random));
            let expected = reference_value(&cpu, signed, 1, 2, index, 0, wide);
            let word = if let Some(index) = index {
                0x0f00_e000 | encode_index(index)
            } else {
                0x0e00_9400
            } | u32::from(!signed) << 29
                | u32::from(wide) << 30
                | 2 << 16
                | 1 << 5;
            let ir = Aarch64Decoder::decode(word).unwrap();
            assert_eq!(
                Aarch64Interpreter::execute(&mut cpu, &Coordinates, ir),
                Aarch64ExecutionExit::Continue
            );
            assert_eq!(cpu.vector(0), expected);
            assert_eq!(cpu.nzcv.bits(), 0x9000_0000);
            assert_eq!(cpu.fpsr, 0x81);
        }
    }

    fn reference_value(
        cpu: &Aarch64CpuState,
        signed: bool,
        left: u8,
        right: u8,
        index: Option<u8>,
        destination: u8,
        wide: bool,
    ) -> u128 {
        let lanes = if wide { 4_u8 } else { 2_u8 };
        let mut result = 0_u128;
        for lane in 0..lanes {
            let mut value = cpu.vector_lane(destination, 32, lane) as u32;
            for byte in 0..4 {
                let a = reference_byte(cpu, left, lane * 4 + byte, signed);
                let b = reference_byte(cpu, right, index.unwrap_or(lane) * 4 + byte, signed);
                value = value.wrapping_add((a * b) as u32);
            }
            result |= u128::from(value) << (lane * 32);
        }
        result
    }

    fn reference_byte(cpu: &Aarch64CpuState, register: u8, lane: u8, signed: bool) -> i32 {
        let value = cpu.vector_lane(register, 8, lane) as u8;
        if signed {
            i32::from(value as i8)
        } else {
            i32::from(value)
        }
    }

    fn encode_index(index: u8) -> u32 {
        u32::from(index & 1) << 21 | u32::from(index >> 1) << 11
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
