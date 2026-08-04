use crate::{ExtendedClass, ExtendedReal, FloatWidth};

#[derive(Clone, Copy)]
pub(crate) struct RealResult {
    pub(crate) bits: u64,
    pub(crate) flags: u16,
}

pub(crate) struct Conversion;

impl Conversion {
    pub(crate) fn expand(bits: u64, format: FloatWidth) -> (ExtendedReal, ExtendedClass) {
        let (fraction_bits, exponent_bits, bias) = match format {
            FloatWidth::Single => (23_u32, 8_u32, 127_i32),
            FloatWidth::Double => (52, 11, 1023),
        };
        let fraction_mask = (1_u64 << fraction_bits) - 1;
        let exponent_mask = (1_u64 << exponent_bits) - 1;
        let fraction = bits & fraction_mask;
        let exponent = bits >> fraction_bits & exponent_mask;
        let sign = bits >> (fraction_bits + exponent_bits) & 1;
        let signed = u128::from(sign) << 79;
        if exponent == 0 {
            if fraction == 0 {
                return (ExtendedReal::from_bits(signed), ExtendedClass::Zero);
            }
            let highest = 63 - fraction.leading_zeros();
            let significand = fraction << (63 - highest);
            let unbiased = 1 - bias - fraction_bits as i32 + highest as i32;
            let encoded = (unbiased + 16383) as u128;
            return (
                ExtendedReal::from_bits(signed | encoded << 64 | u128::from(significand)),
                ExtendedClass::Denormal,
            );
        }
        if exponent == exponent_mask {
            let significand = (1_u64 << 63) | (fraction << (63 - fraction_bits));
            let value = ExtendedReal::from_bits(signed | 0x7fff_u128 << 64 | u128::from(significand));
            return (
                value,
                if fraction == 0 {
                    ExtendedClass::Infinity
                } else if fraction >> (fraction_bits - 1) != 0 {
                    ExtendedClass::QuietNan
                } else {
                    ExtendedClass::SignalingNan
                },
            );
        }
        let significand = ((1_u64 << fraction_bits) | fraction) << (63 - fraction_bits);
        let encoded = exponent as i32 - bias + 16383;
        (
            ExtendedReal::from_bits(signed | (encoded as u128) << 64 | u128::from(significand)),
            ExtendedClass::Normal,
        )
    }

    pub(crate) fn narrow(value: ExtendedReal, class: ExtendedClass, format: FloatWidth, rounding: u16) -> RealResult {
        let (fraction_bits, exponent_bits, bias) = match format {
            FloatWidth::Single => (23_u32, 8_u32, 127_i32),
            FloatWidth::Double => (52, 11, 1023),
        };
        let sign = (value.bits() >> 79) as u64 & 1;
        let operand_flags = if class == ExtendedClass::Denormal { 1 << 1 } else { 0 };
        let sign_bits = sign << (fraction_bits + exponent_bits);
        let exponent_mask = (1_u64 << exponent_bits) - 1;
        match class {
            ExtendedClass::Empty | ExtendedClass::Unsupported | ExtendedClass::SignalingNan => {
                return RealResult {
                    bits: Self::indefinite(format),
                    flags: 1,
                };
            }
            ExtendedClass::Zero => {
                return RealResult {
                    bits: sign_bits,
                    flags: 0,
                };
            }
            ExtendedClass::Infinity => {
                return RealResult {
                    bits: sign_bits | exponent_mask << fraction_bits,
                    flags: 0,
                };
            }
            ExtendedClass::QuietNan => {
                let payload = (value.bits() as u64 & 0x7fff_ffff_ffff_ffff) >> (63 - fraction_bits);
                return RealResult {
                    bits: sign_bits | exponent_mask << fraction_bits | payload | (1 << (fraction_bits - 1)),
                    flags: 0,
                };
            }
            ExtendedClass::Denormal | ExtendedClass::Normal => {}
        }
        let raw = value.bits();
        let encoded = (raw >> 64) as u16 & 0x7fff;
        let significand = raw as u64;
        if significand == 0 {
            return RealResult {
                bits: sign_bits,
                flags: operand_flags,
            };
        }
        let highest = 63 - significand.leading_zeros();
        let base = if encoded == 0 {
            1 - 16383 - 63
        } else {
            i32::from(encoded) - 16383 - 63
        };
        let exponent = base + highest as i32;
        let maximum = bias;
        let minimum = 1 - bias;
        if exponent > maximum {
            return Self::overflow(sign, rounding, fraction_bits, exponent_mask);
        }
        if exponent >= minimum {
            let shift = highest - fraction_bits;
            let (mut rounded, inexact) = Self::round(significand, shift, sign != 0, rounding);
            let mut result_exponent = exponent;
            if rounded == 1_u64 << (fraction_bits + 1) {
                rounded >>= 1;
                result_exponent += 1;
            }
            if result_exponent > maximum {
                return Self::overflow(sign, rounding, fraction_bits, exponent_mask);
            }
            return RealResult {
                bits: sign_bits
                    | ((result_exponent + bias) as u64) << fraction_bits
                    | (rounded & ((1_u64 << fraction_bits) - 1)),
                flags: operand_flags | if inexact { 1 << 5 } else { 0 },
            };
        }
        let quantum = minimum - fraction_bits as i32;
        let shift = u32::try_from(quantum - base).unwrap_or(128);
        let (rounded, inexact) = Self::round(significand, shift, sign != 0, rounding);
        let fraction_limit = 1_u64 << fraction_bits;
        let bits = if rounded >= fraction_limit {
            sign_bits | 1_u64 << fraction_bits
        } else {
            sign_bits | rounded
        };
        RealResult {
            bits,
            flags: operand_flags | if inexact { (1 << 4) | (1 << 5) } else { 0 },
        }
    }

    fn round(value: u64, shift: u32, negative: bool, rounding: u16) -> (u64, bool) {
        if shift == 0 {
            return (value, false);
        }
        let quotient = if shift < 64 { value >> shift } else { 0 };
        let remainder = if shift < 64 {
            value & ((1_u64 << shift) - 1)
        } else {
            value
        };
        if remainder == 0 {
            return (quotient, false);
        }
        let increment = match rounding {
            0 => {
                if shift > 64 {
                    false
                } else {
                    let half = 1_u64 << (shift - 1);
                    remainder > half || remainder == half && quotient & 1 != 0
                }
            }
            1 => negative,
            2 => !negative,
            _ => false,
        };
        (quotient + u64::from(increment), true)
    }

    fn overflow(sign: u64, rounding: u16, fraction_bits: u32, exponent_mask: u64) -> RealResult {
        let infinity = match rounding {
            0 => true,
            1 => sign != 0,
            2 => sign == 0,
            _ => false,
        };
        let sign_bits = sign << (fraction_bits + exponent_mask.count_ones());
        let magnitude = if infinity {
            exponent_mask << fraction_bits
        } else {
            ((exponent_mask - 1) << fraction_bits) | ((1_u64 << fraction_bits) - 1)
        };
        RealResult {
            bits: sign_bits | magnitude,
            flags: (1 << 3) | (1 << 5),
        }
    }

    pub(crate) const fn indefinite(format: FloatWidth) -> u64 {
        match format {
            FloatWidth::Single => 0xffc0_0000,
            FloatWidth::Double => 0xfff8_0000_0000_0000,
        }
    }
}
