use crate::{Aarch64CpuState, NarrowMode, SimdWideOperation};

pub(crate) fn execute(
    cpu: &Aarch64CpuState,
    operation: SimdWideOperation,
    signed: bool,
    narrow_bits: u8,
    left: u8,
    right: u8,
    destination: u8,
    high: bool,
) -> (u128, bool, bool) {
    match operation {
        SimdWideOperation::PairAddLong => WideMath::pair_add_long(cpu, signed, narrow_bits, left, high),
        SimdWideOperation::SaturatingNarrow {
            source_signed,
            destination_signed,
        } => WideMath::saturating_narrow(
            cpu,
            source_signed,
            destination_signed,
            narrow_bits,
            left,
            destination,
            high,
        ),
        SimdWideOperation::AddHighNarrow { rounding } => {
            WideMath::high_narrow(cpu, false, rounding, narrow_bits, left, right, destination, high)
        }
        SimdWideOperation::SubtractHighNarrow { rounding } => {
            WideMath::high_narrow(cpu, true, rounding, narrow_bits, left, right, destination, high)
        }
        SimdWideOperation::ShiftNarrow { amount, rounding, mode } => {
            WideMath::shift_narrow(cpu, amount, rounding, mode, narrow_bits, left, destination, high)
        }
        SimdWideOperation::ShiftLong { amount } => WideMath::shift_long(cpu, signed, amount, narrow_bits, left, high),
        operation => WideMath::widen(cpu, operation, signed, narrow_bits, left, right, destination, high),
    }
}

struct WideMath;

impl WideMath {
    fn pair_add_long(
        cpu: &Aarch64CpuState,
        signed: bool,
        narrow_bits: u8,
        source: u8,
        wide: bool,
    ) -> (u128, bool, bool) {
        let source_lanes = if wide { 128 } else { 64 } / narrow_bits;
        let result_bits = narrow_bits * 2;
        let result_mask = Self::mask(result_bits);
        let mut result = 0_u128;
        for lane in 0..source_lanes / 2 {
            let first = Self::extend(
                signed,
                cpu.vector_lane(source, narrow_bits, lane * 2),
                narrow_bits,
                result_mask,
            );
            let second = Self::extend(
                signed,
                cpu.vector_lane(source, narrow_bits, lane * 2 + 1),
                narrow_bits,
                result_mask,
            );
            result = Self::insert(result, first.wrapping_add(second) & result_mask, result_bits, lane);
        }
        (result, wide, false)
    }

    fn shift_long(
        cpu: &Aarch64CpuState,
        signed: bool,
        amount: u8,
        narrow_bits: u8,
        source: u8,
        high: bool,
    ) -> (u128, bool, bool) {
        let lanes = 64 / narrow_bits;
        let wide_bits = narrow_bits * 2;
        let wide_mask = Self::mask(wide_bits);
        let mut result = 0;
        for lane in 0..lanes {
            let source_lane = lane + u8::from(high) * lanes;
            let value = Self::extend(
                signed,
                cpu.vector_lane(source, narrow_bits, source_lane),
                narrow_bits,
                wide_mask,
            ) << amount;
            result = Self::insert(result, value & wide_mask, wide_bits, lane);
        }
        (result, true, false)
    }

    fn shift_narrow(
        cpu: &Aarch64CpuState,
        amount: u8,
        rounding: bool,
        mode: NarrowMode,
        narrow_bits: u8,
        source: u8,
        destination: u8,
        high: bool,
    ) -> (u128, bool, bool) {
        let lanes = 64 / narrow_bits;
        let wide_bits = narrow_bits * 2;
        let mut result = if high { cpu.vector(destination) } else { 0 };
        let mut saturated = false;
        for lane in 0..lanes {
            let raw = cpu.vector_lane(source, wide_bits, lane);
            let value = match mode {
                NarrowMode::Truncate if amount == 0 => raw,
                NarrowMode::Truncate => raw.wrapping_add(u64::from(rounding) << (amount - 1)) >> amount,
                NarrowMode::Saturate {
                    source_signed,
                    destination_signed,
                } => {
                    let source = Self::signed_source(source_signed, raw, wide_bits);
                    let shifted = (source + (i128::from(rounding) << (amount - 1))) >> amount;
                    let (minimum, maximum) = Self::narrow_bounds(destination_signed, narrow_bits);
                    let clamped = shifted.clamp(minimum, maximum);
                    saturated |= shifted != clamped;
                    clamped as u64
                }
            };
            let output_lane = lane + u8::from(high) * lanes;
            result = Self::insert(result, value, narrow_bits, output_lane);
        }
        (result, high, saturated)
    }

