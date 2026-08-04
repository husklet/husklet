use crate::{Aarch64CpuState, Aarch64DecodeError, Aarch64Instruction, SimdWideOperation};

pub(crate) struct LongProduct;

impl LongProduct {
    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        let operation = match word & 0x9f00_f400 {
            0x0f00_a000 => SimdWideOperation::MultiplyLong,
            0x0f00_2000 => SimdWideOperation::MultiplyAccumulateLong { subtract: false },
            0x0f00_6000 => SimdWideOperation::MultiplyAccumulateLong { subtract: true },
            _ => return None,
        };
        let size = (word >> 22 & 3) as u8;
        if !matches!(size, 1 | 2) {
            return Some(Err(Aarch64DecodeError::Reserved));
        }
        let narrow_bits = 8 << size;
        let (right, index) = if narrow_bits == 16 {
            (
                (word >> 16 & 15) as u8,
                ((word >> 20 & 1) | (word >> 20 & 2) | (word >> 9 & 4)) as u8,
            )
        } else {
            ((word >> 16 & 31) as u8, ((word >> 21 & 1) | (word >> 10 & 2)) as u8)
        };
        Some(Ok(Aarch64Instruction::SimdLongElement {
            operation,
            signed: word & 1 << 29 == 0,
            narrow_bits,
            left: (word >> 5 & 31) as u8,
            right,
            index,
            destination: (word & 31) as u8,
            high: word & 1 << 30 != 0,
        }))
    }

    pub(crate) fn execute(
        cpu: &Aarch64CpuState,
        operation: SimdWideOperation,
        signed: bool,
        narrow_bits: u8,
        left: u8,
        right: u8,
        index: u8,
        destination: u8,
        high: bool,
    ) -> u128 {
        let lanes = 64 / narrow_bits;
        let wide_bits = narrow_bits * 2;
        let mask = if wide_bits == 64 {
            u64::MAX
        } else {
            (1_u64 << wide_bits) - 1
        };
        let element = Self::extend(cpu.vector_lane(right, narrow_bits, index), narrow_bits, signed);
        let mut result = 0_u128;
        for lane in 0..lanes {
            let source_lane = lane + u8::from(high) * lanes;
            let value = Self::extend(cpu.vector_lane(left, narrow_bits, source_lane), narrow_bits, signed)
                .wrapping_mul(element)
                & mask;
            let base = cpu.vector_lane(destination, wide_bits, lane);
            let value = match operation {
                SimdWideOperation::MultiplyLong => value,
                SimdWideOperation::MultiplyAccumulateLong { subtract: false } => base.wrapping_add(value) & mask,
                SimdWideOperation::MultiplyAccumulateLong { subtract: true } => base.wrapping_sub(value) & mask,
                _ => unreachable!(),
            };
            result |= u128::from(value) << (u32::from(lane) * u32::from(wide_bits));
        }
        result
    }

    fn extend(value: u64, bits: u8, signed: bool) -> u64 {
        if signed {
            (((value << (64 - bits)) as i64) >> (64 - bits)) as u64
        } else {
            value
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Aarch64Decoder, Aarch64ExecutionExit, Aarch64Interpreter, Aarch64Ir, Nzcv, PcCoordinatePort};

    struct Coordinates;
    impl PcCoordinatePort for Coordinates {
        fn architectural_pc(&self, pc: u64) -> u64 {
            pc
        }
    }

    #[test]
    fn encodings() {
        for (base, operation) in [
            (0x0e20_c000, SimdWideOperation::MultiplyLong),
            (
                0x0e20_8000,
                SimdWideOperation::MultiplyAccumulateLong { subtract: false },
            ),
            (
                0x0e20_a000,
                SimdWideOperation::MultiplyAccumulateLong { subtract: true },
            ),
        ] {
            vector_family(base, operation);
        }
        for (base, operation) in [
            (0x0f00_a000, SimdWideOperation::MultiplyLong),
            (
                0x0f00_2000,
                SimdWideOperation::MultiplyAccumulateLong { subtract: false },
            ),
            (
                0x0f00_6000,
                SimdWideOperation::MultiplyAccumulateLong { subtract: true },
            ),
        ] {
            indexed_family(base, operation);
        }
        for word in [0x0f00_a000, 0x0fc0_a000, 0x2f00_2000, 0x2fc0_6000] {
            assert_eq!(LongProduct::decode(word), Some(Err(Aarch64DecodeError::Reserved)));
        }
    }

    fn indexed_family(base: u32, operation: SimdWideOperation) {
        for signed in [true, false] {
            for high in [false, true] {
                let base = base | u32::from(!signed) << 29 | u32::from(high) << 30;
                indexed_encodings(base, operation, signed, 16, 16, 8);
                indexed_encodings(base, operation, signed, 32, 32, 4);
            }
        }
    }

    fn vector_family(base: u32, operation: SimdWideOperation) {
        for configuration in 0_u32..12 {
            let signed = configuration / 6 == 0;
            let high = configuration / 3 % 2 != 0;
            let size = configuration % 3;
            vector_encodings(
                base | u32::from(!signed) << 29 | u32::from(high) << 30 | size << 22,
                operation,
                signed,
                (8 << size) as u8,
                high,
            );
        }
    }

    fn vector_encodings(base: u32, operation: SimdWideOperation, signed: bool, lane_bits: u8, high: bool) {
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
                    instruction: Aarch64Instruction::SimdWide {
                        operation,
                        signed,
                        lane_bits,
                        left: left as u8,
                        right: right as u8,
                        destination: destination as u8,
                        high
                    }
                })
            );
        }
    }

    fn indexed_encodings(base: u32, operation: SimdWideOperation, signed: bool, bits: u8, registers: u8, indexes: u8) {
        let shape = if bits == 16 { 1 << 22 } else { 2 << 22 };
        for combined in 0_u32..u32::from(registers) * u32::from(indexes) * 1024 {
            let right = (combined / (u32::from(indexes) * 1024)) as u8;
            let index = (combined / 1024 % u32::from(indexes)) as u8;
            let encoded = combined % 1024;
            let left = encoded / 32;
            let destination = encoded % 32;
            let word = base | shape | u32::from(right) << 16 | encode(bits, index) | left << 5 | destination;
            assert_eq!(
                LongProduct::decode(word),
                Some(Ok(Aarch64Instruction::SimdLongElement {
                    operation,
                    signed,
                    narrow_bits: bits,
                    left: left as u8,
                    right,
                    index,
                    destination: destination as u8,
                    high: base >> 30 & 1 != 0
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
    fn aliases_random() {
        let mut state = 0xc2b2_ae35_u32;
        let mut cpu = Aarch64CpuState::default();
        for configuration in 0_u8..12 {
            let bits = 8 << (configuration % 3);
            let signed = configuration / 3 % 2 != 0;
            let high = configuration / 6 != 0;
            check_vector(&mut cpu, &mut state, bits, signed, high);
        }
        for bits in [16, 32] {
            element_shapes(&mut cpu, &mut state, bits);
        }
        cpu.set_vector(0, random128(&mut state));
        let expected = reference(
            &cpu,
            SimdWideOperation::MultiplyAccumulateLong { subtract: false },
            true,
            16,
            0,
            0,
            Some(0),
            0,
            false,
        );
        assert_eq!(
            LongProduct::execute(
                &cpu,
                SimdWideOperation::MultiplyAccumulateLong { subtract: false },
                true,
                16,
                0,
                0,
                0,
                0,
                false
            ),
            expected
        );
    }

    fn element_shapes(cpu: &mut Aarch64CpuState, state: &mut u32, bits: u8) {
        for signed in [false, true] {
            for high in [false, true] {
                check_element(cpu, state, bits, signed, high);
            }
        }
    }

    #[test]
    fn retained_sequence() {
        let source = [i32::MIN as u32, i32::MAX as u32, (-1_000_003_i32) as u32, 77_777_u32];
        let packed = source.iter().enumerate().fold(0_u128, |value, (lane, element)| {
            value | u128::from(*element) << (lane * 32)
        });
        let words = [0x0f9f_a3fe, 0x4f9f_23fd, 0x0f9f_63fd];
        let operations = [
            SimdWideOperation::MultiplyLong,
            SimdWideOperation::MultiplyAccumulateLong { subtract: false },
            SimdWideOperation::MultiplyAccumulateLong { subtract: true },
        ];
        for (word, operation) in words.into_iter().zip(operations) {
            let destination = (word & 31) as u8;
            assert_eq!(
                Aarch64Decoder::decode(word).unwrap().instruction,
                Aarch64Instruction::SimdLongElement {
                    operation,
                    signed: true,
                    narrow_bits: 32,
                    left: 31,
                    right: 31,
                    index: 0,
                    destination,
                    high: word >> 30 & 1 != 0
                }
            );
            let mut cpu = Aarch64CpuState {
                pc: 0x4036_38,
                nzcv: Nzcv::from_bits(0x6000_0000),
                ..Default::default()
            };
            cpu.set_vector(31, packed);
            cpu.set_vector(destination, u128::MAX);
            let snapshot = cpu.clone();
            let expected = reference(
                &snapshot,
                operation,
                true,
                32,
                31,
                31,
                Some(0),
                destination,
                word >> 30 & 1 != 0,
            );
            assert_eq!(
                Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
                Aarch64ExecutionExit::Continue
            );
            assert_eq!(cpu.vector(destination), expected, "word={word:08x}");
            assert_eq!(cpu.nzcv.bits(), 0x6000_0000);
        }
    }

    fn check_vector(cpu: &mut Aarch64CpuState, state: &mut u32, bits: u8, signed: bool, high: bool) {
        for _ in 0..2_000 {
            cpu.set_vector(0, random128(state));
            cpu.set_vector(1, random128(state));
            cpu.set_vector(2, random128(state));
            for operation in operations() {
                let expected = reference(cpu, operation, signed, bits, 1, 2, None, 0, high);
                assert_eq!(
                    crate::aarch64_simd_wide_interpreter::execute(cpu, operation, signed, bits, 1, 2, 0, high).0,
                    expected
                );
            }
        }
    }

    fn check_element(cpu: &mut Aarch64CpuState, state: &mut u32, bits: u8, signed: bool, high: bool) {
        let indexes = 128 / bits;
        for _ in 0..2_000 {
            cpu.set_vector(0, random128(state));
            cpu.set_vector(1, random128(state));
            cpu.set_vector(2, random128(state));
            let index = (next(state) as u8) % indexes;
            for operation in operations() {
                let expected = reference(cpu, operation, signed, bits, 1, 2, Some(index), 0, high);
                assert_eq!(
                    LongProduct::execute(cpu, operation, signed, bits, 1, 2, index, 0, high),
                    expected
                );
            }
        }
    }

    fn operations() -> [SimdWideOperation; 3] {
        [
            SimdWideOperation::MultiplyLong,
            SimdWideOperation::MultiplyAccumulateLong { subtract: false },
            SimdWideOperation::MultiplyAccumulateLong { subtract: true },
        ]
    }

    fn reference(
        cpu: &Aarch64CpuState,
        operation: SimdWideOperation,
        signed: bool,
        bits: u8,
        left: u8,
        right: u8,
        index: Option<u8>,
        destination: u8,
        high: bool,
    ) -> u128 {
        let lanes = 64 / bits;
        let wide = bits * 2;
        let mask = if wide == 64 { u64::MAX } else { (1_u64 << wide) - 1 };
        let mut result = 0_u128;
        for lane in 0..lanes {
            let source_lane = lane + u8::from(high) * lanes;
            let a = extend(cpu.vector_lane(left, bits, source_lane), bits, signed);
            let b = extend(cpu.vector_lane(right, bits, index.unwrap_or(source_lane)), bits, signed);
            let product = a.wrapping_mul(b) & mask;
            let base = cpu.vector_lane(destination, wide, lane);
            let value = match operation {
                SimdWideOperation::MultiplyLong => product,
                SimdWideOperation::MultiplyAccumulateLong { subtract: false } => base.wrapping_add(product) & mask,
                SimdWideOperation::MultiplyAccumulateLong { subtract: true } => base.wrapping_sub(product) & mask,
                _ => unreachable!(),
            };
            result |= u128::from(value) << (u32::from(lane) * u32::from(wide));
        }
        result
    }

    fn extend(value: u64, bits: u8, signed: bool) -> u64 {
        if signed {
            (((value << (64 - bits)) as i64) >> (64 - bits)) as u64
        } else {
            value
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
