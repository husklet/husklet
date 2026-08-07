use crate::x86::real::Conversion;
use crate::{
    AccessKind, CpuState, EffectiveAddress, ExecutionExit, ExtendedClass, ExtendedReal, FloatWidth, GuestOperandMemory,
};

use super::memory::ExtendedMemory;

impl ExtendedMemory {
    pub(crate) fn integer<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        bytes: u8,
        load: bool,
        pop: bool,
        truncate: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let access = if load { AccessKind::Read } else { AccessKind::Write };
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(u64::from(bytes) - 1)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            };
        }
        if load {
            let Ok(bits) = memory.read(address, bytes) else { return Self::fault(instruction, address, access, bytes) };
            let signed = match bytes {
                2 => i64::from(bits as i16),
                4 => i64::from(bits as i32),
                8 => bits as i64,
                _ => return ExecutionExit::UndefinedInstruction { instruction },
            };
            let (value, class) = Conversion::expand((signed as f64).to_bits(), FloatWidth::Double);
            let mut staged = cpu.clone();
            let destination = usize::from(((staged.x87_status >> 11) as u8).wrapping_sub(1) & 7);
            if staged.x87_classes[destination] != ExtendedClass::Empty {
                return Self::stack_fault(cpu, staged, destination, instruction, next, true);
            }
            staged.x87_status = (staged.x87_status & !0x3a00) | (destination as u16) << 11;
            staged.x87_values[destination] = value;
            staged.x87_classes[destination] = class;
            staged.rip = next;
            *cpu = staged;
            return ExecutionExit::Continue;
        }
        let Ok(reservation) = memory.reserve_write(address, bytes) else {
            return Self::fault(instruction, address, access, bytes)
        };
        let top = usize::from((cpu.x87_status >> 11) & 7);
        let mut staged = cpu.clone();
        let empty = staged.x87_classes[top] == ExtendedClass::Empty;
        let mut invalid = empty;
        let mut rounded_up = false;
        let stored = if empty {
            Self::raise_stack(&mut staged, false);
            1_u64 << (u32::from(bytes) * 8 - 1)
        } else {
            let value = f64::from_bits(
                Conversion::narrow(staged.x87_values[top], staged.x87_classes[top], FloatWidth::Double, 0).bits,
            );
            let rounded = if truncate {
                value.trunc()
            } else {
                match (staged.x87_control >> 10) & 3 {
                    0 => value.round_ties_even(),
                    1 => value.floor(),
                    2 => value.ceil(),
                    _ => value.trunc(),
                }
            };
            rounded_up = !rounded.is_nan() && !value.is_nan() && rounded.abs() > value.abs();
            let (minimum, maximum) = match bytes {
                2 => (i16::MIN as f64, i16::MAX as f64),
                4 => (i32::MIN as f64, i32::MAX as f64),
                8 => (i64::MIN as f64, i64::MAX as f64),
                _ => return ExecutionExit::UndefinedInstruction { instruction },
            };
            if !rounded.is_finite() || rounded < minimum || rounded > maximum {
                invalid = true;
                1_u64 << (u32::from(bytes) * 8 - 1)
            } else {
                rounded as i64 as u64
            }
        };
        if invalid && Self::raise(&mut staged, 1) {
            *cpu = staged;
            return ExecutionExit::UndefinedInstruction { instruction };
        }
        if memory.commit_write(reservation, stored).is_err() {
            return Self::fault(instruction, address, access, bytes);
        }
        staged.x87_status = (staged.x87_status & !(1 << 9)) | u16::from(rounded_up) << 9;
        if pop {
            staged.x87_classes[top] = ExtendedClass::Empty;
            staged.x87_status = (staged.x87_status & !0x3800) | (((top + 1) & 7) as u16) << 11;
        }
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn status_compare<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        effective: Option<EffectiveAddress>,
        source: u8,
        pop: u8,
        format: FloatWidth,
        ordered: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let top = usize::from((cpu.x87_status >> 11) & 7);
        let (right, right_class) = if let Some(effective) = effective {
            let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
            let bytes = if format == FloatWidth::Single { 4 } else { 8 };
            let Ok(bits) = memory.read(address, bytes as u8) else {
                return Self::fault(instruction, address, AccessKind::Read, bytes as u8)
            };
            Conversion::expand(bits, format)
        } else {
            let index = (top + usize::from(source)) & 7;
            (cpu.x87_values[index], cpu.x87_classes[index])
        };
        let left_class = cpu.x87_classes[top];
        let stack_fault = left_class == ExtendedClass::Empty || right_class == ExtendedClass::Empty;
        let invalid =
            stack_fault || Self::invalid_compare(left_class, ordered) || Self::invalid_compare(right_class, ordered);
        let mut staged = cpu.clone();
        let relation = if invalid {
            if stack_fault {
                staged.x87_status |= 1 << 6;
            }
            if Self::raise(&mut staged, 1) {
                *cpu = staged;
                return ExecutionExit::UndefinedInstruction { instruction };
            }
            None
        } else {
            Self::relation(cpu.x87_values[top], left_class, right, right_class)
        };
        staged.x87_status &= !((1 << 14) | (1 << 10) | (1 << 9) | (1 << 8));
        if relation.is_none() || relation == Some(std::cmp::Ordering::Equal) {
            staged.x87_status |= 1 << 14;
        }
        if relation.is_none() {
            staged.x87_status |= 1 << 10;
        }
        if relation.is_none() || relation == Some(std::cmp::Ordering::Less) {
            staged.x87_status |= 1 << 8;
        }
        for _ in 0..pop {
            let current = usize::from((staged.x87_status >> 11) & 7);
            staged.x87_classes[current] = ExtendedClass::Empty;
            staged.x87_status = (staged.x87_status & !0x3800) | (((current + 1) & 7) as u16) << 11;
        }
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn arithmetic<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        effective: Option<EffectiveAddress>,
        source: u8,
        mut operation: u8,
        destination_source: bool,
        pop: bool,
        format: FloatWidth,
        integer_bytes: u8,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let top = usize::from((cpu.x87_status >> 11) & 7);
        let source_index = (top + usize::from(source)) & 7;
        let (right, right_class) = if let Some(effective) = effective {
            let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
            let bytes = if integer_bytes != 0 {
                u64::from(integer_bytes)
            } else if format == FloatWidth::Single {
                4
            } else {
                8
            };
            if !Self::canonical(address) || !Self::canonical(address.wrapping_add(bytes - 1)) {
                return ExecutionExit::NonCanonical {
                    instruction,
                    address,
                    access: AccessKind::Read,
                };
            }
            let Ok(bits) = memory.read(address, bytes as u8) else {
                return Self::fault(instruction, address, AccessKind::Read, bytes as u8)
            };
            if integer_bytes != 0 {
                let signed = if integer_bytes == 2 {
                    i64::from(bits as i16)
                } else {
                    i64::from(bits as i32)
                };
                Conversion::expand((signed as f64).to_bits(), FloatWidth::Double)
            } else {
                Conversion::expand(bits, format)
            }
        } else {
            (
                cpu.x87_values[if destination_source { top } else { source_index }],
                cpu.x87_classes[if destination_source { top } else { source_index }],
            )
        };
        let destination = if destination_source { source_index } else { top };
        let left = cpu.x87_values[destination];
        let left_class = cpu.x87_classes[destination];
        let mut staged = cpu.clone();
        staged.x87_status &= !(1 << 9);
        if left_class == ExtendedClass::Empty || right_class == ExtendedClass::Empty {
            if Self::raise_stack(&mut staged, false) {
                *cpu = staged;
                return ExecutionExit::UndefinedInstruction { instruction };
            }
            staged.x87_values[destination] = ExtendedReal::INDEFINITE;
            staged.x87_classes[destination] = ExtendedClass::QuietNan;
        } else {
            if destination_source && matches!(operation, 4..=7) {
                operation ^= 1;
            }
            let left_value = left;
            let right_value = right;
            let left = f64::from_bits(Conversion::narrow(left_value, left_class, FloatWidth::Double, 0).bits);
            let right = f64::from_bits(Conversion::narrow(right_value, right_class, FloatWidth::Double, 0).bits);
            let mut environment = hl_softfloat::Environment::default();
            environment.rounding = match cpu.x87_control >> 10 & 3 {
                0 => hl_softfloat::RoundingMode::NearestEven,
                1 => hl_softfloat::RoundingMode::TowardNegative,
                2 => hl_softfloat::RoundingMode::TowardPositive,
                _ => hl_softfloat::RoundingMode::TowardZero,
            };
            let left_soft = hl_softfloat::Value::from_bits(hl_softfloat::Format::Binary64, left.to_bits());
            let right_soft = hl_softfloat::Value::from_bits(hl_softfloat::Format::Binary64, right.to_bits());
            let soft = match operation {
                0 => environment.add(left_soft, right_soft),
                1 => environment.multiply(left_soft, right_soft),
                4 => environment.subtract(left_soft, right_soft),
                5 => environment.subtract(right_soft, left_soft),
                6 => environment.divide(left_soft, right_soft),
                7 => environment.divide(right_soft, left_soft),
                _ => return ExecutionExit::UndefinedInstruction { instruction },
            };
            let result_bits = soft.value.bits();
            let mut soft_flags = crate::x86::scalar::arithmetic::Arithmetic::exceptions(soft.flags) as u16;
            let (result_bits, precision) = Self::precision(result_bits, cpu.x87_control);
            soft_flags |= precision;
            let result = f64::from_bits(result_bits);
            let invalid = matches!(operation, 0 | 4 | 5)
                && result.is_nan()
                && left_class != ExtendedClass::QuietNan
                && right_class != ExtendedClass::QuietNan
                || operation == 1 && ((left == 0.0 && right.is_infinite()) || (right == 0.0 && left.is_infinite()))
                || matches!(operation, 6 | 7)
                    && ((left == 0.0 && right == 0.0) || (left.is_infinite() && right.is_infinite()))
                || left_class == ExtendedClass::SignalingNan
                || right_class == ExtendedClass::SignalingNan;
            let divisor = if operation == 6 { right } else { left };
            let dividend = if operation == 6 { left } else { right };
            let divide_zero = matches!(operation, 6 | 7) && divisor == 0.0 && dividend != 0.0 && dividend.is_finite();
            let inexact = matches!(operation, 6 | 7)
                && !invalid
                && !divide_zero
                && result.is_finite()
                && Self::division_inexact(
                    if operation == 6 { left_value } else { right_value },
                    if operation == 6 { right_value } else { left_value },
                );
            let flags = soft_flags
                | u16::from(invalid)
                | u16::from(left_class == ExtendedClass::Denormal || right_class == ExtendedClass::Denormal) << 1
                | u16::from(divide_zero) << 2
                | u16::from(inexact) << 5;
            if flags != 0 && Self::raise(&mut staged, flags) {
                staged.rip = next;
                *cpu = staged;
                return ExecutionExit::Continue;
            }
            let (value, class) = Self::computed(result.to_bits(), &[left_class, right_class]);
            staged.x87_values[destination] = value;
            staged.x87_classes[destination] = class;
        }
        if pop {
            staged.x87_classes[top] = ExtendedClass::Empty;
            staged.x87_status = (staged.x87_status & !0x3800) | (((top + 1) & 7) as u16) << 11;
        }
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn compare(
        cpu: &mut CpuState,
        source: u8,
        ordered: bool,
        pop: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let top = usize::from((cpu.x87_status >> 11) & 7);
        let right = (top + usize::from(source)) & 7;
        let left_class = cpu.x87_classes[top];
        let right_class = cpu.x87_classes[right];
        let stack_fault = left_class == ExtendedClass::Empty || right_class == ExtendedClass::Empty;
        let invalid =
            stack_fault || Self::invalid_compare(left_class, ordered) || Self::invalid_compare(right_class, ordered);
        let mut staged = cpu.clone();
        let relation = if invalid {
            if stack_fault {
                staged.x87_status |= 1 << 6;
                staged.x87_status &= !(1 << 9);
            }
            if Self::raise(&mut staged, 1) {
                *cpu = staged;
                return ExecutionExit::UndefinedInstruction { instruction };
            }
            None
        } else {
            Self::relation(cpu.x87_values[top], left_class, cpu.x87_values[right], right_class)
        };
        staged.flags = staged
            .flags
            .with(
                crate::Flag::Zero,
                relation.is_none() || relation == Some(std::cmp::Ordering::Equal),
            )
            .with(crate::Flag::Parity, relation.is_none())
            .with(
                crate::Flag::Carry,
                relation.is_none() || relation == Some(std::cmp::Ordering::Less),
            )
            .with(crate::Flag::Overflow, false)
            .with(crate::Flag::Sign, false)
            .with(crate::Flag::Auxiliary, false);
        if pop {
            staged.x87_classes[top] = ExtendedClass::Empty;
            staged.x87_status = (staged.x87_status & !0x3800) | (((top + 1) & 7) as u16) << 11;
        }
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn conditional_move(
        cpu: &mut CpuState,
        source: u8,
        condition: u8,
        negate: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let selected = match condition {
            0 => cpu.flags.contains(crate::Flag::Carry),
            1 => cpu.flags.contains(crate::Flag::Zero),
            2 => cpu.flags.contains(crate::Flag::Carry) || cpu.flags.contains(crate::Flag::Zero),
            3 => cpu.flags.contains(crate::Flag::Parity),
            _ => return ExecutionExit::UndefinedInstruction { instruction },
        };
        let top = usize::from((cpu.x87_status >> 11) & 7);
        let source = (top + usize::from(source)) & 7;
        let mut staged = cpu.clone();
        if selected != negate {
            staged.x87_values[top] = staged.x87_values[source];
            staged.x87_classes[top] = staged.x87_classes[source];
        }
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }
}
