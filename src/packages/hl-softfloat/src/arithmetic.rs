use crate::{
    Class, ExceptionFlags, Format, NaNMode, Operand, Result, RoundingMode, TininessMode, Value, bits::BitArithmetic,
    conversion,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Environment {
    pub rounding: RoundingMode,
    pub tininess: TininessMode,
    pub nan: NaNMode,
    pub flush_inputs: bool,
    pub flush_outputs: bool,
}

impl Default for Environment {
    fn default() -> Self {
        Self {
            rounding: RoundingMode::NearestEven,
            tininess: TininessMode::AfterRounding,
            nan: NaNMode::PropagatePayload,
            flush_inputs: false,
            flush_outputs: false,
        }
    }
}

impl Environment {
    pub fn add(self, left: Value, right: Value) -> Result<Value> {
        self.add_subtract(left, right, false)
    }

    pub fn subtract(self, left: Value, right: Value) -> Result<Value> {
        self.add_subtract(left, right, true)
    }

    pub fn multiply(self, left: Value, right: Value) -> Result<Value> {
        assert_eq!(left.format(), right.format());
        let format = left.format();
        let (left, mut flags) = self.unpack(left);
        let (right, right_flags) = self.unpack(right);
        flags |= right_flags;
        if left.class >= Class::QuietNaN || right.class >= Class::QuietNaN {
            return self.nan_result(format, &[left, right], flags);
        }
        let sign = left.sign ^ right.sign;
        if (left.class == Class::Infinity && right.class == Class::Zero)
            || (right.class == Class::Infinity && left.class == Class::Zero)
        {
            return self.invalid(format, flags);
        }
        if left.class == Class::Infinity || right.class == Class::Infinity {
            return Result {
                value: self.infinity(format, sign),
                flags,
            };
        }
        if left.class == Class::Zero || right.class == Class::Zero {
            return Result {
                value: self.zero(format, sign),
                flags,
            };
        }
        let product = left.significand * right.significand;
        let top = 127 - product.leading_zeros();
        let wanted = u32::from(format.precision()) + 2;
        let extended = BitArithmetic::shift_right_jam(product, top - wanted);
        let carry = i32::from(top == u32::from(format.precision()) * 2 - 1);
        self.round_pack(format, sign, left.exponent + right.exponent + carry, extended, flags)
    }

    /// ARM's extended multiply: identical to IEEE multiplication except that
    /// zero times infinity produces signed 2.0 instead of an invalid NaN.
    pub fn multiply_extended(self, left: Value, right: Value) -> Result<Value> {
        assert_eq!(left.format(), right.format());
        let format = left.format();
        let (unpacked_left, mut flags) = self.unpack(left);
        let (unpacked_right, right_flags) = self.unpack(right);
        flags |= right_flags;
        if unpacked_left.class >= Class::QuietNaN || unpacked_right.class >= Class::QuietNaN {
            return self.multiply(left, right);
        }
        let special = (unpacked_left.class == Class::Infinity && unpacked_right.class == Class::Zero)
            || (unpacked_right.class == Class::Infinity && unpacked_left.class == Class::Zero);
        if !special {
            return self.multiply(left, right);
        }
        let sign = unpacked_left.sign ^ unpacked_right.sign;
        let two = ((format.bias() as u64 + 1) << format.fraction_bits()) | if sign { format.sign_mask() } else { 0 };
        Result {
            value: Value::from_bits(format, two),
            flags,
        }
    }

    pub fn divide(self, left: Value, right: Value) -> Result<Value> {
        assert_eq!(left.format(), right.format());
        let format = left.format();
        let (left, mut flags) = self.unpack(left);
        let (right, right_flags) = self.unpack(right);
        flags |= right_flags;
        if left.class >= Class::QuietNaN || right.class >= Class::QuietNaN {
            return self.nan_result(format, &[left, right], flags);
        }
        if (left.class == Class::Infinity && right.class == Class::Infinity)
            || (left.class == Class::Zero && right.class == Class::Zero)
        {
            return self.invalid(format, flags);
        }
        let sign = left.sign ^ right.sign;
        if left.class == Class::Infinity {
            return Result {
                value: self.infinity(format, sign),
                flags,
            };
        }
        if right.class == Class::Infinity {
            return Result {
                value: self.zero(format, sign),
                flags,
            };
        }
        if right.class == Class::Zero {
            flags |= ExceptionFlags::DIVIDE_BY_ZERO;
            return Result {
                value: self.infinity(format, sign),
                flags,
            };
        }
        if left.class == Class::Zero {
            return Result {
                value: self.zero(format, sign),
                flags,
            };
        }
        let precision = u32::from(format.precision());
        let mut exponent = left.exponent - right.exponent;
        let mut numerator = left.significand;
        if numerator < right.significand {
            numerator <<= 1;
            exponent -= 1;
        }
        let scaled = numerator << (precision + 2);
        let mut quotient = scaled / right.significand;
        if scaled % right.significand != 0 {
            quotient |= 1;
        }
        self.round_pack(format, sign, exponent, quotient, flags)
    }

    pub fn square_root(self, operand: Value) -> Result<Value> {
        let format = operand.format();
        let (operand, flags) = self.unpack(operand);
        if operand.class >= Class::QuietNaN {
            return self.nan_result(format, &[operand], flags);
        }
        if operand.sign && operand.class != Class::Zero {
            return self.invalid(format, flags);
        }
        if matches!(operand.class, Class::Infinity | Class::Zero) {
            return Result {
                value: Value::from_bits(format, operand.bits),
                flags,
            };
        }
        let parity = (operand.exponent & 1) as u32;
        let shift = u32::from(format.precision() - 1) + 6 + parity;
        let radicand = operand.significand << shift;
        let (mut root, remainder) = BitArithmetic::integer_square_root(radicand);
        if remainder != 0 {
            root |= 1;
        }
        self.round_pack(format, false, operand.exponent.div_euclid(2), root, flags)
    }

    pub fn from_signed(self, format: Format, value: i64) -> Result<Value> {
        conversion::from_integer(self, format, value.unsigned_abs(), value.is_negative())
    }

    pub fn from_unsigned(self, format: Format, value: u64) -> Result<Value> {
        conversion::from_integer(self, format, value, false)
    }

    pub fn to_signed(self, value: Value, width: u8) -> Result<u64> {
        conversion::to_integer(self, value, width, true, 0)
    }

    pub fn to_unsigned(self, value: Value, width: u8) -> Result<u64> {
        conversion::to_integer(self, value, width, false, 0)
    }

    pub fn to_signed_scaled(self, value: Value, width: u8, scale: u8) -> Result<u64> {
        conversion::to_integer(self, value, width, true, scale)
    }

    pub fn to_unsigned_scaled(self, value: Value, width: u8, scale: u8) -> Result<u64> {
        conversion::to_integer(self, value, width, false, scale)
    }

    pub fn convert(self, value: Value, format: Format) -> Result<Value> {
        conversion::convert(self, value, format)
    }

    fn add_subtract(self, left: Value, right: Value, subtract: bool) -> Result<Value> {
        assert_eq!(left.format(), right.format());
        let format = left.format();
        let (left, mut flags) = self.unpack(left);
        let (mut right, right_flags) = self.unpack(right);
        flags |= right_flags;
        right.sign ^= subtract;
        if left.class >= Class::QuietNaN || right.class >= Class::QuietNaN {
            return self.nan_result(format, &[left, right], flags);
        }
        if left.class == Class::Infinity || right.class == Class::Infinity {
            if left.class == Class::Infinity && right.class == Class::Infinity && left.sign != right.sign {
                return self.invalid(format, flags);
            }
            let infinite = if left.class == Class::Infinity { left } else { right };
            return Result {
                value: self.infinity(format, infinite.sign),
                flags,
            };
        }
        if left.class == Class::Zero && right.class == Class::Zero {
            let sign = if left.sign == right.sign {
                left.sign
            } else {
                self.rounding == RoundingMode::TowardNegative
            };
            return Result {
                value: self.zero(format, sign),
                flags,
            };
        }
        if left.class == Class::Zero {
            return Result {
                value: self.pack_exact(format, right),
                flags,
            };
        }
        if right.class == Class::Zero {
            return Result {
                value: self.pack_exact(format, left),
                flags,
            };
        }
        let (large, small) = BitArithmetic::larger(left, right);
        let mut large_sig = large.significand << 3;
        let small_sig =
            BitArithmetic::shift_right_jam(small.significand << 3, (large.exponent - small.exponent) as u32);
        let mut exponent = large.exponent;
        if large.sign == small.sign {
            large_sig += small_sig;
            if large_sig >> (format.precision() + 3) != 0 {
                large_sig = BitArithmetic::shift_right_jam(large_sig, 1);
                exponent += 1;
            }
        } else {
            large_sig -= small_sig;
            if large_sig == 0 {
                return Result {
                    value: self.zero(format, self.rounding == RoundingMode::TowardNegative),
                    flags,
                };
            }
            while large_sig >> (format.precision() + 2) == 0 {
                large_sig <<= 1;
                exponent -= 1;
            }
        }
        self.round_pack(format, large.sign, exponent, large_sig, flags)
    }

    pub(crate) fn unpack(self, value: Value) -> (Operand, ExceptionFlags) {
        let format = value.format();
        let mut bits = value.bits();
        let sign = bits & format.sign_mask() != 0;
        let exponent_field = bits >> format.fraction_bits() & format.exponent_mask();
        let fraction = bits & format.fraction_mask();
        let mut flags = ExceptionFlags::default();
        let (class, exponent, mut significand) = if exponent_field == 0 {
            if fraction == 0 {
                (Class::Zero, format.minimum_exponent(), 0)
            } else {
                (Class::Subnormal, format.minimum_exponent(), u128::from(fraction))
            }
        } else if exponent_field == format.exponent_mask() {
            let quiet = fraction >> (format.fraction_bits() - 1) & 1 != 0;
            (
                if fraction == 0 {
                    Class::Infinity
                } else if quiet {
                    Class::QuietNaN
                } else {
                    Class::SignalingNaN
                },
                0,
                u128::from(fraction),
            )
        } else {
            (
                Class::Normal,
                exponent_field as i32 - format.bias(),
                u128::from(fraction | 1_u64 << format.fraction_bits()),
            )
        };
        let mut class = class;
        let mut exponent = exponent;
        if class == Class::Subnormal {
            if self.flush_inputs {
                flags |= ExceptionFlags::INPUT_DENORMAL;
                class = Class::Zero;
                significand = 0;
                bits &= format.sign_mask();
            } else {
                (significand, exponent) = BitArithmetic::normalize(significand, exponent, format.fraction_bits());
            }
        }
        (
            Operand {
                sign,
                exponent,
                significand,
                class,
                bits,
            },
            flags,
        )
    }

    pub(crate) fn round_pack(
        self,
        format: Format,
        sign: bool,
        mut exponent: i32,
        mut extended: u128,
        mut flags: ExceptionFlags,
    ) -> Result<Value> {
        let tiny_before = exponent < format.minimum_exponent();
        if tiny_before {
            extended = BitArithmetic::shift_right_jam(extended, (format.minimum_exponent() - exponent) as u32);
            exponent = format.minimum_exponent();
        }
        let discarded = (extended & 7) as u8;
        let mut significand = extended >> 3;
        if BitArithmetic::should_increment(self.rounding, sign, discarded, significand & 1 != 0) {
            significand += 1;
        }
        if significand >> format.precision() != 0 {
            significand >>= 1;
            exponent += 1;
        }
        if exponent > format.maximum_exponent() {
            flags |= ExceptionFlags::OVERFLOW | ExceptionFlags::INEXACT;
            return Result {
                value: self.overflow(format, sign),
                flags,
            };
        }
        let inexact = discarded != 0;
        let normal = significand >> format.fraction_bits() != 0;
        if inexact {
            flags |= ExceptionFlags::INEXACT;
            let tiny_after = !normal;
            if (self.tininess == TininessMode::BeforeRounding && tiny_before)
                || (self.tininess == TininessMode::AfterRounding && tiny_after)
            {
                flags |= ExceptionFlags::UNDERFLOW;
            }
        }
        if self.flush_outputs && !normal && significand != 0 {
            flags |= ExceptionFlags::UNDERFLOW | ExceptionFlags::INEXACT;
            return Result {
                value: self.zero(format, sign),
                flags,
            };
        }
        let exponent_field = if normal { (exponent + format.bias()) as u64 } else { 0 };
        let bits = u64::from(sign) << (format.width() - 1)
            | exponent_field << format.fraction_bits()
            | significand as u64 & format.fraction_mask();
        Result {
            value: Value::from_bits(format, bits),
            flags,
        }
    }

    pub(crate) fn nan_result(self, format: Format, operands: &[Operand], mut flags: ExceptionFlags) -> Result<Value> {
        if operands.iter().any(|operand| operand.class == Class::SignalingNaN) {
            flags |= ExceptionFlags::INVALID;
        }
        if self.nan == NaNMode::Default {
            return Result {
                value: self.default_nan(format),
                flags,
            };
        }
        let chosen = operands
            .iter()
            .find(|operand| operand.class == Class::SignalingNaN)
            .or_else(|| operands.iter().find(|operand| operand.class == Class::QuietNaN))
            .expect("NaN result requires a NaN operand");
        let quiet = 1_u64 << (format.fraction_bits() - 1);
        Result {
            value: Value::from_bits(format, chosen.bits | quiet),
            flags,
        }
    }

    pub(crate) fn invalid(self, format: Format, mut flags: ExceptionFlags) -> Result<Value> {
        flags |= ExceptionFlags::INVALID;
        Result {
            value: self.default_nan(format),
            flags,
        }
    }

    pub(crate) fn pack_exact(self, format: Format, value: Operand) -> Value {
        Value::from_bits(
            format,
            value.bits ^ u64::from(value.sign != (value.bits & format.sign_mask() != 0)) * format.sign_mask(),
        )
    }

    pub(crate) fn zero(self, format: Format, sign: bool) -> Value {
        Value::from_bits(format, u64::from(sign) << (format.width() - 1))
    }

    pub(crate) fn infinity(self, format: Format, sign: bool) -> Value {
        Value::from_bits(
            format,
            u64::from(sign) << (format.width() - 1) | format.exponent_mask() << format.fraction_bits(),
        )
    }

    fn default_nan(self, format: Format) -> Value {
        Value::from_bits(
            format,
            format.exponent_mask() << format.fraction_bits() | 1_u64 << (format.fraction_bits() - 1),
        )
    }

    fn overflow(self, format: Format, sign: bool) -> Value {
        let infinity = match self.rounding {
            RoundingMode::NearestEven | RoundingMode::NearestAway => true,
            RoundingMode::TowardPositive => !sign,
            RoundingMode::TowardNegative => sign,
            RoundingMode::TowardZero => false,
        };
        if infinity {
            self.infinity(format, sign)
        } else {
            Value::from_bits(
                format,
                u64::from(sign) << (format.width() - 1)
                    | (format.exponent_mask() - 1) << format.fraction_bits()
                    | format.fraction_mask(),
            )
        }
    }
}
