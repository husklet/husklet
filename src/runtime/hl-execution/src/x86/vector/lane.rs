use crate::{
    AccessKind, CpuState, ExecutionExit, GuestOperandMemory, ScalarOperand, ScalarRegister, ScalarWidth, Staged,
    VectorArithmetic, VectorComparison, VectorOperation, VectorShiftKind, VectorShuffleMode, VectorSource,
};

pub struct Lane;

impl Lane {
    #[must_use]
    pub fn carryless_multiply(left: u64, right: u64) -> u128 {
        let mut product = 0_u128;
        for bit in 0..64 {
            if right >> bit & 1 != 0 {
                product ^= u128::from(left) << bit;
            }
        }
        product
    }

    #[must_use]
    pub fn ssse3(left: u128, right: u128, lane: u8, operation: crate::x86::Ssse3Operation) -> u128 {
        let bits = u32::from(lane) * 8;
        let count = 128 / bits;
        let mut output = 0_u128;
        for index in 0..count {
            let value = match operation {
                crate::x86::Ssse3Operation::Horizontal { subtract, saturating } => {
                    let half = count / 2;
                    let vector = if index < half { left } else { right };
                    let pair = index % half * 2;
                    let first = Self::signed(Self::element(vector, pair, bits), bits);
                    let second = Self::signed(Self::element(vector, pair + 1, bits), bits);
                    let value = if subtract { first - second } else { first + second };
                    if saturating {
                        Self::saturate(value, bits)
                    } else {
                        value as u128 & Self::mask(bits)
                    }
                }
                crate::x86::Ssse3Operation::Sign => {
                    let value = Self::element(left, index, bits);
                    let control = Self::signed(Self::element(right, index, bits), bits);
                    if control < 0 {
                        value.wrapping_neg() & Self::mask(bits)
                    } else if control == 0 {
                        0
                    } else {
                        value
                    }
                }
                crate::x86::Ssse3Operation::RoundedMultiply => {
                    let first = Self::signed(Self::element(left, index, bits), bits);
                    let second = Self::signed(Self::element(right, index, bits), bits);
                    ((((first * second) >> 14) + 1) >> 1) as u128 & Self::mask(bits)
                }
                crate::x86::Ssse3Operation::MultiplyAdd => {
                    let byte = index * 2;
                    let first =
                        Self::element(left, byte, 8) as u8 as i64 * Self::signed(Self::element(right, byte, 8), 8);
                    let second = Self::element(left, byte + 1, 8) as u8 as i64
                        * Self::signed(Self::element(right, byte + 1, 8), 8);
                    Self::saturate(first + second, 16)
                }
                crate::x86::Ssse3Operation::Absolute => {
                    let value = Self::element(right, index, bits);
                    if Self::signed(value, bits) < 0 {
                        value.wrapping_neg() & Self::mask(bits)
                    } else {
                        value
                    }
                }
            };
            output |= value << (index * bits);
        }
        output
    }

    const fn element(vector: u128, index: u32, bits: u32) -> u128 {
        vector >> (index * bits) & Self::mask(bits)
    }

    const fn mask(bits: u32) -> u128 {
        (1_u128 << bits) - 1
    }

    fn signed(value: u128, bits: u32) -> i64 {
        ((value << (128 - bits)) as i128 >> (128 - bits)) as i64
    }

    fn saturate(value: i64, bits: u32) -> u128 {
        let maximum = (1_i64 << (bits - 1)) - 1;
        let minimum = -(1_i64 << (bits - 1));
        value.clamp(minimum, maximum) as u128 & Self::mask(bits)
    }
    pub fn insert_word<M: GuestOperandMemory>(
        staged: &mut CpuState,
        cpu: &CpuState,
        memory: &M,
        destination: u8,
        source: ScalarOperand,
        lane: u8,
        next: u64,
        instruction: u64,
    ) -> Result<Staged<M::Reservation, M::BatchReservation>, ExecutionExit> {
        let value = match source {
            ScalarOperand::Register(register) => cpu.read_register(register, ScalarWidth::Word),
            ScalarOperand::Memory(address) => {
                let address = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                let end = address.checked_add(1).ok_or(ExecutionExit::NonCanonical {
                    instruction,
                    address,
                    access: AccessKind::Read,
                })?;
                if !Self::canonical_address(address) || !Self::canonical_address(end) {
                    return Err(ExecutionExit::NonCanonical {
                        instruction,
                        address,
                        access: AccessKind::Read,
                    });
                }
                memory.read(address, 2).map_err(|()| {
                    ExecutionExit::OperandFault(crate::FaultAccess::operand(instruction, address, AccessKind::Read, 2))
                })?
            }
            ScalarOperand::Immediate(_) => unreachable!(),
        };
        let shift = u32::from(lane) * 16;
        let mask = u128::from(u16::MAX) << shift;
        let vector = &mut staged.vectors[usize::from(destination)];
        *vector = (*vector & !mask) | (u128::from(value & 0xffff) << shift);
        Ok(Staged::Cpu)
    }