    fn saturating_narrow(
        cpu: &Aarch64CpuState,
        source_signed: bool,
        destination_signed: bool,
        narrow_bits: u8,
        source: u8,
        destination: u8,
        high: bool,
    ) -> (u128, bool, bool) {
        let lanes = 64 / narrow_bits;
        let wide_bits = narrow_bits * 2;
        let mut result = if high { cpu.vector(destination) } else { 0 };
        let mut saturated = false;
        for lane in 0..lanes {
            let raw = cpu.vector_lane(source, wide_bits, lane);
            let value = Self::signed_source(source_signed, raw, wide_bits);
            let (minimum, maximum) = Self::narrow_bounds(destination_signed, narrow_bits);
            let clamped = value.clamp(minimum, maximum);
            saturated |= clamped != value;
            let output_lane = lane + u8::from(high) * lanes;
            result = Self::insert(result, clamped as u64, narrow_bits, output_lane);
        }
        (result, high, saturated)
    }

    fn high_narrow(
        cpu: &Aarch64CpuState,
        subtract: bool,
        rounding: bool,
        narrow_bits: u8,
        left: u8,
        right: u8,
        destination: u8,
        high: bool,
    ) -> (u128, bool, bool) {
        let lanes = 64 / narrow_bits;
        let wide_bits = narrow_bits * 2;
        let mut result = if high { cpu.vector(destination) } else { 0 };
        for lane in 0..lanes {
            let left = cpu.vector_lane(left, wide_bits, lane);
            let right = cpu.vector_lane(right, wide_bits, lane);
            let combined = Self::add_or_subtract(subtract, left, right);
            let bias = u64::from(rounding) << (narrow_bits - 1);
            let value = combined.wrapping_add(bias) >> narrow_bits;
            let output_lane = lane + u8::from(high) * lanes;
            result = Self::insert(result, value, narrow_bits, output_lane);
        }
        (result, high, false)
    }

    fn widen(
        cpu: &Aarch64CpuState,
        operation: SimdWideOperation,
        signed: bool,
        narrow_bits: u8,
        left: u8,
        right: u8,
        destination: u8,
        high: bool,
    ) -> (u128, bool, bool) {
        let lanes = 64 / narrow_bits;
        let wide_bits = narrow_bits * 2;
        let wide_mask = Self::mask(wide_bits);
        let mut result = 0;
        for lane in 0..lanes {
            let source_lane = lane + u8::from(high) * lanes;
            let a = Self::left_operand(
                cpu,
                operation,
                signed,
                narrow_bits,
                wide_bits,
                wide_mask,
                left,
                lane,
                source_lane,
            );
            let b = Self::extend(
                signed,
                cpu.vector_lane(right, narrow_bits, source_lane),
                narrow_bits,
                wide_mask,
            );
            let base = cpu.vector_lane(destination, wide_bits, lane);
            let value = Self::wide_operation(operation, a, b, base) & wide_mask;
            result = Self::insert(result, value, wide_bits, lane);
        }
        (result, true, false)
    }

    fn left_operand(
        cpu: &Aarch64CpuState,
        operation: SimdWideOperation,
        signed: bool,
        narrow_bits: u8,
        wide_bits: u8,
        wide_mask: u64,
        left: u8,
        lane: u8,
        source_lane: u8,
    ) -> u64 {
        if matches!(operation, SimdWideOperation::AddWide | SimdWideOperation::SubtractWide) {
            cpu.vector_lane(left, wide_bits, lane)
        } else {
            Self::extend(
                signed,
                cpu.vector_lane(left, narrow_bits, source_lane),
                narrow_bits,
                wide_mask,
            )
        }
    }

