use hl_softfloat::{Environment, ExceptionFlags, Format, NaNMode, RoundingMode, TininessMode, Value};

pub(crate) struct Half;

impl Half {
    pub(crate) fn widen(bits: u16) -> u32 {
        let environment = Environment {
            flush_inputs: false,
            flush_outputs: false,
            ..Environment::default()
        };
        let result = environment.convert(Value::from_bits(Format::Binary16, u64::from(bits)), Format::Binary32);
        (result.value.bits() as u32) | (u32::from(bits >> 15) << 31)
    }

    pub(crate) fn narrow(bits: u32, control: u8, mxcsr: u32) -> (u16, u32) {
        let daz = mxcsr & (1 << 6) != 0;
        let denormal = bits & 0x7f80_0000 == 0 && bits & 0x007f_ffff != 0;
        let source = if daz && denormal { bits & 0x8000_0000 } else { bits };
        let selected = if control & 4 != 0 {
            mxcsr >> 13 & 3
        } else {
            u32::from(control & 3)
        };
        let environment = Environment {
            rounding: match selected {
                0 => RoundingMode::NearestEven,
                1 => RoundingMode::TowardNegative,
                2 => RoundingMode::TowardPositive,
                _ => RoundingMode::TowardZero,
            },
            tininess: TininessMode::BeforeRounding,
            nan: NaNMode::PropagatePayload,
            flush_inputs: false,
            flush_outputs: false,
        };
        let result = environment.convert(Value::from_bits(Format::Binary32, u64::from(source)), Format::Binary16);
        let mut exceptions = Self::exceptions(result.flags);
        if denormal && !daz {
            exceptions |= 1 << 1;
        }
        let value = result.value.bits() as u16 | ((bits >> 16) as u16 & 0x8000);
        (value, exceptions)
    }

    pub(crate) fn multiply_single(left: u32, right: u32, mxcsr: u32) -> (u32, u32) {
        let environment = Environment {
            rounding: match mxcsr >> 13 & 3 {
                0 => RoundingMode::NearestEven,
                1 => RoundingMode::TowardNegative,
                2 => RoundingMode::TowardPositive,
                _ => RoundingMode::TowardZero,
            },
            tininess: TininessMode::AfterRounding,
            nan: NaNMode::PropagatePayload,
            flush_inputs: mxcsr & (1 << 6) != 0,
            flush_outputs: mxcsr & (1 << 15) != 0,
        };
        let result = environment.multiply(
            Value::from_bits(Format::Binary32, u64::from(left)),
            Value::from_bits(Format::Binary32, u64::from(right)),
        );
        let mut bits = result.value.bits() as u32;
        if Self::nan(left) {
            bits = left | 0x0040_0000;
        } else if Self::nan(right) {
            bits = right | 0x0040_0000;
        }
        let mut exceptions = Self::exceptions(result.flags);
        if mxcsr & (1 << 6) != 0 {
            exceptions &= !(1 << 1);
        } else if Self::denormal(left) || Self::denormal(right) {
            exceptions |= 1 << 1;
        }
        (bits, exceptions)
    }

    fn exceptions(flags: ExceptionFlags) -> u32 {
        let mut result = 0;
        for (flag, bit) in [
            (ExceptionFlags::INVALID, 0),
            (ExceptionFlags::INPUT_DENORMAL, 1),
            (ExceptionFlags::DIVIDE_BY_ZERO, 2),
            (ExceptionFlags::OVERFLOW, 3),
            (ExceptionFlags::UNDERFLOW, 4),
            (ExceptionFlags::INEXACT, 5),
        ] {
            if flags.contains(flag) {
                result |= 1 << bit;
            }
        }
        result
    }

    const fn nan(bits: u32) -> bool {
        bits & 0x7f80_0000 == 0x7f80_0000 && bits & 0x007f_ffff != 0
    }

    const fn denormal(bits: u32) -> bool {
        bits & 0x7f80_0000 == 0 && bits & 0x007f_ffff != 0
    }
}

#[cfg(test)]
mod tests {
    use super::Half;

    #[test]
    fn rounding_and_control() {
        let halfway = 0x3f80_1000;
        assert_eq!(Half::narrow(halfway, 0, 0x1f80).0, 0x3c00);
        assert_eq!(Half::narrow(halfway, 2, 0x1f80).0, 0x3c01);
        assert_eq!(Half::narrow(halfway, 4, 0x1f80 | (2 << 13)).0, 0x3c01);
    }

    #[test]
    fn nan_sign_and_payload() {
        let (half, flags) = Half::narrow(0xff80_0001, 0, 0x1f80);
        assert_eq!(half, 0xfe00);
        assert_eq!(flags & 1, 1);
        assert_eq!(Half::widen(0xfe55), 0xffca_a000);
    }

    #[test]
    fn denormal_and_flush_controls() {
        let (_, plain) = Half::narrow(1, 0, 0x1f80 | (1 << 15));
        assert_eq!(plain & 0x32, 0x32);
        let (value, daz) = Half::narrow(0x8000_0001, 0, 0x1f80 | (1 << 6) | (1 << 15));
        assert_eq!(value, 0x8000);
        assert_eq!(daz, 0);
        assert_eq!(Half::widen(1), 0x3380_0000);
    }
}