    #[must_use]
    pub fn integer(left: u128, right: u128, lane: u8, operation: VectorArithmetic) -> u128 {
        if operation == VectorArithmetic::SumAbsoluteDifferences {
            let mut result = 0_u128;
            for group in 0..2_u32 {
                let mut sum = 0_u16;
                for index in 0..8_u32 {
                    let shift = (group * 8 + index) * 8;
                    sum += ((left >> shift) as u8).abs_diff((right >> shift) as u8) as u16;
                }
                result |= u128::from(sum) << (group * 64);
            }
            return result;
        }
        if matches!(
            operation,
            VectorArithmetic::MultiplyLowWords
                | VectorArithmetic::MultiplyHighWords { .. }
                | VectorArithmetic::MultiplyAddWords
        ) {
            return Self::multiply_words(left, right, operation);
        }
        if operation == VectorArithmetic::UnsignedMultiplyEvenDwords {
            let low = u64::from(left as u32) * u64::from(right as u32);
            let high = u64::from((left >> 64) as u32) * u64::from((right >> 64) as u32);
            return u128::from(low) | (u128::from(high) << 64);
        }
        if operation == VectorArithmetic::SignedMultiplyEvenDwords {
            let low = i128::from(left as u32 as i32) * i128::from(right as u32 as i32);
            let high = i128::from((left >> 64) as u32 as i32) * i128::from((right >> 64) as u32 as i32);
            return (low as u128 & u128::from(u64::MAX)) | ((high as u128 & u128::from(u64::MAX)) << 64);
        }
        let bits = u32::from(lane) * 8;
        let mask = (1_u128 << bits) - 1;
        let mut result = 0;
        for index in 0..16 / u32::from(lane) {
            let shift = index * bits;
            let a = (left >> shift) & mask;
            let b = (right >> shift) & mask;
            let value = match operation {
                VectorArithmetic::Add => a.wrapping_add(b) & mask,
                VectorArithmetic::AddUnsignedSaturating => a.saturating_add(b).min(mask),
                VectorArithmetic::Average => (a + b + 1) >> 1,
                VectorArithmetic::Subtract => a.wrapping_sub(b) & mask,
                VectorArithmetic::MultiplyLowDwords => a.wrapping_mul(b) & mask,
                VectorArithmetic::UnsignedMinimum
                | VectorArithmetic::UnsignedMaximum
                | VectorArithmetic::SignedMinimum
                | VectorArithmetic::SignedMaximum => Self::extremum(a, b, bits, operation),
                VectorArithmetic::UnsignedMultiplyEvenDwords => unreachable!(),
                VectorArithmetic::SignedMultiplyEvenDwords => unreachable!(),
                VectorArithmetic::MultiplyLowWords
                | VectorArithmetic::MultiplyHighWords { .. }
                | VectorArithmetic::MultiplyAddWords => unreachable!(),
                VectorArithmetic::SumAbsoluteDifferences => unreachable!(),
            };
            result |= value << shift;
        }
        result
    }

    fn extremum(a: u128, b: u128, bits: u32, operation: VectorArithmetic) -> u128 {
        match operation {
            VectorArithmetic::UnsignedMinimum => a.min(b),
            VectorArithmetic::UnsignedMaximum => a.max(b),
            VectorArithmetic::SignedMinimum => (a ^ (1 << (bits - 1))).min(b ^ (1 << (bits - 1))) ^ (1 << (bits - 1)),
            VectorArithmetic::SignedMaximum => (a ^ (1 << (bits - 1))).max(b ^ (1 << (bits - 1))) ^ (1 << (bits - 1)),
            VectorArithmetic::Add
            | VectorArithmetic::Subtract
            | VectorArithmetic::UnsignedMultiplyEvenDwords
            | VectorArithmetic::SignedMultiplyEvenDwords
            | VectorArithmetic::MultiplyLowDwords => {
                unreachable!()
            }
            VectorArithmetic::MultiplyLowWords
            | VectorArithmetic::MultiplyHighWords { .. }
            | VectorArithmetic::MultiplyAddWords => unreachable!(),
            VectorArithmetic::SumAbsoluteDifferences => unreachable!(),
            VectorArithmetic::AddUnsignedSaturating => unreachable!(),
            VectorArithmetic::Average => unreachable!(),
        }
    }