    fn wide_operation(operation: SimdWideOperation, a: u64, b: u64, base: u64) -> u64 {
        match operation {
            SimdWideOperation::AddLong | SimdWideOperation::AddWide => a.wrapping_add(b),
            SimdWideOperation::SubtractLong | SimdWideOperation::SubtractWide => a.wrapping_sub(b),
            SimdWideOperation::MultiplyLong => a.wrapping_mul(b),
            SimdWideOperation::MultiplyAccumulateLong { subtract } => {
                let product = a.wrapping_mul(b);
                Self::add_or_subtract(subtract, base, product)
            }
            SimdWideOperation::PairAddLong
            | SimdWideOperation::SaturatingNarrow { .. }
            | SimdWideOperation::AddHighNarrow { .. }
            | SimdWideOperation::SubtractHighNarrow { .. }
            | SimdWideOperation::ShiftNarrow { .. }
            | SimdWideOperation::ShiftLong { .. } => unreachable!(),
        }
    }

    fn signed_source(signed: bool, value: u64, bits: u8) -> i128 {
        if signed {
            i128::from(((value << (64 - bits)) as i64) >> (64 - bits))
        } else {
            i128::from(value)
        }
    }

    fn narrow_bounds(signed: bool, bits: u8) -> (i128, i128) {
        if signed {
            (-(1_i128 << (bits - 1)), (1_i128 << (bits - 1)) - 1)
        } else {
            (0, (1_i128 << bits) - 1)
        }
    }

    fn extend(signed: bool, value: u64, bits: u8, mask: u64) -> u64 {
        if signed {
            (((value << (64 - bits)) as i64) >> (64 - bits)) as u64 & mask
        } else {
            value
        }
    }

    fn add_or_subtract(subtract: bool, left: u64, right: u64) -> u64 {
        if subtract {
            left.wrapping_sub(right)
        } else {
            left.wrapping_add(right)
        }
    }

    fn mask(bits: u8) -> u64 {
        if bits == 64 { u64::MAX } else { (1_u64 << bits) - 1 }
    }

    fn insert(vector: u128, value: u64, bits: u8, lane: u8) -> u128 {
        let mask = u128::from(Self::mask(bits));
        let shift = u32::from(lane) * u32::from(bits);
        vector & !(mask << shift) | (u128::from(value) & mask) << shift
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        Aarch64DecodeError, Aarch64Decoder, Aarch64ExecutionExit, Aarch64Instruction, Aarch64Interpreter, Nzcv,
        PcCoordinatePort,
    };

    struct Coordinates;

    impl PcCoordinatePort for Coordinates {
        fn architectural_pc(&self, execution_pc: u64) -> u64 {
            execution_pc
        }
    }

