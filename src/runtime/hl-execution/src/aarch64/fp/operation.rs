use super::interpreter::Aarch64FpExecutor;
use crate::{
    Aarch64CpuState, FPSR_INPUT_DENORMAL, FPSR_INVALID, FpArithmetic, FpArithmeticPort, FpFormat, FpRequest, Nzcv,
};

impl Aarch64FpExecutor {
    pub(super) fn fused<P: FpArithmeticPort>(
        cpu: &Aarch64CpuState,
        staged: &mut Aarch64CpuState,
        port: &mut P,
        format: FpFormat,
        lanes: u8,
        subtract: bool,
        left: u8,
        right: u8,
        index: Option<u8>,
        destination: u8,
    ) {
        let mut value = 0_u128;
        for lane in 0..lanes {
            let mut left_value = cpu.vector_lane(left, format.bits(), lane);
            if subtract {
                left_value ^= 1_u64 << (format.bits() - 1);
            }
            let mut result = port.evaluate(FpRequest {
                operation: FpArithmetic::FusedMultiplyAdd,
                format,
                left: left_value,
                right: cpu.vector_lane(right, format.bits(), index.unwrap_or(lane)),
                addend: cpu.vector_lane(destination, format.bits(), lane),
                fpcr: cpu.fpcr as u32,
            });
            result = Self::half_fused_adjust(
                result,
                format,
                left_value,
                cpu.vector_lane(right, format.bits(), index.unwrap_or(lane)),
                cpu.vector_lane(destination, format.bits(), lane),
                cpu.fpcr,
            );
            staged.fpsr |= u64::from(result.exceptions);
            value |= u128::from(result.value) << (u32::from(lane) * u32::from(format.bits()));
        }
        staged.set_vector(destination, value);
    }

    pub(super) fn half_fused_adjust(
        mut result: crate::FpResult,
        format: FpFormat,
        left: u64,
        right: u64,
        addend: u64,
        fpcr: u64,
    ) -> crate::FpResult {
        if format != FpFormat::Half || addend & 0x7e00 != 0x7e00 {
            return result;
        }
        let zero_infinite =
            (left & 0x7fff == 0 && right & 0x7fff == 0x7c00) || (right & 0x7fff == 0 && left & 0x7fff == 0x7c00);
        if !zero_infinite {
            return result;
        }
        if fpcr & 2 == 0 {
            result.value = 0x7e00;
            result.exceptions |= crate::FPSR_INVALID;
        } else {
            result.value = if fpcr & 1 << 25 != 0 { 0x7e00 } else { addend | 0x0200 };
            result.exceptions &= !crate::FPSR_INVALID;
        }
        result
    }

    pub(super) fn product<P: FpArithmeticPort>(
        cpu: &Aarch64CpuState,
        staged: &mut Aarch64CpuState,
        port: &mut P,
        format: FpFormat,
        lanes: u8,
        extended: bool,
        left: u8,
        right: u8,
        index: Option<u8>,
        destination: u8,
    ) {
        let operation = if extended {
            crate::FpBinaryOperation::MultiplyExtended
        } else {
            crate::FpBinaryOperation::Multiply
        };
        let mut value = 0_u128;
        for lane in 0..lanes {
            let result = port.evaluate(Self::request(
                cpu,
                FpArithmetic::Binary(operation),
                format,
                cpu.vector_lane(left, format.bits(), lane),
                cpu.vector_lane(right, format.bits(), index.unwrap_or(lane)),
            ));
            staged.fpsr |= u64::from(result.exceptions);
            value |= u128::from(result.value) << (u32::from(lane) * u32::from(format.bits()));
        }
        staged.set_vector(destination, value);
    }

    pub(super) fn apply_conversion(
        cpu: &mut Aarch64CpuState,
        result: crate::FpResult,
        fp_to_integer: bool,
        integer_wide: bool,
        destination: u8,
    ) {
        cpu.fpsr |= u64::from(result.exceptions);
        match (fp_to_integer, integer_wide) {
            (true, true) => cpu.set_register(destination, result.value),
            (true, false) => cpu.set_narrow_register(destination, result.value as u32),
            (false, _) => cpu.set_vector(destination, u128::from(result.value)),
        }
    }