    fn multiply_words(left: u128, right: u128, operation: VectorArithmetic) -> u128 {
        let mut result = 0_u128;
        if operation == VectorArithmetic::MultiplyAddWords {
            for pair in 0..4 {
                let mut sum = 0_i32;
                for lane in 0..2 {
                    let shift = (pair * 2 + lane) * 16;
                    let a = (left >> shift) as u16 as i16 as i32;
                    let b = (right >> shift) as u16 as i16 as i32;
                    sum = sum.wrapping_add(a.wrapping_mul(b));
                }
                result |= u128::from(sum as u32) << (pair * 32);
            }
            return result;
        }
        for lane in 0..8 {
            let shift = lane * 16;
            let product = match operation {
                VectorArithmetic::MultiplyHighWords { signed: false } => {
                    u32::from((left >> shift) as u16) * u32::from((right >> shift) as u16)
                }
                _ => ((left >> shift) as u16 as i16 as i32).wrapping_mul((right >> shift) as u16 as i16 as i32) as u32,
            };
            let value = if matches!(operation, VectorArithmetic::MultiplyHighWords { .. }) {
                product >> 16
            } else {
                product
            };
            result |= u128::from(value & 0xffff) << shift;
        }
        result
    }

    #[must_use]
    pub fn extend(value: u128, source_lane: u8, destination_lane: u8, signed: bool) -> u128 {
        let source_bits = u32::from(source_lane) * 8;
        let destination_bits = u32::from(destination_lane) * 8;
        let count = 16 / u32::from(destination_lane);
        let source_mask = (1_u128 << source_bits) - 1;
        let destination_mask = (1_u128 << destination_bits) - 1;
        let mut result = 0;
        for index in 0..count {
            let source = value >> (index * source_bits) & source_mask;
            let extended = if signed && source >> (source_bits - 1) != 0 {
                source | (destination_mask & !source_mask)
            } else {
                source
            };
            result |= extended << (index * destination_bits);
        }
        result
    }

    #[must_use]
    pub fn blend(left: u128, right: u128, selectors: u128, lane: u8, implicit: bool) -> u128 {
        let bits = u32::from(lane) * 8;
        let lane_mask = (1_u128 << bits) - 1;
        let mut result = left;
        for index in 0..16 / u32::from(lane) {
            let selected = if implicit {
                selectors >> (index * bits + bits - 1) & 1 != 0
            } else {
                selectors >> index & 1 != 0
            };
            if selected {
                let shift = index * bits;
                result = (result & !(lane_mask << shift)) | (((right >> shift) & lane_mask) << shift);
            }
        }
        result
    }

    #[must_use]
    pub fn horizontal_minimum(value: u128) -> u128 {
        let mut best = value as u16;
        let mut position = 0_u16;
        for index in 1..8_u32 {
            let lane = (value >> (index * 16)) as u16;
            if lane < best {
                best = lane;
                position = index as u16;
            }
        }
        u128::from(best) | (u128::from(position) << 16)
    }

    #[must_use]
    pub fn sad(left: u128, right: u128, control: u8) -> u128 {
        let left_offset = u32::from(control >> 2 & 1) * 4;
        let right_offset = u32::from(control & 3) * 4;
        let mut result = 0_u128;
        for window in 0..8_u32 {
            let mut sum = 0_u16;
            for byte in 0..4_u32 {
                let a = (left >> ((left_offset + window + byte) * 8)) as u8;
                let b = (right >> ((right_offset + byte) * 8)) as u8;
                sum = sum.wrapping_add(u16::from(a.abs_diff(b)));
            }
            result |= u128::from(sum) << (window * 16);
        }
        result
    }

