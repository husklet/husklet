use crate::{Class, Environment, ExceptionFlags, Format, Result, Value, bits::BitArithmetic};

pub(crate) fn from_integer(environment: Environment, format: Format, magnitude: u64, sign: bool) -> Result<Value> {
    if magnitude == 0 {
        return Result {
            value: Value::from_bits(format, 0),
            flags: ExceptionFlags::default(),
        };
    }
    let top = 63 - magnitude.leading_zeros();
    let exponent = top as i32;
    let wanted = u32::from(format.precision()) + 2;
    let extended = if top > wanted {
        BitArithmetic::shift_right_jam(u128::from(magnitude), top - wanted)
    } else {
        u128::from(magnitude) << (wanted - top)
    };
    environment.round_pack(format, sign, exponent, extended, ExceptionFlags::default())
}

pub(crate) fn to_integer(environment: Environment, value: Value, width: u8, signed: bool, scale: u8) -> Result<u64> {
    assert!(matches!(width, 32 | 64));
    let (operand, mut flags) = environment.unpack(value);
    if operand.class >= Class::QuietNaN {
        flags |= ExceptionFlags::INVALID;
        return Result { value: 0, flags };
    }
    let maximum = if signed {
        if operand.sign {
            1_u128 << (width - 1)
        } else {
            (1_u128 << (width - 1)) - 1
        }
    } else if operand.sign {
        0
    } else if width == 64 {
        u128::from(u64::MAX)
    } else {
        (1_u128 << width) - 1
    };
    if operand.class == Class::Infinity {
        flags |= ExceptionFlags::INVALID;
        return Result {
            value: saturated(operand.sign, signed, width),
            flags,
        };
    }
    if operand.class == Class::Zero {
        return Result { value: 0, flags };
    }
    let shift = operand.exponent + i32::from(scale) - i32::from(value.format().fraction_bits());
    let mut magnitude;
    let discarded;
    if shift >= 0 {
        let distance = shift as u32;
        magnitude = if distance >= 128 || operand.significand > u128::MAX >> distance {
            u128::MAX
        } else {
            operand.significand << distance
        };
        discarded = 0;
    } else {
        let distance = (-shift) as u32;
        let extended = BitArithmetic::shift_right_jam(operand.significand << 3, distance);
        discarded = (extended & 7) as u8;
        magnitude = extended >> 3;
        if BitArithmetic::should_increment(environment.rounding, operand.sign, discarded, magnitude & 1 != 0) {
            magnitude += 1;
        }
    }
    if magnitude > maximum || (!signed && operand.sign && magnitude != 0) {
        flags |= ExceptionFlags::INVALID;
        return Result {
            value: saturated(operand.sign, signed, width),
            flags,
        };
    }
    if discarded != 0 {
        flags |= ExceptionFlags::INEXACT;
    }
    let bits = if operand.sign {
        (0_u128.wrapping_sub(magnitude)) as u64
    } else {
        magnitude as u64
    };
    Result {
        value: if width == 32 { u64::from(bits as u32) } else { bits },
        flags,
    }
}

pub(crate) fn convert(environment: Environment, value: Value, destination: Format) -> Result<Value> {
    let (operand, mut flags) = environment.unpack(value);
    if operand.class >= Class::QuietNaN {
        if operand.class == Class::SignalingNaN {
            flags |= ExceptionFlags::INVALID;
        }
        if environment.nan == crate::NaNMode::Default {
            let bits =
                destination.exponent_mask() << destination.fraction_bits() | 1_u64 << (destination.fraction_bits() - 1);
            return Result {
                value: Value::from_bits(destination, bits),
                flags,
            };
        }
        let quiet = 1_u64 << (destination.fraction_bits() - 1);
        let payload = operand.significand as u64;
        let shifted = if value.format().fraction_bits() > destination.fraction_bits() {
            payload >> (value.format().fraction_bits() - destination.fraction_bits())
        } else {
            payload << (destination.fraction_bits() - value.format().fraction_bits())
        };
        let bits = destination.exponent_mask() << destination.fraction_bits() | shifted | quiet;
        return Result {
            value: Value::from_bits(destination, bits),
            flags,
        };
    }
    if operand.class == Class::Infinity {
        let bits = u64::from(operand.sign) << (destination.width() - 1)
            | destination.exponent_mask() << destination.fraction_bits();
        return Result {
            value: Value::from_bits(destination, bits),
            flags,
        };
    }
    if operand.class == Class::Zero {
        return Result {
            value: Value::from_bits(destination, u64::from(operand.sign) << (destination.width() - 1)),
            flags,
        };
    }
    let source_precision = u32::from(value.format().precision());
    let target_precision = u32::from(destination.precision());
    let extended = if source_precision > target_precision {
        BitArithmetic::shift_right_jam(operand.significand << 3, source_precision - target_precision)
    } else {
        operand.significand << (target_precision - source_precision + 3)
    };
    environment.round_pack(destination, operand.sign, operand.exponent, extended, flags)
}

fn saturated(sign: bool, signed: bool, width: u8) -> u64 {
    if !signed {
        if sign {
            0
        } else if width == 64 {
            u64::MAX
        } else {
            u64::from(u32::MAX)
        }
    } else if sign {
        if width == 64 {
            i64::MIN as u64
        } else {
            u64::from(i32::MIN as u32)
        }
    } else if width == 64 {
        i64::MAX as u64
    } else {
        u64::from(i32::MAX as u32)
    }
}