    #[test]
    fn pair_add_long_encodings_and_execution() {
        for (unsigned, signed) in [(false, true), (true, false)] {
            for size in 0..3_u32 {
                for wide in [false, true] {
                    for registers in 0..1024_u32 {
                        let source = registers >> 5;
                        let destination = registers & 31;
                        let word = 0x0e20_2800
                            | u32::from(unsigned) << 29
                            | u32::from(wide) << 30
                            | size << 22
                            | source << 5
                            | destination;
                        assert_eq!(
                            Aarch64Decoder::decode(word).unwrap().instruction,
                            Aarch64Instruction::SimdWide {
                                operation: SimdWideOperation::PairAddLong,
                                signed,
                                lane_bits: 8 << size,
                                left: source as u8,
                                right: 0,
                                destination: destination as u8,
                                high: wide,
                            }
                        );
                    }
                }
            }
        }
        let mut cpu = Aarch64CpuState::default();
        cpu.set_vector(
            1,
            u128::from_le_bytes([255, 1, 128, 128, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]),
        );
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x6e20_2820),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector(0), 0x001d_0019_0015_0011_000d_0009_0100_0100);
    }

    fn families() -> [(u8, bool, NarrowMode); 8] {
        [
            (0x10, false, NarrowMode::Truncate),
            (0x11, false, NarrowMode::Truncate),
            (
                0x10,
                true,
                NarrowMode::Saturate {
                    source_signed: true,
                    destination_signed: false,
                },
            ),
            (
                0x11,
                true,
                NarrowMode::Saturate {
                    source_signed: true,
                    destination_signed: false,
                },
            ),
            (
                0x12,
                false,
                NarrowMode::Saturate {
                    source_signed: true,
                    destination_signed: true,
                },
            ),
            (
                0x13,
                false,
                NarrowMode::Saturate {
                    source_signed: true,
                    destination_signed: true,
                },
            ),
            (
                0x12,
                true,
                NarrowMode::Saturate {
                    source_signed: false,
                    destination_signed: false,
                },
            ),
            (
                0x13,
                true,
                NarrowMode::Saturate {
                    source_signed: false,
                    destination_signed: false,
                },
            ),
        ]
    }

    #[test]
    fn plain_narrow_exact_words() {
        let source = 0x89ab_cdef_0123_4567_fedc_ba98_7654_3210_u128;
        let prior = 0x8877_6655_4433_2211_fedc_ba98_7654_3210_u128;
        for (bits, size) in [(8_u8, 0_u32), (16, 1), (32, 2)] {
            for high in [false, true] {
                let word = 0x0e21_2800 | size << 22 | u32::from(high) << 30 | 30 << 5 | 31;
                let (expected, saturated) = vector_reference(source, prior, bits, 0, false, NarrowMode::Truncate, high);
                let mut cpu = Aarch64CpuState {
                    fpsr: 0x80,
                    ..Default::default()
                };
                cpu.set_vector(30, source);
                cpu.set_vector(31, prior);
                assert_eq!(
                    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
                    Aarch64ExecutionExit::Continue,
                    "{word:#010x}"
                );
                assert_eq!(cpu.vector(31), expected, "{word:#010x}");
                assert!(!saturated);
                assert_eq!(cpu.fpsr, 0x80, "{word:#010x}");
            }
        }

        let mut cpu = Aarch64CpuState::default();
        cpu.set_vector(31, 0x1718_81fa_ed6b_c420_u128 | (0x0000_b8c4_0fd7_6b5e_u128 << 64));
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x0ea1_2bff),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector(31), 0x0fd7_6b5e_ed6b_c420);
    }

    #[test]
    fn saturating_narrow_exact_words() {
        let cases = [
            (0x0e21_48e6, true, true),
            (0x4e21_48e6, true, true),
            (0x2e61_4928, false, false),
            (0x6e61_4928, false, false),
            (0x2ea1_296a, true, false),
            (0x6ea1_296a, true, false),
        ];
        for (word, source_signed, destination_signed) in cases {
            assert!(
                matches!(
                    Aarch64Decoder::decode(word).unwrap().instruction,
                    Aarch64Instruction::SimdWide {
                        operation: SimdWideOperation::SaturatingNarrow {
                            source_signed: decoded_source,
                            destination_signed: decoded_destination,
                        },
                        ..
                    } if decoded_source == source_signed && decoded_destination == destination_signed
                ),
                "{word:#010x}"
            );
        }
    }

    fn encoding(opcode: u8, unsigned: bool, bits: u8, amount: u8, high: bool, source: u8, destination: u8) -> u32 {
        let combined = 2 * bits - amount;
        0x0f00_0400
            | u32::from(unsigned) << 29
            | u32::from(high) << 30
            | u32::from(combined >> 3) << 19
            | u32::from(combined & 7) << 16
            | u32::from(opcode) << 11
            | u32::from(source) << 5
            | u32::from(destination)
    }

    #[test]
    fn narrow_decode() {
        for (opcode, unsigned, mode) in families() {
            for bits in [8_u8, 16, 32] {
                decode_shape(opcode, unsigned, mode, bits);
            }
        }
    }

    fn decode_shape(opcode: u8, unsigned: bool, mode: NarrowMode, bits: u8) {
        for amount in 1..=bits {
            for high in [false, true] {
                decode_registers(opcode, unsigned, mode, bits, amount, high);
            }
        }
    }

    fn decode_registers(opcode: u8, unsigned: bool, mode: NarrowMode, bits: u8, amount: u8, high: bool) {
        for source in 0..32 {
            for destination in 0..32 {
                let word = encoding(opcode, unsigned, bits, amount, high, source, destination);
                assert_eq!(
                    Aarch64Decoder::decode(word).unwrap().instruction,
                    Aarch64Instruction::SimdWide {
                        operation: SimdWideOperation::ShiftNarrow {
                            amount,
                            rounding: opcode & 1 != 0,
                            mode,
                        },
                        signed: false,
                        lane_bits: bits,
                        left: source,
                        right: 0,
                        destination,
                        high,
                    },
                    "{word:#010x}"
                );
            }
        }
    }

    fn lane_reference(
        raw: u64,
        wide_bits: u8,
        narrow_bits: u8,
        amount: u8,
        rounding: bool,
        mode: NarrowMode,
    ) -> (u64, bool) {
        let narrow_mask = (1_u64 << narrow_bits) - 1;
        match mode {
            NarrowMode::Truncate if amount == 0 => (raw & narrow_mask, false),
            NarrowMode::Truncate => (
                raw.wrapping_add(u64::from(rounding) << (amount - 1)) >> amount & narrow_mask,
                false,
            ),
            NarrowMode::Saturate {
                source_signed,
                destination_signed,
            } => {
                let source = if source_signed {
                    i128::from(((raw << (64 - wide_bits)) as i64) >> (64 - wide_bits))
                } else {
                    i128::from(raw)
                };
                let shifted = (source + (i128::from(rounding) << (amount - 1))) >> amount;
                let bounds = if destination_signed {
                    (-(1_i128 << (narrow_bits - 1)), (1_i128 << (narrow_bits - 1)) - 1)
                } else {
                    (0, (1_i128 << narrow_bits) - 1)
                };
                let value = shifted.clamp(bounds.0, bounds.1);
                (value as u64 & narrow_mask, value != shifted)
            }
        }
    }

    fn vector_reference(
        source: u128,
        prior: u128,
        bits: u8,
        amount: u8,
        rounding: bool,
        mode: NarrowMode,
        high: bool,
    ) -> (u128, bool) {
        let wide_bits = 2 * bits;
        let wide_mask = if wide_bits == 64 {
            u64::MAX
        } else {
            (1_u64 << wide_bits) - 1
        };
        let lanes = 64 / bits;
        let mut result = if high { prior } else { 0 };
        let mut saturated = false;
        for lane in 0..lanes {
            let raw = (source >> (u32::from(lane) * u32::from(wide_bits))) as u64 & wide_mask;
            let (value, lane_saturated) = lane_reference(raw, wide_bits, bits, amount, rounding, mode);
            let output = lane + u8::from(high) * lanes;
            let shift = u32::from(output) * u32::from(bits);
            let mask = u128::from((1_u64 << bits) - 1) << shift;
            result = result & !mask | u128::from(value) << shift;
            saturated |= lane_saturated;
        }
        (result, saturated)
    }

    #[test]
    fn narrow_execute() {
        for (opcode, unsigned, mode) in families() {
            for bits in [8_u8, 16, 32] {
                execute_shape(opcode, unsigned, mode, bits);
            }
        }
    }

    fn execute_shape(opcode: u8, unsigned: bool, mode: NarrowMode, bits: u8) {
        for amount in 1..=bits {
            execute_amount(opcode, unsigned, mode, bits, amount);
        }
    }

    fn execute_amount(opcode: u8, unsigned: bool, mode: NarrowMode, bits: u8, amount: u8) {
        let samples = [
            0_u128,
            u128::MAX,
            0x8000_0000_0000_0000_7fff_ffff_ffff_ffff,
            0x00ff_ff00_0080_007f_ffff_0001_8000_7fff,
        ];
        for high in [false, true] {
            for source in samples {
                execute_case(opcode, unsigned, mode, bits, amount, high, source);
            }
        }
    }

    fn execute_case(opcode: u8, unsigned: bool, mode: NarrowMode, bits: u8, amount: u8, high: bool, source: u128) {
        let prior = 0x8877_6655_4433_2211_fedc_ba98_7654_3210;
        let rounding = opcode & 1 != 0;
        let (expected, saturated) = vector_reference(source, prior, bits, amount, rounding, mode, high);
        let mut cpu = Aarch64CpuState {
            pc: 0x4000,
            nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY),
            fpsr: 0x80,
            ..Default::default()
        };
        cpu.set_vector(30, source);
        cpu.set_vector(31, prior);
        let word = encoding(opcode, unsigned, bits, amount, high, 30, 31);
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
            Aarch64ExecutionExit::Continue,
            "{word:#010x}"
        );
        assert_eq!(cpu.vector(31), expected, "{word:#010x}");
        assert_eq!(cpu.fpsr & 1 << 27 != 0, saturated, "{word:#010x}");
        assert_eq!(cpu.fpsr & !(1 << 27), 0x80, "{word:#010x}");
        assert_eq!(cpu.nzcv.bits(), Nzcv::NEGATIVE | Nzcv::CARRY);
        assert_eq!(cpu.pc, 0x4004);
    }

    #[test]
    fn narrow_alias() {
        let source = 0xffff_ffff_8000_0000_7fff_ffff_0000_0001;
        for (opcode, unsigned, mode) in families() {
            let word = encoding(opcode, unsigned, 32, 32, true, 31, 31);
            let (expected, saturated) = vector_reference(source, source, 32, 32, opcode & 1 != 0, mode, true);
            let mut cpu = Aarch64CpuState::default();
            cpu.set_vector(31, source);
            assert_eq!(
                Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
                Aarch64ExecutionExit::Continue
            );
            assert_eq!(cpu.vector(31), expected, "{word:#010x}");
            assert_eq!(cpu.fpsr & 1 << 27 != 0, saturated, "{word:#010x}");
        }
    }

    #[test]
    fn narrow_sticky() {
        let word = encoding(0x10, false, 8, 8, false, 30, 31);
        let mut cpu = Aarch64CpuState {
            fpsr: 1 << 27,
            ..Default::default()
        };
        cpu.set_vector(30, 0);
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
            Aarch64ExecutionExit::Continue
        );
        assert_ne!(cpu.fpsr & 1 << 27, 0);
    }

    #[test]
    fn narrow_frontier() {
        let word = 0x2f20_9fdf;
        let mut cpu = Aarch64CpuState {
            pc: 0x400780,
            ..Default::default()
        };
        cpu.set_vector(30, 0xffff_ffff_ffff_ffff_8000_0000_7fff_ffff);
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector(31), 0xffff_ffff_8000_0000);
        assert_ne!(cpu.fpsr & 1 << 27, 0);
        assert_eq!(cpu.pc, 0x400784);
    }

    #[test]
    fn narrow_reserved() {
        for (opcode, unsigned, _) in families() {
            for high in [false, true] {
                let word = encoding(opcode, unsigned, 64, 1, high, 30, 31);
                assert_eq!(Aarch64Decoder::decode(word), Err(Aarch64DecodeError::Reserved));
                let mut cpu = Aarch64CpuState {
                    pc: 0x9000,
                    fpsr: 0x0800_0080,
                    nzcv: Nzcv::from_bits(Nzcv::OVERFLOW),
                    ..Default::default()
                };
                cpu.set_vector(30, u128::MAX);
                cpu.set_vector(31, 0x1234);
                let before = cpu.clone();
                assert_eq!(
                    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
                    Aarch64ExecutionExit::UndefinedInstruction {
                        instruction: 0x9000,
                        word
                    }
                );
                assert_eq!(cpu, before);
            }
        }
    }
}