    #[must_use]
    pub fn dot(left: u128, right: u128, control: u8, format: crate::FloatWidth, mxcsr: u32) -> (u128, u32) {
        use crate::x86::scalar::arithmetic::Arithmetic;
        let environment = Arithmetic::environment(mxcsr);
        let soft = Arithmetic::soft_format(format);
        let lane_bits = u32::from(Arithmetic::bytes(format)) * 8;
        let lanes = 128 / lane_bits;
        let lane_mask = if lane_bits == 64 {
            u128::from(u64::MAX)
        } else {
            u128::from(u32::MAX)
        };
        let mut sum = hl_softfloat::Value::from_bits(soft, 0);
        let mut exceptions = 0;
        for index in 0..lanes {
            if control & (0x10 << index) == 0 {
                continue;
            }
            let a = ((left >> (index * lane_bits)) & lane_mask) as u64;
            let b = ((right >> (index * lane_bits)) & lane_mask) as u64;
            let product = environment.multiply(
                hl_softfloat::Value::from_bits(soft, a),
                hl_softfloat::Value::from_bits(soft, b),
            );
            let added = environment.add(sum, product.value);
            sum = added.value;
            exceptions |= Arithmetic::exceptions(product.flags) | Arithmetic::exceptions(added.flags);
            if mxcsr & (1 << 6) != 0 {
                exceptions &= !(1 << 1);
            } else if Arithmetic::denormal(a, format) || Arithmetic::denormal(b, format) {
                exceptions |= 1 << 1;
            }
        }
        let mut result = 0;
        for index in 0..lanes {
            if control & (1 << index) != 0 {
                result |= u128::from(sum.bits()) << (index * lane_bits);
            }
        }
        (result, exceptions)
    }

    #[must_use]
    pub const fn shift_bytes(value: u128, count: u8, left: bool) -> u128 {
        if count >= 16 {
            return 0;
        }
        let bits = count as u32 * 8;
        if left { value << bits } else { value >> bits }
    }

    #[must_use]
    pub fn shift(value: u128, lane: u8, count: u8, kind: VectorShiftKind) -> u128 {
        let bits = u32::from(lane) * 8;
        let mask = (1_u128 << bits) - 1;
        let mut result = 0;
        for index in 0..16 / u32::from(lane) {
            let offset = index * bits;
            let source = (value >> offset) & mask;
            let shifted = Self::shift_lane(source, bits, count, kind);
            result |= shifted << offset;
        }
        result
    }

    fn shift_lane(source: u128, bits: u32, count: u8, kind: VectorShiftKind) -> u128 {
        let mask = (1_u128 << bits) - 1;
        if kind == VectorShiftKind::ArithmeticRight {
            let signed = ((source << (128 - bits)) as i128) >> (128 - bits);
            return (signed >> u32::from(count).min(bits - 1)) as u128 & mask;
        }
        if u32::from(count) >= bits {
            return 0;
        }
        if kind == VectorShiftKind::Left {
            source << count & mask
        } else {
            source >> count
        }
    }

    #[must_use]
    pub const fn bitwise(left: u128, right: u128, operation: VectorOperation) -> u128 {
        match operation {
            VectorOperation::And => left & right,
            VectorOperation::AndNot => !left & right,
            VectorOperation::Or => left | right,
            VectorOperation::Xor => left ^ right,
        }
    }

    #[must_use]
    pub fn shuffle(left: u128, right: u128, selectors: u8, mode: VectorShuffleMode) -> u128 {
        if mode == VectorShuffleMode::PackedSingle {
            return Self::shuffle_single(left, right, selectors);
        }
        if mode == VectorShuffleMode::PackedDouble {
            return Self::shuffle_double(left, right, selectors);
        }
        if mode != VectorShuffleMode::Dwords {
            return Self::shuffle_words(right, selectors, mode);
        }
        let mut result = 0;
        for destination in 0..4 {
            let source = u32::from((selectors >> (destination * 2)) & 3);
            result |= ((right >> (source * 32)) & u128::from(u32::MAX)) << (destination * 32);
        }
        result
    }

    #[must_use]
    pub fn shuffle_bytes(data: u128, control: u128) -> u128 {
        let mut result = 0;
        for lane in 0..16 {
            let index = (control >> (lane * 8)) as u8;
            if index & 0x80 == 0 {
                result |= (data >> (u32::from(index & 0x0f) * 8) & 0xff) << (lane * 8);
            }
        }
        result
    }

    fn shuffle_single(left: u128, right: u128, selectors: u8) -> u128 {
        let mut result = 0;
        for destination in 0..4 {
            let source = u32::from((selectors >> (destination * 2)) & 3);
            let value = if destination < 2 { left } else { right };
            result |= ((value >> (source * 32)) & u128::from(u32::MAX)) << (destination * 32);
        }
        result
    }

