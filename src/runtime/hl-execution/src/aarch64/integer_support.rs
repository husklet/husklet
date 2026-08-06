use crate::{Aarch64CpuState, Aarch64Shift, BitfieldOperation, LogicalOperation, MultiplyOperation};

pub(crate) fn multiply(
    cpu: &Aarch64CpuState,
    operation: MultiplyOperation,
    subtract: bool,
    source: u8,
    operand: u8,
    addend: u8,
) -> u64 {
    let left = cpu.register(source);
    let right = cpu.register(operand);
    let product = match operation {
        MultiplyOperation::Add => left.wrapping_mul(right),
        MultiplyOperation::SignedLong => (left as i32 as i64).wrapping_mul(right as i32 as i64) as u64,
        MultiplyOperation::UnsignedLong => u64::from(left as u32) * u64::from(right as u32),
        MultiplyOperation::SignedHigh => (((left as i64 as i128) * (right as i64 as i128)) >> 64) as u64,
        MultiplyOperation::UnsignedHigh => ((u128::from(left) * u128::from(right)) >> 64) as u64,
    };
    if matches!(
        operation,
        MultiplyOperation::SignedHigh | MultiplyOperation::UnsignedHigh
    ) {
        product
    } else if subtract {
        cpu.register(addend).wrapping_sub(product)
    } else {
        cpu.register(addend).wrapping_add(product)
    }
}

pub(crate) fn reverse_bytes(value: u64, width: u8, container: u8) -> u64 {
    let mut result = 0_u64;
    for base in (0..width).step_by(usize::from(container)) {
        for byte in 0..container {
            let source = base + container - 1 - byte;
            result |= (value >> (source * 8) & 0xff) << ((base + byte) * 8);
        }
    }
    result
}

pub(crate) fn logical_operand(value: u64, shift: Aarch64Shift, amount: u8, wide: bool, invert: bool) -> u64 {
    let value = shifted(value, shift, amount, wide);
    match (invert, wide) {
        (false, _) => value,
        (true, true) => !value,
        (true, false) => u64::from(!(value as u32)),
    }
}

pub(crate) fn select_value(
    cpu: &Aarch64CpuState,
    source: u8,
    alternate: u8,
    holds: bool,
    invert: bool,
    increment: bool,
) -> u64 {
    if holds {
        return cpu.register(source);
    }
    let value = cpu.register(alternate);
    let value = if invert { !value } else { value };
    if increment { value.wrapping_add(1) } else { value }
}

pub(crate) fn bitfield(
    cpu: &Aarch64CpuState,
    wide: bool,
    operation: BitfieldOperation,
    source: u8,
    destination: u8,
    rotate: u8,
    sign_bit: u8,
    write_mask: u64,
    top_mask: u64,
) -> u64 {
    let source_value = cpu.register(source);
    let rotated = if wide {
        source_value.rotate_right(u32::from(rotate))
    } else {
        u64::from((source_value as u32).rotate_right(u32::from(rotate)))
    };
    let bottom = match operation {
        BitfieldOperation::Insert => cpu.register(destination) & !write_mask | rotated & write_mask,
        BitfieldOperation::Signed | BitfieldOperation::Unsigned => rotated & write_mask,
    };
    let top = match operation {
        BitfieldOperation::Insert => cpu.register(destination),
        BitfieldOperation::Signed if source_value >> sign_bit & 1 != 0 => u64::MAX,
        BitfieldOperation::Signed | BitfieldOperation::Unsigned => 0,
    };
    top & !top_mask | bottom & top_mask
}

pub(crate) fn write_register(cpu: &mut Aarch64CpuState, wide: bool, register: u8, value: u64) {
    if wide {
        cpu.set_register(register, value);
    } else {
        cpu.set_narrow_register(register, value as u32);
    }
}

pub(crate) fn write_destination(cpu: &mut Aarch64CpuState, wide: bool, register: u8, value: u64) {
    if wide {
        cpu.set_destination(register, value);
    } else {
        cpu.set_narrow_destination(register, value as u32);
    }
}

pub(crate) fn shifted(value: u64, shift: Aarch64Shift, amount: u8, wide: bool) -> u64 {
    if wide {
        match shift {
            Aarch64Shift::Lsl => value << amount,
            Aarch64Shift::Lsr => value >> amount,
            Aarch64Shift::Asr => ((value as i64) >> amount) as u64,
            Aarch64Shift::Ror => value.rotate_right(u32::from(amount)),
        }
    } else {
        let value = value as u32;
        u64::from(match shift {
            Aarch64Shift::Lsl => value << amount,
            Aarch64Shift::Lsr => value >> amount,
            Aarch64Shift::Asr => ((value as i32) >> amount) as u32,
            Aarch64Shift::Ror => value.rotate_right(u32::from(amount)),
        })
    }
}

pub(crate) fn logical(operation: LogicalOperation, left: u64, right: u64, wide: bool) -> u64 {
    let result = match operation {
        LogicalOperation::And | LogicalOperation::Ands => left & right,
        LogicalOperation::Orr => left | right,
        LogicalOperation::Eor => left ^ right,
    };
    if wide { result } else { u64::from(result as u32) }
}

pub(crate) fn logical_flags(cpu: &mut Aarch64CpuState, result: u64, wide: bool) {
    let negative = if wide { result >> 63 != 0 } else { result >> 31 & 1 != 0 };
    let zero = if wide { result == 0 } else { result as u32 == 0 };
    cpu.nzcv.set(negative, zero, false, false);
}

pub(crate) fn arithmetic(
    cpu: &mut Aarch64CpuState,
    wide: bool,
    left: u64,
    right: u64,
    subtract: bool,
    set_flags: bool,
) -> u64 {
    if wide {
        let (result, carry, overflow) = if subtract {
            let (result, borrow) = left.overflowing_sub(right);
            (result, !borrow, (left as i64).overflowing_sub(right as i64).1)
        } else {
            let (result, carry) = left.overflowing_add(right);
            (result, carry, (left as i64).overflowing_add(right as i64).1)
        };
        if set_flags {
            cpu.nzcv.set(result >> 63 != 0, result == 0, carry, overflow);
        }
        result
    } else {
        let left = left as u32;
        let right = right as u32;
        let (result, carry, overflow) = if subtract {
            let (result, borrow) = left.overflowing_sub(right);
            (result, !borrow, (left as i32).overflowing_sub(right as i32).1)
        } else {
            let (result, carry) = left.overflowing_add(right);
            (result, carry, (left as i32).overflowing_add(right as i32).1)
        };
        if set_flags {
            cpu.nzcv.set(result >> 31 != 0, result == 0, carry, overflow);
        }
        u64::from(result)
    }
}

pub(crate) fn add_carry(
    cpu: &mut Aarch64CpuState,
    wide: bool,
    left: u64,
    right: u64,
    carry: bool,
    subtract: bool,
    set_flags: bool,
) -> u64 {
    let bits = if wide { 64 } else { 32 };
    let mask = if wide { u64::MAX } else { u64::from(u32::MAX) };
    let left = left & mask;
    let operand = if subtract { !right & mask } else { right & mask };
    let unsigned = u128::from(left) + u128::from(operand) + u128::from(carry);
    let result = unsigned as u64 & mask;
    if set_flags {
        let sign = 1_u64 << (bits - 1);
        let overflow = (!(left ^ operand) & (left ^ result) & sign) != 0;
        cpu.nzcv
            .set(result & sign != 0, result == 0, unsigned >> bits != 0, overflow);
    }
    result
}
