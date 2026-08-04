use crate::{Aarch64CpuState, Aarch64DecodeError, Aarch64Instruction};

pub(crate) struct HighProduct;

impl HighProduct {
    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        let vector = match word & 0xbf20_fc00 {
            0x0e20_b400 => Some(false),
            0x2e20_b400 => Some(true),
            _ => None,
        };
        if let Some(rounding) = vector {
            return Some(Self::instruction(word, rounding, None, false));
        }
        let scalar = word & 0x5000_0000 == 0x5000_0000;
        let masked = if scalar { word & 0xdf00_f400 } else { word & 0x9f00_f400 };
        let rounding = match masked {
            0x0f00_c000 | 0x5f00_c000 => false,
            0x0f00_d000 | 0x5f00_d000 => true,
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
        Some(Self::instruction(word, rounding, Some((right, index)), scalar))
    }

    fn instruction(
        word: u32,
        rounding: bool,
        element: Option<(u8, u8)>,
        scalar: bool,
    ) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let size = (word >> 22 & 3) as u8;
        if !matches!(size, 1 | 2) {
            return Err(Aarch64DecodeError::Reserved);
        }
        let lane_bits = 8 << size;
        Ok(Aarch64Instruction::SimdHighProduct {
            rounding,
            lane_bits,
            left: (word >> 5 & 31) as u8,
            right: element.map_or((word >> 16 & 31) as u8, |value| value.0),
            index: element.map(|value| value.1),
            destination: (word & 31) as u8,
            wide: word >> 30 & 1 != 0,
            scalar,
        })
    }

    pub(crate) fn execute(
        cpu: &Aarch64CpuState,
        rounding: bool,
        lane_bits: u8,
        left: u8,
        right: u8,
        index: Option<u8>,
        wide: bool,
        scalar: bool,
    ) -> (u128, bool) {
        let lanes = if scalar {
            1
        } else {
            (if wide { 128 } else { 64 }) / lane_bits
        };
        let minimum = -(1_i128 << (lane_bits - 1));
        let maximum = (1_i128 << (lane_bits - 1)) - 1;
        let mut result = 0_u128;
        let mut saturated = false;
        for lane in 0..lanes {
            let a = Self::signed(cpu.vector_lane(left, lane_bits, lane), lane_bits);
            let b = Self::signed(cpu.vector_lane(right, lane_bits, index.unwrap_or(lane)), lane_bits);
            let doubled = 2 * a * b + if rounding { 1_i128 << (lane_bits - 1) } else { 0 };
            let value = (doubled >> lane_bits).clamp(minimum, maximum);
            saturated |= value != doubled >> lane_bits;
            result |= (value as u128 & ((1_u128 << lane_bits) - 1)) << (u32::from(lane) * u32::from(lane_bits));
        }
        (result, saturated)
    }

    fn signed(value: u64, bits: u8) -> i128 {
        i128::from(((value << (64 - bits)) as i64) >> (64 - bits))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Aarch64ExecutionExit, Aarch64Interpreter, Nzcv, PcCoordinatePort};

    struct Coordinates;
    impl PcCoordinatePort for Coordinates {
        fn architectural_pc(&self, pc: u64) -> u64 {
            pc
        }
    }

    #[test]
    fn encodings() {
        for (base, rounding) in [(0x0e20_b400, false), (0x2e20_b400, true)] {
            for (shape, bits, wide) in [
                (1 << 22, 16, false),
                (1 << 30 | 1 << 22, 16, true),
                (2 << 22, 32, false),
                (1 << 30 | 2 << 22, 32, true),
            ] {
                vector_encodings(base | shape, rounding, bits, wide);
            }
        }
        for (base, rounding) in [(0x0f00_c000, false), (0x0f00_d000, true)] {
            for wide in [false, true] {
                indexed_encodings(base, rounding, 16, wide, 16, 8);
            }
            for wide in [false, true] {
                indexed_encodings(base, rounding, 32, wide, 32, 4);
            }
        }
        for (base, rounding) in [(0x5f00_c000, false), (0x5f00_d000, true)] {
            scalar_encodings(base, rounding, 16, 16, 8);
            scalar_encodings(base, rounding, 32, 32, 4);
        }
        for word in [0x0e20_b400, 0x0ee0_b400, 0x0f00_c000, 0x0fc0_d000] {
            assert_eq!(HighProduct::decode(word), Some(Err(Aarch64DecodeError::Reserved)));
        }
    }

    fn scalar_encodings(base: u32, rounding: bool, lane_bits: u8, registers: u8, indexes: u8) {
        let shape = if lane_bits == 16 { 1 << 22 } else { 2 << 22 };
        for combined in 0_u32..u32::from(registers) * u32::from(indexes) * 1024 {
            let right = (combined / (u32::from(indexes) * 1024)) as u8;
            let index = (combined / 1024 % u32::from(indexes)) as u8;
            let encoded = combined % 1024;
            let left = encoded / 32;
            let destination = encoded % 32;
            let word = base | shape | u32::from(right) << 16 | encode(lane_bits, index) | left << 5 | destination;
            assert_eq!(
                HighProduct::decode(word),
                Some(Ok(Aarch64Instruction::SimdHighProduct {
                    rounding,
                    lane_bits,
                    left: left as u8,
                    right,
                    index: Some(index),
                    destination: destination as u8,
                    wide: true,
                    scalar: true
                }))
            );
        }
    }

    fn vector_encodings(base: u32, rounding: bool, lane_bits: u8, wide: bool) {
        for encoded in 0_u32..32 * 32 * 32 {
            let left = encoded / 1024;
            let right = encoded / 32 % 32;
            let destination = encoded % 32;
            assert_eq!(
                HighProduct::decode(base | right << 16 | left << 5 | destination),
                Some(Ok(Aarch64Instruction::SimdHighProduct {
                    rounding,
                    lane_bits,
                    left: left as u8,
                    right: right as u8,
                    index: None,
                    destination: destination as u8,
                    wide,
                    scalar: false
                }))
            );
        }
    }

    fn indexed_encodings(base: u32, rounding: bool, lane_bits: u8, wide: bool, registers: u8, indexes: u8) {
        let shape = u32::from(wide) << 30 | if lane_bits == 16 { 1 << 22 } else { 2 << 22 };
        for combined in 0_u32..u32::from(registers) * u32::from(indexes) * 1024 {
            let right = (combined / (u32::from(indexes) * 1024)) as u8;
            let index = (combined / 1024 % u32::from(indexes)) as u8;
            let encoded = combined % 1024;
            let left = encoded / 32;
            let destination = encoded % 32;
            let word = base | shape | u32::from(right) << 16 | encode(lane_bits, index) | left << 5 | destination;
            assert_eq!(
                HighProduct::decode(word),
                Some(Ok(Aarch64Instruction::SimdHighProduct {
                    rounding,
                    lane_bits,
                    left: left as u8,
                    right,
                    index: Some(index),
                    destination: destination as u8,
                    wide,
                    scalar: false
                }))
            );
        }
    }

    fn encode(bits: u8, index: u8) -> u32 {
        if bits == 16 {
            u32::from(index & 1) << 20 | u32::from(index & 2) << 20 | u32::from(index & 4) << 9
        } else {
            u32::from(index & 1) << 21 | u32::from(index & 2) << 10
        }
    }

    #[test]
    fn boundaries_aliases() {
        let mut cpu = Aarch64CpuState {
            pc: 0xa00,
            nzcv: Nzcv::from_bits(0x6000_0000),
            fpsr: 0x10,
            ..Default::default()
        };
        cpu.set_vector(0, 0x8000_8000_8000_8000);
        cpu.set_vector(1, 0x8000_8000_8000_8000);
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x0e61_b400),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector(0), 0x7fff_7fff_7fff_7fff);
        assert_eq!(cpu.fpsr, 0x10 | 1 << 27);
        assert_eq!(cpu.pc, 0xa04);
        assert_eq!(cpu.nzcv.bits(), 0x6000_0000);

        cpu.fpsr = 0;
        cpu.set_vector(0, 0x0001_0001_0001_0001);
        cpu.set_vector(1, 0x4000_4000_4000_4000);
        Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x2e61_b400);
        assert_eq!(cpu.vector(0), 0x0001_0001_0001_0001);
        assert_eq!(cpu.fpsr, 0);
        cpu.set_vector(15, 0x8000);
        cpu.set_vector(31, 0x8000_8000_8000_8000_8000_8000_8000_8000);
        Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4f4f_c3e0);
        assert_eq!(cpu.vector(0), 0x7fff_7fff_7fff_7fff_7fff_7fff_7fff_7fff);

        cpu.pc = 0xa20;
        cpu.fpsr = 0x10;
        cpu.set_vector(29, 0x8000);
        cpu.set_vector(6, 0x8000_8000_8000_8000_8000_8000_8000_8000);
        assert_eq!(
            HighProduct::execute(&cpu, false, 16, 29, 6, Some(7), true, true),
            (0x7fff, true)
        );
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x5f76_cbbf),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector(31), 0x7fff);
        assert_eq!(cpu.fpsr, 0x10 | 1 << 27);
    }

    #[test]
    fn random_reference() {
        let mut state = 0x85eb_ca6b_u32;
        for bits in [16, 32] {
            for wide in [false, true] {
                check_random(bits, wide, &mut state);
            }
        }
        for bits in [16, 32] {
            check_scalar(bits, &mut state);
        }
    }

    fn check_scalar(bits: u8, state: &mut u32) {
        let mut cpu = Aarch64CpuState::default();
        let indexes = 128 / bits;
        for _ in 0..5_000 {
            cpu.set_vector(1, random128(state));
            cpu.set_vector(2, random128(state));
            let index = (*state as u8) % indexes;
            for rounding in [false, true] {
                let a = signed(cpu.vector_lane(1, bits, 0), bits);
                let b = signed(cpu.vector_lane(2, bits, index), bits);
                let exact = a * b * 2 + i128::from(rounding) * (1_i128 << (bits - 1));
                let shifted = exact >> bits;
                let value = shifted.clamp(-(1_i128 << (bits - 1)), (1_i128 << (bits - 1)) - 1);
                assert_eq!(
                    HighProduct::execute(&cpu, rounding, bits, 1, 2, Some(index), true, true),
                    (value as u128 & ((1_u128 << bits) - 1), value != shifted)
                );
            }
        }
    }

    fn check_random(bits: u8, wide: bool, state: &mut u32) {
        let mut cpu = Aarch64CpuState::default();
        for _ in 0..5_000 {
            cpu.set_vector(1, random128(state));
            cpu.set_vector(2, random128(state));
            for rounding in [false, true] {
                let expected = reference(&cpu, rounding, bits, wide);
                assert_eq!(
                    HighProduct::execute(&cpu, rounding, bits, 1, 2, None, wide, false),
                    expected
                );
            }
        }
    }

    fn reference(cpu: &Aarch64CpuState, rounding: bool, bits: u8, wide: bool) -> (u128, bool) {
        let lanes = if wide { 128 } else { 64 } / bits;
        let mut result = 0_u128;
        let mut saturated = false;
        for lane in 0..lanes {
            let a = signed(cpu.vector_lane(1, bits, lane), bits);
            let b = signed(cpu.vector_lane(2, bits, lane), bits);
            let exact = a * b * 2 + i128::from(rounding) * (1_i128 << (bits - 1));
            let shifted = exact >> bits;
            let value = shifted.clamp(-(1_i128 << (bits - 1)), (1_i128 << (bits - 1)) - 1);
            saturated |= value != shifted;
            result |= (value as u128 & ((1_u128 << bits) - 1)) << (u32::from(lane) * u32::from(bits));
        }
        (result, saturated)
    }

    fn signed(value: u64, bits: u8) -> i128 {
        i128::from(((value << (64 - bits)) as i64) >> (64 - bits))
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