    fn shuffle_double(left: u128, right: u128, selectors: u8) -> u128 {
        let low = left >> (u32::from(selectors & 1) * 64) & u128::from(u64::MAX);
        let high = right >> (u32::from(selectors >> 1 & 1) * 64) & u128::from(u64::MAX);
        low | high << 64
    }

    fn shuffle_words(value: u128, selectors: u8, mode: VectorShuffleMode) -> u128 {
        let base = if mode == VectorShuffleMode::HighWords { 4 } else { 0 };
        let preserved = if base == 0 {
            value & (!0_u128 << 64)
        } else {
            value & u128::from(u64::MAX)
        };
        let mut result = preserved;
        for destination in 0..4 {
            let source = u32::from((selectors >> (destination * 2)) & 3);
            let word = value >> ((base + source) * 16) & u128::from(u16::MAX);
            result |= word << ((base + destination) * 16);
        }
        result
    }

    #[must_use]
    pub fn compare(left: u128, right: u128, lane: u8, comparison: VectorComparison) -> u128 {
        let bits = u32::from(lane) * 8;
        let mask = (1_u128 << bits) - 1;
        let sign = 1_u128 << (bits - 1);
        let mut result = 0;
        for index in 0..16 / u32::from(lane) {
            let shift = index * bits;
            let a = (left >> shift) & mask;
            let b = (right >> shift) & mask;
            let selected = match comparison {
                VectorComparison::Equal => a == b,
                VectorComparison::SignedGreater => (a ^ sign) > (b ^ sign),
            };
            if selected {
                result |= mask << shift;
            }
        }
        result
    }

    #[must_use]
    pub fn sign_mask(value: u128, lane: u8) -> u16 {
        let bits = u32::from(lane) * 8;
        let mut result = 0;
        for index in 0..16 / u32::from(lane) {
            result |= (((value >> (index * bits + bits - 1)) & 1) as u16) << index;
        }
        result
    }

    pub fn write_mask(cpu: &mut CpuState, destination: ScalarRegister, source: u8, lane: u8) {
        let value = Self::sign_mask(cpu.vectors[usize::from(source)], lane);
        cpu.write_register(destination, ScalarWidth::Dword, u64::from(value));
    }

    pub fn read<M: GuestOperandMemory>(
        cpu: &CpuState,
        memory: &M,
        source: VectorSource,
        next: u64,
        instruction: u64,
    ) -> Result<u128, ExecutionExit> {
        let VectorSource::Memory(address) = source else {
            let VectorSource::Register(index) = source else {
                unreachable!()
            };
            return Ok(cpu.vectors[usize::from(index)]);
        };
        let address = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        Self::canonical(address, instruction)?;
        let low = memory.read(address, 8).map_err(|()| {
            ExecutionExit::OperandFault(crate::FaultAccess::operand(instruction, address, AccessKind::Read, 16))
        })?;
        let high = memory.read(address + 8, 8).map_err(|()| {
            ExecutionExit::OperandFault(crate::FaultAccess::operand(
                instruction,
                address + 8,
                AccessKind::Read,
                16,
            ))
        })?;
        Ok(u128::from(low) | (u128::from(high) << 64))
    }

    #[must_use]
    pub fn unpack(left: u128, right: u128, lane: u8, high: bool) -> u128 {
        let bits = u32::from(lane) * 8;
        let mask = (1_u128 << bits) - 1;
        let lanes = 16 / u32::from(lane);
        let base = if high { lanes / 2 } else { 0 };
        let mut result = 0;
        for index in 0..lanes / 2 {
            let source = base + index;
            result |= ((left >> (source * bits)) & mask) << ((index * 2) * bits);
            result |= ((right >> (source * bits)) & mask) << ((index * 2 + 1) * bits);
        }
        result
    }

    fn canonical(address: u64, instruction: u64) -> Result<(), ExecutionExit> {
        let end = address.checked_add(15).ok_or(ExecutionExit::NonCanonical {
            instruction,
            address,
            access: AccessKind::Read,
        })?;
        if Self::canonical_address(address) && Self::canonical_address(end) {
            Ok(())
        } else {
            Err(ExecutionExit::NonCanonical {
                instruction,
                address,
                access: AccessKind::Read,
            })
        }
    }

    const fn canonical_address(value: u64) -> bool {
        let upper = value >> 48;
        upper == 0 || upper == 0xffff
    }
}
