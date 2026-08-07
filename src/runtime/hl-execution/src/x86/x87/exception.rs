use crate::x86::real::Conversion;
use crate::{AccessKind, CpuState, ExecutionExit, ExtendedClass, ExtendedReal, FloatWidth};

use super::memory::ExtendedMemory;

impl ExtendedMemory {
    pub(crate) fn stack_fault(
        cpu: &mut CpuState,
        mut staged: CpuState,
        index: usize,
        instruction: u64,
        next: u64,
        overflow: bool,
    ) -> ExecutionExit {
        Self::raise_stack(&mut staged, overflow);
        if staged.x87_control & 1 == 0 {
            *cpu = staged;
            return ExecutionExit::UndefinedInstruction { instruction };
        }
        staged.x87_status = (staged.x87_status & !0x3800) | (index as u16) << 11;
        staged.x87_values[index] = ExtendedReal::INDEFINITE;
        staged.x87_classes[index] = ExtendedClass::QuietNan;
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn raise_stack(cpu: &mut CpuState, overflow: bool) -> bool {
        cpu.x87_status |= 1 << 6;
        if overflow {
            cpu.x87_status |= 1 << 9;
        } else {
            cpu.x87_status &= !(1 << 9);
        }
        Self::raise(cpu, 1)
    }

    pub(crate) fn raise(cpu: &mut CpuState, flags: u16) -> bool {
        cpu.x87_status |= flags;
        let unmasked = flags & !cpu.x87_control & 0x3f != 0;
        if unmasked {
            cpu.x87_status |= (1 << 7) | (1 << 15);
        }
        unmasked
    }

    pub(crate) fn computed(bits: u64, inputs: &[ExtendedClass]) -> (ExtendedReal, ExtendedClass) {
        let (value, class) = Conversion::expand(bits, FloatWidth::Double);
        let input_nan = inputs
            .iter()
            .any(|class| matches!(class, ExtendedClass::QuietNan | ExtendedClass::SignalingNan));
        if !input_nan && matches!(class, ExtendedClass::QuietNan | ExtendedClass::SignalingNan) {
            (ExtendedReal::INDEFINITE, ExtendedClass::QuietNan)
        } else {
            (value, class)
        }
    }

    pub(crate) fn scale(value: f64, exponent: f64) -> f64 {
        if !value.is_finite() || value == 0.0 {
            return value;
        }
        if exponent.is_nan() {
            return value + exponent;
        }
        let exponent = exponent.clamp(-4096.0, 4096.0);
        let biased = (exponent.trunc() as i64 + 1023).clamp(0, 2047) as u64;
        value * f64::from_bits(biased << 52)
    }

    pub(crate) fn precision(bits: u64, control: u16) -> (u64, u16) {
        if control & 0x0300 != 0 || bits >> 52 & 0x7ff == 0x7ff {
            return (bits, 0);
        }
        let dropped = bits & ((1_u64 << 29) - 1);
        if dropped == 0 {
            return (bits, 0);
        }
        let negative = bits >> 63 != 0;
        let mut kept = bits - dropped;
        let increment = match control >> 10 & 3 {
            0 => dropped > 1_u64 << 28 || dropped == 1_u64 << 28 && kept & (1_u64 << 29) != 0,
            1 => negative,
            2 => !negative,
            _ => false,
        };
        if increment {
            kept += 1_u64 << 29;
        }
        (kept, 1 << 5)
    }

    pub(crate) fn division_inexact(dividend: ExtendedReal, divisor: ExtendedReal) -> bool {
        let mut left = dividend.bits() as u64;
        let mut right = divisor.bits() as u64;
        while right != 0 {
            (left, right) = (right, left % right);
        }
        let reduced_divisor = (divisor.bits() as u64) / left;
        !reduced_divisor.is_power_of_two()
    }

    pub(crate) const fn canonical(address: u64) -> bool {
        (((address << 16) as i64) >> 16) as u64 == address
    }

    pub(crate) const fn fault(instruction: u64, address: u64, access: AccessKind, length: u8) -> ExecutionExit {
        ExecutionExit::OperandFault(crate::FaultAccess::operand(instruction, address, access, length as u64))
    }
}
