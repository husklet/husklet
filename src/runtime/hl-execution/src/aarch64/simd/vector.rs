use super::interpreter::Aarch64SimdInterpreter;
use crate::{Aarch64CpuState, SimdCopy, SimdPermute, SimdShift, SimdUnary};

impl Aarch64SimdInterpreter {
    pub(super) fn table(
        cpu: &Aarch64CpuState,
        first_table: u8,
        table_count: u8,
        indexes: u8,
        destination: u8,
        extend: bool,
        wide: bool,
    ) -> u128 {
        let bytes = if wide { 16 } else { 8 };
        let index_bytes = cpu.vector(indexes).to_le_bytes();
        let mut result = if extend {
            cpu.vector(destination).to_le_bytes()
        } else {
            [0; 16]
        };
        for index in 0..bytes {
            let selector = usize::from(index_bytes[index]);
            if selector < usize::from(table_count) * 16 {
                let register = first_table.wrapping_add((selector / 16) as u8) & 31;
                result[index] = cpu.vector(register).to_le_bytes()[selector % 16];
            }
        }
        u128::from_le_bytes(result)
    }
    pub(super) fn execute_copy(
        cpu: &Aarch64CpuState,
        staged: &mut Aarch64CpuState,
        operation: SimdCopy,
        lane_bits: u8,
        lane: u8,
        source: u8,
        destination: u8,
        wide: bool,
    ) {
        match operation {
            SimdCopy::DuplicateElement { source_lane } => {
                Self::duplicate(
                    staged,
                    destination,
                    lane_bits,
                    cpu.vector_lane(source, lane_bits, source_lane),
                    wide,
                );
            }
            SimdCopy::DuplicateGeneral => {
                Self::duplicate(staged, destination, lane_bits, cpu.register(source), wide);
            }
            SimdCopy::InsertElement { source_lane } => {
                staged.set_vector_lane(
                    destination,
                    lane_bits,
                    lane,
                    cpu.vector_lane(source, lane_bits, source_lane),
                );
            }
            SimdCopy::InsertGeneral => {
                staged.set_vector_lane(destination, lane_bits, lane, cpu.register(source));
            }
            SimdCopy::MoveUnsigned => {
                let value = cpu.vector_lane(source, lane_bits, lane);
                if wide {
                    staged.set_register(destination, value);
                } else {
                    staged.set_narrow_register(destination, value as u32);
                }
            }
            SimdCopy::MoveSigned => {
                let value = Self::sign_extend(cpu.vector_lane(source, lane_bits, lane), lane_bits);
                if wide {
                    staged.set_register(destination, value);
                } else {
                    staged.set_narrow_register(destination, value as u32);
                }
            }
        }
    }
    pub(super) fn permute(
        cpu: &Aarch64CpuState,
        staged: &mut Aarch64CpuState,
        operation: SimdPermute,
        lane_bits: u8,
        left: u8,
        right: u8,
        destination: u8,
        wide: bool,
    ) {
        let lanes = if wide { 128 } else { 64 } / lane_bits;
        let half = lanes / 2;
        let mut result = 0_u128;
        for lane in 0..lanes {
            let (source, index) = Self::permute_source(operation, lane, half, left, right);
            result |= u128::from(cpu.vector_lane(source, lane_bits, index)) << (u32::from(lane) * u32::from(lane_bits));
        }
        staged.write_vector_width(destination, result, wide);
    }
    pub(super) fn permute_source(operation: SimdPermute, lane: u8, half: u8, left: u8, right: u8) -> (u8, u8) {
        match operation {
            SimdPermute::UnzipLow | SimdPermute::UnzipHigh => {
                let local = if lane < half { lane } else { lane - half };
                (
                    if lane < half { left } else { right },
                    local * 2 + u8::from(operation == SimdPermute::UnzipHigh),
                )
            }
            SimdPermute::TransposeLow | SimdPermute::TransposeHigh => (
                if lane & 1 == 0 { left } else { right },
                lane & !1 | u8::from(operation == SimdPermute::TransposeHigh),
            ),
            SimdPermute::ZipLow | SimdPermute::ZipHigh => (
                if lane & 1 == 0 { left } else { right },
                u8::from(operation == SimdPermute::ZipHigh) * half + lane / 2,
            ),
        }
    }
    pub(super) fn reverse(value: u128, lane_bits: u8, container_bytes: u8, wide: bool) -> [u8; 16] {
        let bytes = value.to_le_bytes();
        let mut result = [0_u8; 16];
        let width = if wide { 16 } else { 8 };
        let element = usize::from(lane_bits / 8);
        let container = usize::from(container_bytes);
        for base in (0..width).step_by(container) {
            for offset in (0..container).step_by(element) {
                let target = base + container - element - offset;
                result[target..target + element].copy_from_slice(&bytes[base + offset..base + offset + element]);
            }
        }
        result
    }
    pub(super) fn shift_lane(
        operation: SimdShift,
        value: u128,
        destination: u128,
        amount: u8,
        bits: u8,
        mask: u128,
    ) -> u128 {
        if let SimdShift::Insert { left } = operation {
            if left {
                let keep = if amount == 0 { 0 } else { (1_u128 << amount) - 1 };
                return (value << amount & mask & !keep) | (destination & keep);
            }
            let keep = if amount == 0 { 0 } else { mask << (bits - amount) & mask };
            return (value >> amount & !keep) | (destination & keep);
        }
        let SimdShift::Right {
            signed,
            rounding,
            accumulating,
        } = operation
        else {
            return value << amount & mask;
        };
        let rounded = if signed {
            Self::signed_shift(value, amount, bits, rounding, mask)
        } else {
            Self::unsigned_shift(value, amount, bits, rounding, mask)
        };
        if accumulating {
            (rounded + destination) & mask
        } else {
            rounded
        }
    }
    pub(super) fn signed_shift(value: u128, amount: u8, bits: u8, rounding: bool, mask: u128) -> u128 {
        let value = Self::signed(value, bits);
        let base = if amount >= bits {
            value >> (bits - 1)
        } else {
            value >> amount
        };
        let round = i128::from(rounding && amount > 0 && (value >> (amount - 1)) & 1 != 0);
        (base + round) as u128 & mask
    }
    pub(super) fn unsigned_shift(value: u128, amount: u8, bits: u8, rounding: bool, mask: u128) -> u128 {
        let base = if amount >= bits { 0 } else { value >> amount };
        let round = u128::from(rounding && amount > 0 && value >> (amount - 1) & 1 != 0);
        (base + round) & mask
    }
    pub(super) fn duplicate(cpu: &mut Aarch64CpuState, destination: u8, lane_bits: u8, element: u64, wide: bool) {
        let lanes = if wide { 128 } else { 64 } / lane_bits;
        let mut value = 0_u128;
        for lane in 0..lanes {
            value |= (u128::from(element) & Self::lane_mask(lane_bits)) << (u32::from(lane) * u32::from(lane_bits));
        }
        cpu.write_vector_width(destination, value, wide);
    }
    pub(super) fn unary(
        cpu: &Aarch64CpuState,
        staged: &mut Aarch64CpuState,
        operation: SimdUnary,
        lane_bits: u8,
        source: u8,
        destination: u8,
        wide: bool,
    ) {
        if let SimdUnary::Reverse { container_bytes } = operation {
            let result = Self::reverse(cpu.vector(source), lane_bits, container_bytes, wide);
            staged.write_vector_width(destination, u128::from_le_bytes(result), wide);
            return;
        }
        let lanes = if wide { 128 } else { 64 } / lane_bits;
        let mask = Self::lane_mask(lane_bits);
        let mut result = 0_u128;
        for lane in 0..lanes {
            let raw = u128::from(cpu.vector_lane(source, lane_bits, lane));
            let signed = Self::signed(raw, lane_bits);
            let value = match operation {
                SimdUnary::CountLeadingSign => {
                    let folded = ((raw >> 1) ^ raw) & (mask >> 1);
                    u128::from(Self::leading_sign_count(folded, lane_bits))
                }
                SimdUnary::CountLeadingZero => u128::from(raw.leading_zeros() - u32::from(128 - lane_bits)),
                SimdUnary::PopulationCount => u128::from(raw.count_ones()),
                SimdUnary::Not => !raw & mask,
                SimdUnary::ReverseBits => u128::from((raw as u8).reverse_bits()),
                SimdUnary::CompareGreaterZero => u128::from(signed > 0) * mask,
                SimdUnary::CompareGreaterEqualZero => u128::from(signed >= 0) * mask,
                SimdUnary::CompareEqualZero => u128::from(raw == 0) * mask,
                SimdUnary::CompareLessEqualZero => u128::from(signed <= 0) * mask,
                SimdUnary::CompareLessZero => u128::from(signed < 0) * mask,
                SimdUnary::Absolute => signed.unsigned_abs() & mask,
                SimdUnary::Negate => signed.wrapping_neg() as u128 & mask,
                SimdUnary::Reverse { .. } => unreachable!(),
            };
            result |= value << (u32::from(lane) * u32::from(lane_bits));
        }
        staged.write_vector_width(destination, result, wide);
    }
    pub(super) fn shift(
        cpu: &Aarch64CpuState,
        staged: &mut Aarch64CpuState,
        operation: SimdShift,
        amount: u8,
        lane_bits: u8,
        source: u8,
        destination: u8,
        wide: bool,
    ) {
        let lanes = if wide { 128 } else { 64 } / lane_bits;
        let mask = Self::lane_mask(lane_bits);
        let mut result = 0_u128;
        for lane in 0..lanes {
            let value = u128::from(cpu.vector_lane(source, lane_bits, lane));
            let base = u128::from(cpu.vector_lane(destination, lane_bits, lane));
            let shifted = Self::shift_lane(operation, value, base, amount, lane_bits, mask);
            result |= shifted << (u32::from(lane) * u32::from(lane_bits));
        }
        staged.write_vector_width(destination, result, wide);
    }
    pub(super) fn lane_mask(bits: u8) -> u128 {
        if bits == 64 {
            u128::from(u64::MAX)
        } else {
            (1_u128 << bits) - 1
        }
    }
    pub(super) fn lane_arithmetic(subtract: bool, left: u128, right: u128) -> u128 {
        if subtract {
            left.wrapping_sub(right)
        } else {
            left.wrapping_add(right)
        }
    }
    pub(super) fn add_lane(
        saturating: bool,
        unsigned: bool,
        subtract: bool,
        left: u128,
        right: u128,
        bits: u8,
        mask: u128,
    ) -> (u128, bool) {
        if saturating {
            Self::saturating_arithmetic(unsigned, subtract, left, right, bits)
        } else {
            (Self::lane_arithmetic(subtract, left, right) & mask, false)
        }
    }
    pub(super) fn saturating_arithmetic(
        unsigned: bool,
        subtract: bool,
        left: u128,
        right: u128,
        bits: u8,
    ) -> (u128, bool) {
        if unsigned {
            let maximum = Self::lane_mask(bits);
            if subtract {
                (left.saturating_sub(right), left < right)
            } else {
                let sum = left + right;
                (sum.min(maximum), sum > maximum)
            }
        } else {
            let left = Self::signed(left, bits);
            let right = Self::signed(right, bits);
            let value = if subtract { left - right } else { left + right };
            let maximum = (1_i128 << (bits - 1)) - 1;
            let minimum = -(1_i128 << (bits - 1));
            let clamped = value.clamp(minimum, maximum);
            (clamped as u128 & Self::lane_mask(bits), value != clamped)
        }
    }
    pub(super) fn sign_extend(value: u64, bits: u8) -> u64 {
        if bits == 64 {
            value
        } else {
            let shift = 64 - bits;
            ((value << shift) as i64 >> shift) as u64
        }
    }
    pub(super) fn signed(value: u128, bits: u8) -> i128 {
        (value << (128 - bits)) as i128 >> (128 - bits)
    }
    pub(super) fn leading_sign_count(folded: u128, bits: u8) -> u8 {
        if folded == 0 {
            bits - 1
        } else {
            folded.leading_zeros() as u8 - (128 - bits) - 1
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Aarch64Decoder, Aarch64Instruction};

    #[test]
    fn shift_insert_decode_and_bit_preservation() {
        for bits in [8_u8, 16, 32, 64] {
            let mask = Aarch64SimdInterpreter::lane_mask(bits);
            for amount in 1..=bits {
                let value = 0x5a5a_5a5a_5a5a_5a5a_u128 & mask;
                let destination = 0xa5a5_a5a5_a5a5_a5a5_u128 & mask;
                let right = Aarch64SimdInterpreter::shift_lane(
                    SimdShift::Insert { left: false },
                    value,
                    destination,
                    amount,
                    bits,
                    mask,
                );
                let left = Aarch64SimdInterpreter::shift_lane(
                    SimdShift::Insert { left: true },
                    value,
                    destination,
                    bits - amount,
                    bits,
                    mask,
                );
                let top = if amount == 0 { 0 } else { mask << (bits - amount) & mask };
                assert_eq!(right & top, destination & top);
                let low = if bits == amount {
                    0
                } else {
                    (1_u128 << (bits - amount)) - 1
                };
                assert_eq!(left & low, destination & low);
            }
        }
        assert!(matches!(
            Aarch64Decoder::decode(0x6f2c_47c4).unwrap().instruction,
            Aarch64Instruction::SimdShift {
                operation: SimdShift::Insert { left: false },
                ..
            }
        ));
    }
}
