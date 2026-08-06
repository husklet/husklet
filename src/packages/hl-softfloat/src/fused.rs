use crate::{Class, Environment, ExceptionFlags, Result, RoundingMode, Value, bits::BitArithmetic};

impl Environment {
    /// Computes `left * right + addend` with one final rounding.
    #[must_use]
    pub fn fused_multiply_add(self, left: Value, right: Value, addend: Value) -> Result<Value> {
        assert_eq!(left.format(), right.format());
        assert_eq!(left.format(), addend.format());
        let format = left.format();
        let (left, mut flags) = self.unpack(left);
        let (right, right_flags) = self.unpack(right);
        let (addend, addend_flags) = self.unpack(addend);
        flags |= right_flags | addend_flags;
        let invalid_product = matches!(
            (left.class, right.class),
            (Class::Infinity, Class::Zero) | (Class::Zero, Class::Infinity)
        );
        if invalid_product && addend.class == Class::QuietNaN {
            return self.invalid(format, flags);
        }
        if left.class >= Class::QuietNaN || right.class >= Class::QuietNaN || addend.class >= Class::QuietNaN {
            return self.nan_result(format, &[addend, left, right], flags);
        }
        if invalid_product {
            return self.invalid(format, flags);
        }
        let product_sign = left.sign ^ right.sign;
        if left.class == Class::Infinity || right.class == Class::Infinity {
            if addend.class == Class::Infinity && addend.sign != product_sign {
                return self.invalid(format, flags);
            }
            return Result {
                value: self.infinity(format, product_sign),
                flags,
            };
        }
        if addend.class == Class::Infinity {
            return Result {
                value: self.infinity(format, addend.sign),
                flags,
            };
        }
        if left.class == Class::Zero || right.class == Class::Zero {
            let product = crate::Operand {
                sign: product_sign,
                exponent: format.minimum_exponent(),
                significand: 0,
                class: Class::Zero,
                bits: u64::from(product_sign) << (format.width() - 1),
            };
            return self.add_exact_operands(format, product, addend, flags);
        }
        let precision = u32::from(format.precision());
        let mut product = left.significand * right.significand;
        let mut product_exponent = left.exponent + right.exponent;
        if product >> (precision * 2 - 1) != 0 {
            product = BitArithmetic::shift_right_jam(product, 1);
            product_exponent += 1;
        }
        let addend_significand = addend.significand << (precision - 1);
        self.fused_sum(
            format,
            product_sign,
            product_exponent,
            product,
            addend.sign,
            addend.exponent,
            addend_significand,
            flags,
        )
    }

    fn add_exact_operands(
        self,
        format: crate::Format,
        product: crate::Operand,
        addend: crate::Operand,
        flags: ExceptionFlags,
    ) -> Result<Value> {
        if addend.class == Class::Zero {
            let sign = if product.sign == addend.sign {
                product.sign
            } else {
                self.rounding == RoundingMode::TowardNegative
            };
            Result {
                value: self.zero(format, sign),
                flags,
            }
        } else {
            Result {
                value: self.pack_exact(format, addend),
                flags,
            }
        }
    }

    fn fused_sum(
        self,
        format: crate::Format,
        product_sign: bool,
        product_exponent: i32,
        mut product: u128,
        addend_sign: bool,
        addend_exponent: i32,
        mut addend: u128,
        flags: ExceptionFlags,
    ) -> Result<Value> {
        let mut exponent = product_exponent.max(addend_exponent);
        product = BitArithmetic::shift_right_jam(product, (exponent - product_exponent) as u32);
        addend = BitArithmetic::shift_right_jam(addend, (exponent - addend_exponent) as u32);
        let (sign, mut magnitude) = if product_sign == addend_sign {
            (product_sign, product + addend)
        } else if product >= addend {
            (product_sign, product - addend)
        } else {
            (addend_sign, addend - product)
        };
        if magnitude == 0 {
            return Result {
                value: self.zero(format, self.rounding == RoundingMode::TowardNegative),
                flags,
            };
        }
        let precision = u32::from(format.precision());
        let target = precision + 2;
        let top = 127 - magnitude.leading_zeros();
        if top > precision * 2 - 2 {
            magnitude = BitArithmetic::shift_right_jam(magnitude, top - (precision * 2 - 2));
            exponent += (top - (precision * 2 - 2)) as i32;
        } else {
            magnitude <<= precision * 2 - 2 - top;
            exponent -= (precision * 2 - 2 - top) as i32;
        }
        let extended = BitArithmetic::shift_right_jam(magnitude, precision * 2 - 2 - target);
        self.round_pack(format, sign, exponent, extended, flags)
    }
}