    pub(super) fn request(
        cpu: &Aarch64CpuState,
        operation: FpArithmetic,
        format: FpFormat,
        left: u64,
        right: u64,
    ) -> FpRequest {
        FpRequest {
            operation,
            format,
            left,
            right,
            addend: 0,
            fpcr: cpu.fpcr as u32,
        }
    }

    pub(super) fn comparison_operand(cpu: &Aarch64CpuState, register: Option<u8>, format: FpFormat) -> u64 {
        register.map_or(0, |register| cpu.vector_lane(register, format.bits(), 0))
    }

    pub(crate) fn compare(cpu: &mut Aarch64CpuState, format: FpFormat, left: u64, right: u64, signaling: bool) {
        let left_class = Self::classify(left, format);
        let right_class = Self::classify(right, format);
        if left_class >= 4 || right_class >= 4 {
            if signaling || left_class == 5 || right_class == 5 {
                cpu.fpsr |= u64::from(FPSR_INVALID);
            }
            cpu.nzcv = Nzcv::from_bits(Nzcv::CARRY | Nzcv::OVERFLOW);
            return;
        }
        let flush = Self::flushes(cpu.fpcr as u32, format);
        if flush && (left_class == 1 || right_class == 1) && format != FpFormat::Half {
            cpu.fpsr |= u64::from(FPSR_INPUT_DENORMAL);
        }
        let left = if flush && left_class == 1 {
            left & Self::sign_mask(format)
        } else {
            left
        };
        let right = if flush && right_class == 1 {
            right & Self::sign_mask(format)
        } else {
            right
        };
        let both_zero = left & !Self::sign_mask(format) == 0 && right & !Self::sign_mask(format) == 0;
        cpu.nzcv = if both_zero || left == right {
            Nzcv::from_bits(Nzcv::ZERO | Nzcv::CARRY)
        } else if Self::ordered_key(left, format) < Self::ordered_key(right, format) {
            Nzcv::from_bits(Nzcv::NEGATIVE)
        } else {
            Nzcv::from_bits(Nzcv::CARRY)
        };
    }

    pub(super) fn ordered_key(bits: u64, format: FpFormat) -> u64 {
        let bits = bits & Self::value_mask(format);
        let sign = Self::sign_mask(format);
        if bits & sign != 0 {
            !bits & Self::value_mask(format)
        } else {
            bits | sign
        }
    }

    pub(super) fn classify(bits: u64, format: FpFormat) -> u8 {
        let (mantissa, exponent_bits) = match format {
            FpFormat::Half => (10, 5),
            FpFormat::Single => (23, 8),
            FpFormat::Double => (52, 11),
        };
        let fraction_mask = (1_u64 << mantissa) - 1;
        let exponent = bits >> mantissa & ((1_u64 << exponent_bits) - 1);
        let fraction = bits & fraction_mask;
        if exponent == 0 {
            u8::from(fraction != 0)
        } else if exponent != (1_u64 << exponent_bits) - 1 {
            2
        } else if fraction == 0 {
            3
        } else if fraction >> (mantissa - 1) & 1 != 0 {
            4
        } else {
            5
        }
    }

    pub(super) fn flushes(fpcr: u32, format: FpFormat) -> bool {
        fpcr & if format == FpFormat::Half { 1 << 19 } else { 1 << 24 } != 0
    }

    pub(super) fn sign_mask(format: FpFormat) -> u64 {
        1_u64 << (format.bits() - 1)
    }

    pub(super) fn value_mask(format: FpFormat) -> u64 {
        if format == FpFormat::Double {
            u64::MAX
        } else {
            (1_u64 << format.bits()) - 1
        }
    }

    pub(super) fn condition_holds(flags: Nzcv, condition: u8) -> bool {
        let result = match condition >> 1 {
            0 => flags.zero(),
            1 => flags.carry(),
            2 => flags.negative(),
            3 => flags.overflow(),
            4 => flags.carry() && !flags.zero(),
            5 => flags.negative() == flags.overflow(),
            6 => !flags.zero() && flags.negative() == flags.overflow(),
            _ => true,
        };
        result ^ (condition & 1 != 0 && condition != 15)
    }
}
