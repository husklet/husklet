use core::cmp::Ordering;

use crate::{Class, Comparison, Environment, ExceptionFlags, Result, Value};

impl Environment {
    pub fn minimum(self, left: Value, right: Value) -> Result<Value> {
        self.select(left, right, false, false)
    }

    pub fn maximum(self, left: Value, right: Value) -> Result<Value> {
        self.select(left, right, true, false)
    }

    pub fn minimum_number(self, left: Value, right: Value) -> Result<Value> {
        self.select(left, right, false, true)
    }

    pub fn maximum_number(self, left: Value, right: Value) -> Result<Value> {
        self.select(left, right, true, true)
    }

    pub fn compare(self, left: Value, right: Value, signal_quiet_nan: bool) -> Result<Comparison> {
        assert_eq!(left.format(), right.format());
        let left_value = left;
        let right_value = right;
        let (left, mut flags) = self.unpack(left_value);
        let (right, right_flags) = self.unpack(right_value);
        flags |= right_flags;
        if left.class >= Class::QuietNaN || right.class >= Class::QuietNaN {
            if signal_quiet_nan || left.class == Class::SignalingNaN || right.class == Class::SignalingNaN {
                flags |= ExceptionFlags::INVALID;
            }
            return Result {
                value: Comparison::Unordered,
                flags,
            };
        }
        let both_zero = left.class == Class::Zero && right.class == Class::Zero;
        let ordering = if both_zero {
            Ordering::Equal
        } else {
            left_value.order_key().cmp(&right_value.order_key())
        };
        let value = match ordering {
            Ordering::Less => Comparison::Less,
            Ordering::Equal => Comparison::Equal,
            Ordering::Greater => Comparison::Greater,
        };
        Result { value, flags }
    }

    pub fn total_order(self, left: Value, right: Value) -> Ordering {
        assert_eq!(left.format(), right.format());
        left.order_key().cmp(&right.order_key())
    }

    pub fn round_to_integral(self, operand: Value, exact: bool) -> Result<Value> {
        let format = operand.format();
        let (operand, mut flags) = self.unpack(operand);
        if operand.class >= Class::QuietNaN {
            return self.nan_result(format, &[operand], flags);
        }
        if matches!(operand.class, Class::Infinity | Class::Zero) || operand.exponent >= 0 {
            if operand.exponent >= i32::from(format.fraction_bits()) {
                return Result {
                    value: self.pack_exact(format, operand),
                    flags,
                };
            }
        }
        let shift = i32::from(format.fraction_bits()) - operand.exponent;
        if shift <= 0 {
            return Result {
                value: self.pack_exact(format, operand),
                flags,
            };
        }
        let extended = if shift >= 128 {
            u128::from(operand.significand != 0)
        } else {
            crate::bits::BitArithmetic::shift_right_jam(operand.significand << 3, shift as u32)
        };
        let discarded = extended & 7;
        let mut magnitude = extended >> 3;
        if crate::bits::BitArithmetic::should_increment(
            self.rounding,
            operand.sign,
            discarded as u8,
            magnitude & 1 != 0,
        ) {
            magnitude += 1;
        }
        if exact && discarded != 0 {
            flags |= ExceptionFlags::INEXACT;
        }
        if magnitude == 0 {
            return Result {
                value: self.zero(format, operand.sign),
                flags,
            };
        }
        let converted = if operand.sign {
            self.from_signed(format, -(magnitude as i64))
        } else {
            self.from_unsigned(format, magnitude as u64)
        };
        Result {
            value: converted.value,
            flags: flags | converted.flags,
        }
    }

    fn select(self, left: Value, right: Value, maximum: bool, number: bool) -> Result<Value> {
        assert_eq!(left.format(), right.format());
        let format = left.format();
        let (left, mut flags) = self.unpack(left);
        let (right, right_flags) = self.unpack(right);
        flags |= right_flags;
        if number {
            if left.class == Class::QuietNaN && right.class < Class::QuietNaN {
                return Result {
                    value: self.pack_exact(format, right),
                    flags,
                };
            }
            if right.class == Class::QuietNaN && left.class < Class::QuietNaN {
                return Result {
                    value: self.pack_exact(format, left),
                    flags,
                };
            }
        }
        if left.class >= Class::QuietNaN || right.class >= Class::QuietNaN {
            return self.nan_result(format, &[left, right], flags);
        }
        if left.class == Class::Zero && right.class == Class::Zero {
            let sign = if maximum {
                left.sign && right.sign
            } else {
                left.sign || right.sign
            };
            return Result {
                value: self.zero(format, sign),
                flags,
            };
        }
        let comparison = self.compare(
            Value::from_bits(format, left.bits),
            Value::from_bits(format, right.bits),
            false,
        );
        let choose_left = if maximum {
            comparison.value == Comparison::Greater
        } else {
            comparison.value == Comparison::Less
        };
        Result {
            value: self.pack_exact(format, if choose_left { left } else { right }),
            flags: flags | comparison.flags,
        }
    }
}
