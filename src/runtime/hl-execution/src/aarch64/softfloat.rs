use crate::{
    FPSR_DIVIDE_BY_ZERO, FPSR_INEXACT, FPSR_INPUT_DENORMAL, FPSR_INVALID, FPSR_OVERFLOW, FPSR_UNDERFLOW, FpArithmetic,
    FpArithmeticPort, FpBinaryOperation, FpFormat, FpRequest, FpResult, FpRoundingMode,
};
use hl_softfloat::{Environment, ExceptionFlags, Format, NaNMode, RoundingMode, TininessMode, Value};

/// Deterministic software implementation of the `AArch64` arithmetic port.
#[derive(Default)]
pub struct SoftFloat;
pub use SoftFloat as Aarch64SoftFloat;

impl FpArithmeticPort for Aarch64SoftFloat {
    fn evaluate(&mut self, request: FpRequest) -> FpResult {
        let mut environment = Self::environment(request.fpcr, request.format);
        let format = Self::format(request.format);
        let left = Value::from_bits(format, request.left);
        let result = match request.operation {
            FpArithmetic::Binary(operation) => {
                let right = Value::from_bits(format, request.right);
                match operation {
                    FpBinaryOperation::Add => environment.add(left, right),
                    FpBinaryOperation::Subtract => environment.subtract(left, right),
                    FpBinaryOperation::AbsoluteDifference => {
                        let mut result = environment.subtract(left, right);
                        let sign = 1_u64 << (request.format.bits() - 1);
                        result.value = Value::from_bits(format, result.value.bits() & !sign);
                        result
                    }
                    FpBinaryOperation::Multiply => environment.multiply(left, right),
                    FpBinaryOperation::MultiplyExtended => environment.multiply_extended(left, right),
                    FpBinaryOperation::Divide => environment.divide(left, right),
                    FpBinaryOperation::Minimum => environment.minimum(left, right),
                    FpBinaryOperation::Maximum => environment.maximum(left, right),
                    FpBinaryOperation::MinimumNumber => environment.minimum_number(left, right),
                    FpBinaryOperation::MaximumNumber => environment.maximum_number(left, right),
                }
            }
            FpArithmetic::FusedMultiplyAdd => environment.fused_multiply_add(
                left,
                Value::from_bits(format, request.right),
                Value::from_bits(format, request.addend),
            ),
            FpArithmetic::SquareRoot => environment.square_root(left),
            FpArithmetic::RoundToIntegral { rounding, exact } => {
                environment.rounding = Self::rounding_mode(rounding, request.fpcr);
                environment.round_to_integral(left, exact)
            }
            FpArithmetic::ConvertFormat { destination } => {
                let converted = environment.convert(left, Self::format(destination));
                let mut value = converted.value.bits();
                let mut exceptions = Self::exceptions(converted.flags, request.format);
                let source_mantissa = request.format.bits()
                    - match request.format {
                        FpFormat::Half => 6,
                        FpFormat::Single => 9,
                        FpFormat::Double => 12,
                    };
                let source_fraction = (1_u64 << source_mantissa) - 1;
                let source_exponent = ((1_u64 << (request.format.bits() - source_mantissa - 1)) - 1) << source_mantissa;
                if request.fpcr & 1 << 25 == 0
                    && request.left & source_exponent == source_exponent
                    && request.left & source_fraction != 0
                {
                    let destination_sign = 1_u64 << (destination.bits() - 1);
                    value = value & !destination_sign | (u64::from(request.left >> (request.format.bits() - 1) != 0) * destination_sign);
                }
                let destination_mantissa = destination.bits()
                    - match destination {
                        FpFormat::Half => 6,
                        FpFormat::Single => 9,
                        FpFormat::Double => 12,
                    };
                let destination_fraction = (1_u64 << destination_mantissa) - 1;
                let flush = request.fpcr
                    & if destination == FpFormat::Half {
                        1 << 19
                    } else {
                        1 << 24
                    }
                    != 0;
                if flush
                    && value >> destination_mantissa & ((1_u64 << (destination.bits() - destination_mantissa - 1)) - 1)
                        == 0
                    && (value & destination_fraction != 0 || exceptions & FPSR_UNDERFLOW != 0)
                {
                    value &= 1_u64 << (destination.bits() - 1);
                    exceptions = exceptions & !FPSR_INEXACT | FPSR_UNDERFLOW;
                }
                return FpResult { value, exceptions };
            }
            FpArithmetic::IntegerToFloat { signed, width } => {
                let value = Self::mask_integer(request.left, width);
                if signed {
                    environment.from_signed(format, Self::signed_integer(value, width))
                } else {
                    environment.from_unsigned(format, value)
                }
            }
            FpArithmetic::FloatToInteger {
                signed,
                width,
                rounding,
            } => {
                environment.rounding = Self::rounding_mode(rounding, request.fpcr);
                let converted = if signed {
                    environment.to_signed(left, width)
                } else {
                    environment.to_unsigned(left, width)
                };
                return FpResult {
                    value: converted.value,
                    exceptions: Self::exceptions(converted.flags, request.format),
                };
            }
            FpArithmetic::FloatToScaled {
                signed,
                width,
                scale,
                rounding,
            } => {
                environment.rounding = Self::rounding_mode(rounding, request.fpcr);
                let converted = if signed {
                    environment.to_signed_scaled(left, width, scale)
                } else {
                    environment.to_unsigned_scaled(left, width, scale)
                };
                return FpResult {
                    value: converted.value,
                    exceptions: Self::exceptions(converted.flags, request.format),
                };
            }
        };
        FpResult {
            value: result.value.bits(),
            exceptions: Self::exceptions(result.flags, request.format),
        }
    }
}

