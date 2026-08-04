use crate::{Aarch64CpuState, Aarch64DecodeError, Aarch64Instruction, SimdSaturatingLongOperation};

pub(crate) struct SaturatingProduct;

impl SaturatingProduct {
    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        let vector = match word & 0xbf20_fc00 {
            0x0e20_d000 => Some(SimdSaturatingLongOperation::Multiply),
            0x0e20_9000 => Some(SimdSaturatingLongOperation::Accumulate { subtract: false }),
            0x0e20_b000 => Some(SimdSaturatingLongOperation::Accumulate { subtract: true }),
            _ => None,
        };
        if let Some(operation) = vector {
            return Some(Self::instruction(word, operation, None, false));
        }
        let scalar = word & 0x5000_0000 == 0x5000_0000;
        let masked = if scalar { word & 0xdf00_f400 } else { word & 0x9f00_f400 };
        let operation = match masked {
            0x0f00_b000 => SimdSaturatingLongOperation::Multiply,
            0x0f00_3000 => SimdSaturatingLongOperation::Accumulate { subtract: false },
            0x0f00_7000 => SimdSaturatingLongOperation::Accumulate { subtract: true },
            0x5f00_b000 => SimdSaturatingLongOperation::Multiply,
            0x5f00_3000 => SimdSaturatingLongOperation::Accumulate { subtract: false },
            0x5f00_7000 => SimdSaturatingLongOperation::Accumulate { subtract: true },
            _ => return None,
        };
        let size = (word >> 22 & 3) as u8;
        if !matches!(size, 1 | 2) {
            return Some(Err(Aarch64DecodeError::Reserved));
        }
        let bits = 8 << size;
        let element = if bits == 16 {
            (
                (word >> 16 & 15) as u8,
                ((word >> 20 & 1) | (word >> 20 & 2) | (word >> 9 & 4)) as u8,
            )
        } else {
            ((word >> 16 & 31) as u8, ((word >> 21 & 1) | (word >> 10 & 2)) as u8)
        };
        Some(Self::instruction(word, operation, Some(element), scalar))
    }

    fn instruction(
        word: u32,
        operation: SimdSaturatingLongOperation,
        element: Option<(u8, u8)>,
        scalar: bool,
    ) -> Result<Aarch64Instruction, Aarch64DecodeError> {
        let size = (word >> 22 & 3) as u8;
        if !matches!(size, 1 | 2) {
            return Err(Aarch64DecodeError::Reserved);
        }
        Ok(Aarch64Instruction::SimdSaturatingLong {
            operation,
            narrow_bits: 8 << size,
            left: (word >> 5 & 31) as u8,
            right: element.map_or((word >> 16 & 31) as u8, |value| value.0),
            index: element.map(|value| value.1),
            destination: (word & 31) as u8,
            high: !scalar && word >> 30 & 1 != 0,
            scalar,
        })
    }

    pub(crate) fn execute(
        cpu: &Aarch64CpuState,
        operation: SimdSaturatingLongOperation,
        narrow_bits: u8,
        left: u8,
        right: u8,
        index: Option<u8>,
        destination: u8,
        high: bool,
        scalar: bool,
    ) -> (u128, bool) {
        let lanes = if scalar { 1 } else { 64 / narrow_bits };
        let wide_bits = narrow_bits * 2;
        let minimum = -(1_i128 << (wide_bits - 1));
        let maximum = (1_i128 << (wide_bits - 1)) - 1;
        let mut result = 0_u128;
        let mut saturated = false;
        for lane in 0..lanes {
            let source_lane = lane + u8::from(high) * lanes;
            let a = Self::signed(cpu.vector_lane(left, narrow_bits, source_lane), narrow_bits);
            let b = Self::signed(
                cpu.vector_lane(right, narrow_bits, index.unwrap_or(source_lane)),
                narrow_bits,
            );
            let exact_product = 2 * a * b;
            let product = exact_product.clamp(minimum, maximum);
            saturated |= product != exact_product;
            let base = Self::signed(cpu.vector_lane(destination, wide_bits, lane), wide_bits);
            let exact = match operation {
                SimdSaturatingLongOperation::Multiply => product,
                SimdSaturatingLongOperation::Accumulate { subtract: false } => base + product,
                SimdSaturatingLongOperation::Accumulate { subtract: true } => base - product,
            };
            let value = exact.clamp(minimum, maximum);
            saturated |= value != exact;
            result |= (value as u128 & ((1_u128 << wide_bits) - 1)) << (u32::from(lane) * u32::from(wide_bits));
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
        for (base, operation) in [
            (0x0e20_d000, SimdSaturatingLongOperation::Multiply),
            (0x0e20_9000, SimdSaturatingLongOperation::Accumulate { subtract: false }),
            (0x0e20_b000, SimdSaturatingLongOperation::Accumulate { subtract: true }),
        ] {
            vector_family(base, operation);
        }
        for (base, operation) in [
            (0x0f00_b000, SimdSaturatingLongOperation::Multiply),
            (0x0f00_3000, SimdSaturatingLongOperation::Accumulate { subtract: false }),
            (0x0f00_7000, SimdSaturatingLongOperation::Accumulate { subtract: true }),
        ] {
            for high in [false, true] {
                let base = base | u32::from(high) << 30;
                indexed_encodings(base, operation, 16, 16, 8, high);
                indexed_encodings(base, operation, 32, 32, 4, high);
            }
        }
        for (base, operation) in [
            (0x5f00_b000, SimdSaturatingLongOperation::Multiply),
            (0x5f00_3000, SimdSaturatingLongOperation::Accumulate { subtract: false }),
            (0x5f00_7000, SimdSaturatingLongOperation::Accumulate { subtract: true }),
        ] {
            scalar_encodings(base, operation, 16, 16, 8);
            scalar_encodings(base, operation, 32, 32, 4);
        }
        for word in [0x0e20_d000, 0x0ee0_d000, 0x0f00_b000, 0x0fc0_7000] {
            assert_eq!(SaturatingProduct::decode(word), Some(Err(Aarch64DecodeError::Reserved)));
        }
    }

    fn scalar_encodings(base: u32, operation: SimdSaturatingLongOperation, bits: u8, registers: u8, indexes: u8) {
        let shape = if bits == 16 { 1 << 22 } else { 2 << 22 };
        for combined in 0_u32..u32::from(registers) * u32::from(indexes) * 1024 {
            let right = (combined / (u32::from(indexes) * 1024)) as u8;
            let index = (combined / 1024 % u32::from(indexes)) as u8;
            let encoded = combined % 1024;
            let left = encoded / 32;
            let destination = encoded % 32;
            let word = base | shape | u32::from(right) << 16 | encode(bits, index) | left << 5 | destination;
            assert_eq!(
                SaturatingProduct::decode(word),
                Some(Ok(Aarch64Instruction::SimdSaturatingLong {
                    operation,
                    narrow_bits: bits,
                    left: left as u8,
                    right,
                    index: Some(index),
                    destination: destination as u8,
                    high: false,
                    scalar: true
                }))
            );
        }
    }

    fn vector_family(base: u32, operation: SimdSaturatingLongOperation) {
        for configuration in 0_u32..4 {
            let bits = if configuration & 1 == 0 { 16 } else { 32 };
            let high = configuration & 2 != 0;
            vector_encodings(
                base | u32::from(high) << 30 | u32::from(bits == 16) << 22 | u32::from(bits == 32) << 23,
                operation,
                bits,
                high,
            );
        }
    }

    fn vector_encodings(base: u32, operation: SimdSaturatingLongOperation, bits: u8, high: bool) {
        for encoded in 0_u32..32 * 32 * 32 {
            let left = encoded / 1024;
            let right = encoded / 32 % 32;
            let destination = encoded % 32;
            assert_eq!(
                SaturatingProduct::decode(base | right << 16 | left << 5 | destination),
                Some(Ok(Aarch64Instruction::SimdSaturatingLong {
                    operation,
                    narrow_bits: bits,
                    left: left as u8,
                    right: right as u8,
                    index: None,
                    destination: destination as u8,
                    high,
                    scalar: false
                }))
            );
        }
    }

    fn indexed_encodings(
        base: u32,
        operation: SimdSaturatingLongOperation,
        bits: u8,
        registers: u8,
        indexes: u8,
        high: bool,
    ) {
        let shape = if bits == 16 { 1 << 22 } else { 2 << 22 };
        for combined in 0_u32..u32::from(registers) * u32::from(indexes) * 1024 {
            let right = (combined / (u32::from(indexes) * 1024)) as u8;
            let index = (combined / 1024 % u32::from(indexes)) as u8;
            let encoded = combined % 1024;
            let left = encoded / 32;
            let destination = encoded % 32;
            let word = base | shape | u32::from(right) << 16 | encode(bits, index) | left << 5 | destination;
            assert_eq!(
                SaturatingProduct::decode(word),
                Some(Ok(Aarch64Instruction::SimdSaturatingLong {
                    operation,
                    narrow_bits: bits,
                    left: left as u8,
                    right,
                    index: Some(index),
                    destination: destination as u8,
                    high,
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
    fn boundaries_random() {
        let mut cpu = Aarch64CpuState::default();
        cpu.set_vector(0, u128::MAX);
        cpu.set_vector(1, 0x8000_8000_8000_8000);
        cpu.set_vector(2, 0x8000_8000_8000_8000);
        let (value, saturated) = SaturatingProduct::execute(
            &cpu,
            SimdSaturatingLongOperation::Multiply,
            16,
            1,
            2,
            None,
            0,
            false,
            false,
        );
        assert_eq!(value, 0x7fff_ffff_7fff_ffff_7fff_ffff_7fff_ffff);
        assert!(saturated);
        cpu.set_vector(0, 0x7fff_ffff_7fff_ffff_7fff_ffff_7fff_ffff);
        let (value, saturated) = SaturatingProduct::execute(
            &cpu,
            SimdSaturatingLongOperation::Accumulate { subtract: false },
            16,
            1,
            2,
            None,
            0,
            false,
            false,
        );
        assert_eq!(value, 0x7fff_ffff_7fff_ffff_7fff_ffff_7fff_ffff);
        assert!(saturated);

        let mut state = 0x27d4_eb2f_u32;
        for bits in [16, 32] {
            for high in [false, true] {
                check_random(&mut cpu, &mut state, bits, high);
                check_element(&mut cpu, &mut state, bits, high);
            }
        }
        cpu.set_vector(0, random128(&mut state));
        let expected = reference(
            &cpu,
            SimdSaturatingLongOperation::Accumulate { subtract: true },
            16,
            0,
            0,
            Some(0),
            0,
            false,
        );
        assert_eq!(
            SaturatingProduct::execute(
                &cpu,
                SimdSaturatingLongOperation::Accumulate { subtract: true },
                16,
                0,
                0,
                Some(0),
                0,
                false,
                false
            ),
            expected
        );

        let mut scalar = Aarch64CpuState {
            pc: 0xb00,
            nzcv: Nzcv::from_bits(0x3000_0000),
            fpsr: 0x10,
            ..Default::default()
        };
        scalar.set_vector(30, 0x8000_0000);
        scalar.set_vector(31, 0x8000_0000);
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut scalar, &Coordinates, 0x5f9f_b3df),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(scalar.vector(31), i64::MAX as u128);
        assert_eq!(scalar.fpsr, 0x10 | 1 << 27);
        assert_eq!(scalar.pc, 0xb04);
        assert_eq!(scalar.nzcv.bits(), 0x3000_0000);
    }

    fn check_random(cpu: &mut Aarch64CpuState, state: &mut u32, bits: u8, high: bool) {
        for _ in 0..5_000 {
            cpu.set_vector(0, random128(state));
            cpu.set_vector(1, random128(state));
            cpu.set_vector(2, random128(state));
            for operation in [
                SimdSaturatingLongOperation::Multiply,
                SimdSaturatingLongOperation::Accumulate { subtract: false },
                SimdSaturatingLongOperation::Accumulate { subtract: true },
            ] {
                assert_eq!(
                    SaturatingProduct::execute(cpu, operation, bits, 1, 2, None, 0, high, false),
                    reference(cpu, operation, bits, 1, 2, None, 0, high)
                );
            }
        }
    }

    fn check_element(cpu: &mut Aarch64CpuState, state: &mut u32, bits: u8, high: bool) {
        for _ in 0..2_000 {
            cpu.set_vector(0, random128(state));
            cpu.set_vector(1, random128(state));
            cpu.set_vector(2, random128(state));
            let index = (next(state) as u8) % (128 / bits);
            for operation in [
                SimdSaturatingLongOperation::Multiply,
                SimdSaturatingLongOperation::Accumulate { subtract: false },
                SimdSaturatingLongOperation::Accumulate { subtract: true },
            ] {
                assert_eq!(
                    SaturatingProduct::execute(cpu, operation, bits, 1, 2, Some(index), 0, high, false),
                    reference(cpu, operation, bits, 1, 2, Some(index), 0, high)
                );
            }
        }
    }

    fn reference(
        cpu: &Aarch64CpuState,
        operation: SimdSaturatingLongOperation,
        bits: u8,
        left: u8,
        right: u8,
        index: Option<u8>,
        destination: u8,
        high: bool,
    ) -> (u128, bool) {
        let lanes = 64 / bits;
        let wide = bits * 2;
        let min = -(1_i128 << (wide - 1));
        let max = (1_i128 << (wide - 1)) - 1;
        let mut result = 0_u128;
        let mut saturated = false;
        for lane in 0..lanes {
            let source = lane + u8::from(high) * lanes;
            let a = signed(cpu.vector_lane(left, bits, source), bits);
            let b = signed(cpu.vector_lane(right, bits, index.unwrap_or(source)), bits);
            let exact_product = a * b * 2;
            let product = exact_product.clamp(min, max);
            saturated |= product != exact_product;
            let base = signed(cpu.vector_lane(destination, wide, lane), wide);
            let exact = match operation {
                SimdSaturatingLongOperation::Multiply => product,
                SimdSaturatingLongOperation::Accumulate { subtract: false } => base + product,
                SimdSaturatingLongOperation::Accumulate { subtract: true } => base - product,
            };
            let value = exact.clamp(min, max);
            saturated |= value != exact;
            result |= (value as u128 & ((1_u128 << wide) - 1)) << (u32::from(lane) * u32::from(wide));
        }
        (result, saturated)
    }

    fn signed(value: u64, bits: u8) -> i128 {
        i128::from(((value << (64 - bits)) as i64) >> (64 - bits))
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