impl Aarch64SoftFloat {
    fn environment(fpcr: u32, fp_format: FpFormat) -> Environment {
        let flush = fpcr & if fp_format == FpFormat::Half { 1 << 19 } else { 1 << 24 } != 0;
        Environment {
            rounding: Self::rounding_mode(FpRoundingMode::Current, fpcr),
            tininess: TininessMode::BeforeRounding,
            nan: if fpcr & 1 << 25 != 0 {
                NaNMode::Default
            } else {
                NaNMode::PropagatePayload
            },
            flush_inputs: flush,
            flush_outputs: flush,
        }
    }

    fn format(format: FpFormat) -> Format {
        match format {
            FpFormat::Half => Format::Binary16,
            FpFormat::Single => Format::Binary32,
            FpFormat::Double => Format::Binary64,
        }
    }

    fn rounding_mode(mode: FpRoundingMode, fpcr: u32) -> RoundingMode {
        match mode {
            FpRoundingMode::NearestEven => RoundingMode::NearestEven,
            FpRoundingMode::PositiveInfinity => RoundingMode::TowardPositive,
            FpRoundingMode::NegativeInfinity => RoundingMode::TowardNegative,
            FpRoundingMode::Zero => RoundingMode::TowardZero,
            FpRoundingMode::NearestAway => RoundingMode::NearestAway,
            FpRoundingMode::Current => match fpcr >> 22 & 3 {
                0 => RoundingMode::NearestEven,
                1 => RoundingMode::TowardPositive,
                2 => RoundingMode::TowardNegative,
                _ => RoundingMode::TowardZero,
            },
        }
    }

    fn exceptions(flags: ExceptionFlags, format: FpFormat) -> u32 {
        let mut result = 0;
        for (soft, architectural) in [
            (ExceptionFlags::INVALID, FPSR_INVALID),
            (ExceptionFlags::DIVIDE_BY_ZERO, FPSR_DIVIDE_BY_ZERO),
            (ExceptionFlags::OVERFLOW, FPSR_OVERFLOW),
            (ExceptionFlags::UNDERFLOW, FPSR_UNDERFLOW),
            (ExceptionFlags::INEXACT, FPSR_INEXACT),
        ] {
            if flags.contains(soft) {
                result |= architectural;
            }
        }
        if format != FpFormat::Half && flags.contains(ExceptionFlags::INPUT_DENORMAL) {
            result |= FPSR_INPUT_DENORMAL;
        }
        result
    }

    fn mask_integer(value: u64, width: u8) -> u64 {
        if width == 32 { u64::from(value as u32) } else { value }
    }

    fn signed_integer(value: u64, width: u8) -> i64 {
        if width == 32 {
            i64::from(value as u32 as i32)
        } else {
            value as i64
        }
    }
}
