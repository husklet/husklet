use crate::{
    Aarch64CpuState, Aarch64Instruction, FPSR_DIVIDE_BY_ZERO, FPSR_INEXACT, FPSR_INPUT_DENORMAL, FPSR_INVALID,
    FPSR_OVERFLOW, FPSR_UNDERFLOW, FpArithmetic, FpArithmeticPort, FpFormat, FpRequest,
};

pub(crate) struct Reciprocal;

impl Reciprocal {
    pub(crate) fn decode(word: u32) -> Option<Aarch64Instruction> {
        if let Some(instruction) = Self::decode_exponent(word) {
            return Some(instruction);
        }
        if let Some(instruction) = Self::decode_unsigned(word) {
            return Some(instruction);
        }
        let (format, reciprocal_sqrt, lanes) = match word & 0xffff_fc00 {
            0x0ef9_d800 => (FpFormat::Half, false, 4),
            0x4ef9_d800 => (FpFormat::Half, false, 8),
            0x5ef9_d800 => (FpFormat::Half, false, 1),
            0x2ef9_d800 => (FpFormat::Half, true, 4),
            0x6ef9_d800 => (FpFormat::Half, true, 8),
            0x7ef9_d800 => (FpFormat::Half, true, 1),
            0x4ea1_d800 => (FpFormat::Single, false, 4),
            0x4ee1_d800 => (FpFormat::Double, false, 2),
            0x5ea1_d800 => (FpFormat::Single, false, 1),
            0x5ee1_d800 => (FpFormat::Double, false, 1),
            0x6ea1_d800 => (FpFormat::Single, true, 4),
            0x6ee1_d800 => (FpFormat::Double, true, 2),
            0x7ea1_d800 => (FpFormat::Single, true, 1),
            0x7ee1_d800 => (FpFormat::Double, true, 1),
            _ => return Self::decode_step(word),
        };
        Some(Aarch64Instruction::SimdFpEstimate {
            format,
            reciprocal_sqrt,
            source: (word >> 5 & 31) as u8,
            destination: (word & 31) as u8,
            lanes,
        })
    }

    fn decode_unsigned(word: u32) -> Option<Aarch64Instruction> {
        let reciprocal_sqrt = match word & 0xbfff_fc00 {
            0x0ea1_c800 => false,
            0x2ea1_c800 => true,
            _ => return None,
        };
        Some(Aarch64Instruction::SimdUnsignedEstimate {
            reciprocal_sqrt,
            source: (word >> 5 & 31) as u8,
            destination: (word & 31) as u8,
            wide: word >> 30 & 1 != 0,
        })
    }

    pub(crate) fn unsigned(cpu: &mut Aarch64CpuState, reciprocal_sqrt: bool, source: u8, destination: u8, wide: bool) {
        let mut value = 0_u128;
        let lanes = if wide { 4 } else { 2 };
        for lane in 0..lanes {
            let input = cpu.vector_lane(source, 32, lane) as u32;
            value |= u128::from(Self::unsigned_lane(input, reciprocal_sqrt)) << (lane * 32);
        }
        cpu.set_vector(destination, value);
    }

    fn unsigned_lane(value: u32, sqrt: bool) -> u32 {
        if if sqrt { value >> 30 == 0 } else { value >> 31 == 0 } {
            return u32::MAX;
        }
        let estimate = if sqrt {
            Self::sqrt_table(value >> 23)
        } else {
            Self::recip_table(value >> 23)
        };
        estimate << 23
    }

    fn decode_exponent(word: u32) -> Option<Aarch64Instruction> {
        let format = match word & 0xffff_fc00 {
            0x5ef9_f800 => FpFormat::Half,
            0x5ea1_f800 => FpFormat::Single,
            0x5ee1_f800 => FpFormat::Double,
            _ => return None,
        };
        Some(Aarch64Instruction::SimdFpRecipExponent {
            format,
            source: (word >> 5 & 31) as u8,
            destination: (word & 31) as u8,
        })
    }

    pub(crate) fn exponent(cpu: &mut Aarch64CpuState, format: FpFormat, source: u8, destination: u8) {
        let (value, flags) = Self::exponent_lane(cpu.vector_lane(source, format.bits(), 0), format, cpu.fpcr as u32);
        cpu.fpsr |= u64::from(flags);
        cpu.set_vector(destination, u128::from(value));
    }

    fn exponent_lane(bits: u64, format: FpFormat, fpcr: u32) -> (u64, u32) {
        let mut flags = 0;
        if Self::class(bits, format) >= 4 {
            return (Self::nan(bits, format, fpcr, &mut flags), flags);
        }
        if Self::class(bits, format) == 1 && Self::flush(fpcr, format) && format != FpFormat::Half {
            flags |= FPSR_INPUT_DENORMAL;
        }
        let exponent = bits >> Self::mantissa(format) & Self::inf_exp(format);
        let reflected = if exponent == 0 {
            Self::inf_exp(format) - 1
        } else {
            !exponent & Self::inf_exp(format)
        };
        (bits & Self::sign(format) | reflected << Self::mantissa(format), flags)
    }

    fn decode_step(word: u32) -> Option<Aarch64Instruction> {
        let (format, reciprocal_sqrt, lanes) = match word & 0xffe0_fc00 {
            0x0e40_3c00 => (FpFormat::Half, false, 4),
            0x4e40_3c00 => (FpFormat::Half, false, 8),
            0x5e40_3c00 => (FpFormat::Half, false, 1),
            0x0ec0_3c00 => (FpFormat::Half, true, 4),
            0x4ec0_3c00 => (FpFormat::Half, true, 8),
            0x5ec0_3c00 => (FpFormat::Half, true, 1),
            0x4e20_fc00 => (FpFormat::Single, false, 4),
            0x4e60_fc00 => (FpFormat::Double, false, 2),
            0x5e20_fc00 => (FpFormat::Single, false, 1),
            0x5e60_fc00 => (FpFormat::Double, false, 1),
            0x4ea0_fc00 => (FpFormat::Single, true, 4),
            0x4ee0_fc00 => (FpFormat::Double, true, 2),
            0x5ea0_fc00 => (FpFormat::Single, true, 1),
            0x5ee0_fc00 => (FpFormat::Double, true, 1),
            _ => return None,
        };
        Some(Aarch64Instruction::SimdFpStep {
            format,
            reciprocal_sqrt,
            left: (word >> 5 & 31) as u8,
            right: (word >> 16 & 31) as u8,
            destination: (word & 31) as u8,
            lanes,
        })
    }

    pub(crate) fn estimate(
        cpu: &mut Aarch64CpuState,
        format: FpFormat,
        reciprocal_sqrt: bool,
        source: u8,
        destination: u8,
        lanes: u8,
    ) {
        let mut value = 0_u128;
        let mut flags = 0;
        for lane in 0..lanes {
            let (result, exceptions) = Self::estimate_lane(
                cpu.vector_lane(source, format.bits(), lane),
                format,
                reciprocal_sqrt,
                cpu.fpcr as u32,
            );
            value |= u128::from(result) << (u32::from(lane) * u32::from(format.bits()));
            flags |= exceptions;
        }
        cpu.fpsr |= u64::from(flags);
        cpu.set_vector(destination, value);
    }

    pub(crate) fn step<P: FpArithmeticPort>(
        cpu: &mut Aarch64CpuState,
        port: &mut P,
        format: FpFormat,
        reciprocal_sqrt: bool,
        left: u8,
        right: u8,
        destination: u8,
        lanes: u8,
    ) {
        let mut value = 0_u128;
        for lane in 0..lanes {
            let a = cpu.vector_lane(left, format.bits(), lane);
            let b = cpu.vector_lane(right, format.bits(), lane);
            let result = Self::step_lane(port, a, b, format, reciprocal_sqrt, cpu.fpcr as u32);
            value |= u128::from(result.0) << (u32::from(lane) * u32::from(format.bits()));
            cpu.fpsr |= u64::from(result.1);
        }
        cpu.set_vector(destination, value);
    }

    fn step_lane<P: FpArithmeticPort>(
        port: &mut P,
        mut a: u64,
        mut b: u64,
        format: FpFormat,
        reciprocal_sqrt: bool,
        fpcr: u32,
    ) -> (u64, u32) {
        if fpcr & 2 != 0 {
            let mut negated = a ^ Self::sign(format);
            if reciprocal_sqrt {
                negated = Self::halve(negated, format);
            }
            let result = port.evaluate(FpRequest {
                operation: FpArithmetic::FusedMultiplyAdd,
                format,
                left: negated,
                right: b,
                addend: Self::constant(format, reciprocal_sqrt),
                fpcr,
            });
            return (result.value, result.exceptions);
        }
        a ^= Self::sign(format);
        let mut flags = 0;
        Self::flush_operand(&mut a, format, fpcr, &mut flags);
        Self::flush_operand(&mut b, format, fpcr, &mut flags);
        let ca = Self::class(a, format);
        let cb = Self::class(b, format);
        if ca == 5 || cb == 5 {
            let nan = if ca == 5 { a } else { b };
            return (Self::nan(nan, format, fpcr, &mut flags), flags);
        }
        if ca == 4 || cb == 4 {
            let nan = if ca == 4 { a } else { b };
            return (Self::nan(nan, format, fpcr, &mut flags), flags);
        }
        if ca == 3 && cb == 0 || ca == 0 && cb == 3 {
            return (Self::constant(format, reciprocal_sqrt), flags);
        }
        if ca == 3 || cb == 3 {
            return ((a ^ b) & Self::sign(format) | Self::infinity(format), flags);
        }

        let exponent = a >> Self::mantissa(format) & Self::inf_exp(format);
        let prehalve = reciprocal_sqrt && exponent >= 2;
        if prehalve {
            a = Self::halve(a, format);
        }
        let addend = if reciprocal_sqrt && !prehalve {
            u64::from((Self::bias(format) + 1) as u32) << Self::mantissa(format) | 1 << (Self::mantissa(format) - 1)
        } else {
            Self::constant(format, reciprocal_sqrt)
        };
        let result = port.evaluate(FpRequest {
            operation: FpArithmetic::FusedMultiplyAdd,
            format,
            left: a,
            right: b,
            addend,
            fpcr,
        });
        flags |= result.exceptions;
        if reciprocal_sqrt && !prehalve {
            let exponent = result.value >> Self::mantissa(format) & Self::inf_exp(format);
            return (
                if exponent == 0 {
                    result.value & Self::sign(format)
                } else {
                    Self::halve(result.value, format)
                },
                flags,
            );
        }
        (result.value, flags)
    }

    fn estimate_lane(mut bits: u64, format: FpFormat, sqrt: bool, fpcr: u32) -> (u64, u32) {
        let mut flags = 0;
        let sign = bits & Self::sign(format);
        if Self::class(bits, format) == 1 && Self::flush(fpcr, format) {
            bits = sign;
            if format != FpFormat::Half {
                flags |= FPSR_INPUT_DENORMAL;
            }
        }
        let class = Self::class(bits, format);
        if class >= 4 {
            return (Self::nan(bits, format, fpcr, &mut flags), flags);
        }
        if sqrt {
            if class == 0 {
                flags |= FPSR_DIVIDE_BY_ZERO;
                return (sign | Self::infinity(format), flags);
            }
            if sign != 0 {
                flags |= FPSR_INVALID;
                return (Self::default_nan(format), flags);
            }
            if class == 3 {
                return (0, flags);
            }
            return (Self::sqrt_estimate(bits, format), flags);
        }
        if class == 3 {
            return (sign, flags);
        }
        if class == 0 {
            flags |= FPSR_DIVIDE_BY_ZERO;
            return (sign | Self::infinity(format), flags);
        }
        let mantissa = Self::mantissa(format);
        let fraction = bits & Self::fraction_mask(format);
        let mut exponent = ((bits >> mantissa) & Self::inf_exp(format)) as i32;
        if exponent == 0 && fraction >> (mantissa - 2) == 0 {
            flags |= FPSR_OVERFLOW | FPSR_INEXACT;
            let rounding = fpcr >> 22 & 3;
            let infinity = rounding == 0 || rounding == 1 && sign == 0 || rounding == 2 && sign != 0;
            let magnitude = if infinity {
                Self::infinity(format)
            } else {
                Self::max_finite(format)
            };
            return (sign | magnitude, flags);
        }
        if Self::flush(fpcr, format) && exponent >= 2 * Self::bias(format) - 1 {
            flags |= FPSR_UNDERFLOW;
            return (sign, flags);
        }
        let mut normalized = fraction << (52 - mantissa);
        if exponent == 0 {
            if normalized >> 51 == 0 {
                exponent = -1;
                normalized = normalized << 2 & ((1_u64 << 52) - 1);
            } else {
                normalized = normalized << 1 & ((1_u64 << 52) - 1);
            }
        }
        let estimate = Self::recip_table(256 + ((normalized >> 44) & 255) as u32);
        let mut result_exp = 2 * Self::bias(format) - 1 - exponent;
        normalized = u64::from(estimate & 255) << 44;
        if result_exp == 0 {
            normalized = 1 << 51 | normalized >> 1;
        } else if result_exp == -1 {
            normalized = 1 << 50 | normalized >> 2;
            result_exp = 0;
        }
        (
            sign | (result_exp as u64) << mantissa | normalized >> (52 - mantissa),
            flags,
        )
    }

    fn sqrt_estimate(bits: u64, format: FpFormat) -> u64 {
        let mantissa = Self::mantissa(format);
        let inf = Self::inf_exp(format);
        let mut fraction = (bits & Self::fraction_mask(format)) << (52 - mantissa);
        let mut exponent = ((bits >> mantissa) & inf) as i32;
        if exponent == 0 {
            while fraction >> 51 == 0 {
                fraction = fraction << 1 & ((1_u64 << 52) - 1);
                exponent -= 1;
            }
            fraction = fraction << 1 & ((1_u64 << 52) - 1);
        }
        let scaled = if exponent & 1 != 0 {
            128 + ((fraction >> 45) & 127) as u32
        } else {
            256 + ((fraction >> 44) & 255) as u32
        };
        let result_exp = (3 * Self::bias(format) - 1 - exponent) / 2;
        (result_exp as u64) << mantissa | u64::from(Self::sqrt_table(scaled) & 255) << (mantissa - 8)
    }

    fn flush_operand(value: &mut u64, format: FpFormat, fpcr: u32, flags: &mut u32) {
        if Self::class(*value, format) != 1 || !Self::flush(fpcr, format) {
            return;
        }
        *value &= Self::sign(format);
        if format != FpFormat::Half {
            *flags |= FPSR_INPUT_DENORMAL;
        }
    }

    fn nan(bits: u64, format: FpFormat, fpcr: u32, flags: &mut u32) -> u64 {
        if Self::class(bits, format) == 5 {
            *flags |= FPSR_INVALID;
        }
        if fpcr & 1 << 25 != 0 {
            Self::default_nan(format)
        } else {
            bits | 1 << (Self::mantissa(format) - 1)
        }
    }
    fn recip_table(a: u32) -> u32 {
        let b = (1 << 19) / (a * 2 + 1);
        b.div_ceil(2)
    }
    fn sqrt_table(a: u32) -> u32 {
        let a = if a < 256 { a * 2 + 1 } else { ((a >> 1) * 2 + 1) * 2 };
        // The architectural table asks for the first candidate whose squared
        // product reaches 2^28.  Its input domain bounds that candidate to
        // [512, 1024], so binary search preserves the exact integer result
        // while avoiding hundreds of interpreter-side iterations per lane.
        let mut low = 512_u64;
        let mut high = 1024_u64;
        while low < high {
            let candidate = low + (high - low) / 2;
            if u64::from(a) * (candidate + 1) * (candidate + 1) < 1 << 28 {
                low = candidate + 1;
            } else {
                high = candidate;
            }
        }
        low.div_ceil(2) as u32
    }
    fn class(bits: u64, format: FpFormat) -> u8 {
        let e = bits >> Self::mantissa(format) & Self::inf_exp(format);
        let f = bits & Self::fraction_mask(format);
        if e == 0 {
            u8::from(f != 0)
        } else if e != Self::inf_exp(format) {
            2
        } else if f == 0 {
            3
        } else if f >> (Self::mantissa(format) - 1) & 1 != 0 {
            4
        } else {
            5
        }
    }
    fn mantissa(format: FpFormat) -> u32 {
        match format {
            FpFormat::Half => 10,
            FpFormat::Single => 23,
            FpFormat::Double => 52,
        }
    }
    fn bias(format: FpFormat) -> i32 {
        match format {
            FpFormat::Half => 15,
            FpFormat::Single => 127,
            FpFormat::Double => 1023,
        }
    }
    fn inf_exp(format: FpFormat) -> u64 {
        match format {
            FpFormat::Half => 0x1f,
            FpFormat::Single => 0xff,
            FpFormat::Double => 0x7ff,
        }
    }
    fn sign(format: FpFormat) -> u64 {
        1 << match format {
            FpFormat::Half => 15,
            FpFormat::Single => 31,
            FpFormat::Double => 63,
        }
    }
    fn flush(fpcr: u32, format: FpFormat) -> bool {
        fpcr & if format == FpFormat::Half { 1 << 19 } else { 1 << 24 } != 0
    }
    fn fraction_mask(format: FpFormat) -> u64 {
        (1 << Self::mantissa(format)) - 1
    }
    fn infinity(format: FpFormat) -> u64 {
        Self::inf_exp(format) << Self::mantissa(format)
    }
    fn max_finite(format: FpFormat) -> u64 {
        (Self::inf_exp(format) - 1) << Self::mantissa(format) | Self::fraction_mask(format)
    }
    fn default_nan(format: FpFormat) -> u64 {
        Self::infinity(format) | 1 << (Self::mantissa(format) - 1)
    }
    fn constant(format: FpFormat, sqrt: bool) -> u64 {
        if sqrt {
            u64::from(Self::bias(format) as u32) << Self::mantissa(format) | 1 << (Self::mantissa(format) - 1)
        } else {
            u64::from((Self::bias(format) + 1) as u32) << Self::mantissa(format)
        }
    }
    fn halve(bits: u64, format: FpFormat) -> u64 {
        bits.wrapping_sub(1 << Self::mantissa(format))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Aarch64ExecutionExit, Aarch64FpExecutor, Aarch64SoftFloat, Nzcv};

    #[test]
    fn encodings() {
        for (base, sqrt, wide) in [
            (0x0ea1_c800, false, false),
            (0x4ea1_c800, false, true),
            (0x2ea1_c800, true, false),
            (0x6ea1_c800, true, true),
        ] {
            for encoded in 0_u32..1024 {
                assert_eq!(
                    Reciprocal::decode(base | encoded),
                    Some(Aarch64Instruction::SimdUnsignedEstimate {
                        reciprocal_sqrt: sqrt,
                        source: (encoded >> 5) as u8,
                        destination: (encoded & 31) as u8,
                        wide
                    })
                );
            }
        }
        for (base, format) in [
            (0x5ef9_f800, FpFormat::Half),
            (0x5ea1_f800, FpFormat::Single),
            (0x5ee1_f800, FpFormat::Double),
        ] {
            for encoded in 0_u32..1024 {
                assert_eq!(
                    Reciprocal::decode(base | encoded),
                    Some(Aarch64Instruction::SimdFpRecipExponent {
                        format,
                        source: (encoded >> 5) as u8,
                        destination: (encoded & 31) as u8
                    })
                );
            }
        }
        for (base, format, sqrt, lanes) in [
            (0x0ef9_d800, FpFormat::Half, false, 4),
            (0x4ef9_d800, FpFormat::Half, false, 8),
            (0x5ef9_d800, FpFormat::Half, false, 1),
            (0x2ef9_d800, FpFormat::Half, true, 4),
            (0x6ef9_d800, FpFormat::Half, true, 8),
            (0x7ef9_d800, FpFormat::Half, true, 1),
            (0x4ea1_d800, FpFormat::Single, false, 4),
            (0x4ee1_d800, FpFormat::Double, false, 2),
            (0x5ea1_d800, FpFormat::Single, false, 1),
            (0x5ee1_d800, FpFormat::Double, false, 1),
            (0x6ea1_d800, FpFormat::Single, true, 4),
            (0x6ee1_d800, FpFormat::Double, true, 2),
            (0x7ea1_d800, FpFormat::Single, true, 1),
            (0x7ee1_d800, FpFormat::Double, true, 1),
        ] {
            for encoded in 0_u32..1024 {
                assert_eq!(
                    Reciprocal::decode(base | encoded),
                    Some(Aarch64Instruction::SimdFpEstimate {
                        format,
                        reciprocal_sqrt: sqrt,
                        source: (encoded >> 5) as u8,
                        destination: (encoded & 31) as u8,
                        lanes
                    })
                );
            }
        }
        for (base, format, sqrt, lanes) in [
            (0x0e40_3c00, FpFormat::Half, false, 4),
            (0x4e40_3c00, FpFormat::Half, false, 8),
            (0x5e40_3c00, FpFormat::Half, false, 1),
            (0x0ec0_3c00, FpFormat::Half, true, 4),
            (0x4ec0_3c00, FpFormat::Half, true, 8),
            (0x5ec0_3c00, FpFormat::Half, true, 1),
            (0x4e20_fc00, FpFormat::Single, false, 4),
            (0x4e60_fc00, FpFormat::Double, false, 2),
            (0x5e20_fc00, FpFormat::Single, false, 1),
            (0x5e60_fc00, FpFormat::Double, false, 1),
            (0x4ea0_fc00, FpFormat::Single, true, 4),
            (0x4ee0_fc00, FpFormat::Double, true, 2),
            (0x5ea0_fc00, FpFormat::Single, true, 1),
            (0x5ee0_fc00, FpFormat::Double, true, 1),
        ] {
            for encoded in 0_u32..32 * 32 * 32 {
                let left = encoded / 1024;
                let right = encoded / 32 % 32;
                let destination = encoded % 32;
                assert_eq!(
                    Reciprocal::decode(base | right << 16 | left << 5 | destination),
                    Some(Aarch64Instruction::SimdFpStep {
                        format,
                        reciprocal_sqrt: sqrt,
                        left: left as u8,
                        right: right as u8,
                        destination: destination as u8,
                        lanes
                    })
                );
            }
        }
        for word in [0x0ea1_d820, 0x4e21_d820, 0x4e20_f820, 0x4e20_dc20] {
            assert_eq!(Reciprocal::decode(word), None);
        }
    }

    #[test]
    fn unsigned_domains() {
        unsigned_indexes(false, 0);
        unsigned_indexes(false, 1);
        unsigned_indexes(false, 0x7f_ffff);
        unsigned_indexes(true, 0);
        unsigned_indexes(true, 1);
        unsigned_indexes(true, 0x7f_ffff);
        for (value, sqrt) in [
            (0, false),
            (0x7fff_ffff, false),
            (0x8000_0000, false),
            (0, true),
            (0x3fff_ffff, true),
            (0x4000_0000, true),
            (u32::MAX, true),
        ] {
            assert_eq!(Reciprocal::unsigned_lane(value, sqrt), unsigned_reference(value, sqrt));
        }
    }

    #[test]
    fn unsigned_random() {
        let mut state = 0xa409_3822_u32;
        for sqrt in [false, true] {
            for _ in 0..100_000 {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                assert_eq!(Reciprocal::unsigned_lane(state, sqrt), unsigned_reference(state, sqrt));
            }
        }
    }

    #[test]
    fn unsigned_alias() {
        let mut cpu = Aarch64CpuState {
            pc: 0x402000,
            fpcr: u64::MAX,
            fpsr: 0x95,
            nzcv: Nzcv::from_bits(0x6000_0000),
            ..Default::default()
        };
        cpu.set_vector(31, 0xffff_ffff_c000_0000_8000_0000_7fff_ffff);
        execute(&mut cpu, 0x4ea1_cbff);
        let expected = [0xffff_ffff, 0xff80_0000, 0xaa80_0000, 0x8000_0000_u32]
            .into_iter()
            .enumerate()
            .fold(0_u128, |all, (lane, value)| all | u128::from(value) << (lane * 32));
        assert_eq!(cpu.vector(31), expected);
        assert_eq!(cpu.fpsr, 0x95);
        assert_eq!(cpu.fpcr, u64::MAX);
        assert_eq!(cpu.nzcv.bits(), 0x6000_0000);
        cpu.pc = 0;
        cpu.set_vector(0, u128::MAX);
        cpu.set_vector(1, 0xffff_ffff_8000_0000);
        execute(&mut cpu, 0x0ea1_c820);
        assert_eq!(cpu.vector(0) >> 64, 0);
    }

    #[test]
    fn exponent_domains() {
        for format in [FpFormat::Half, FpFormat::Single, FpFormat::Double] {
            exponent_format(format, 0);
            exponent_format(format, Reciprocal::sign(format));
        }
    }

    #[test]
    fn exponent_random() {
        let mut state = 0xd1b5_4a32_d192_ed03_u64;
        for format in [FpFormat::Half, FpFormat::Single, FpFormat::Double] {
            for fpcr in [0, 1 << 19, 1 << 24, 1 << 25, 1 << 19 | 1 << 25, 1 << 24 | 1 << 25] {
                exponent_samples(&mut state, format, fpcr);
            }
        }
    }

    #[test]
    fn exponent_alias() {
        let mut cpu = Aarch64CpuState {
            pc: 0x401000,
            fpsr: 1 << 27,
            nzcv: Nzcv::from_bits(0x9000_0000),
            ..Default::default()
        };
        cpu.set_vector(31, u128::MAX << 64 | 0x3ff8_1234_5678_9abc);
        execute(&mut cpu, 0x5ee1_fbff);
        assert_eq!(cpu.vector(31), 0x4000_0000_0000_0000);
        assert_eq!(cpu.fpsr, 1 << 27);
        assert_eq!(cpu.nzcv.bits(), 0x9000_0000);
        assert_eq!(cpu.pc, 0x401004);
    }

    #[test]
    fn table_domains() {
        for a in 256..512 {
            assert_eq!(Reciprocal::recip_table(a), recip_reference(a));
        }
        for a in 128..512 {
            assert_eq!(Reciprocal::sqrt_table(a), sqrt_reference(a));
        }
    }

    #[test]
    fn random_reference() {
        let mut state = 0x243f_6a88_u64;
        for format in [FpFormat::Half, FpFormat::Single, FpFormat::Double] {
            random_format(&mut state, format, false);
            random_format(&mut state, format, true);
        }
    }

    #[test]
    fn specials_modes() {
        let single = FpFormat::Single;
        for (bits, sqrt, expected, flags) in [
            (0_u64, false, 0x7f80_0000, FPSR_DIVIDE_BY_ZERO),
            (0x8000_0000, false, 0xff80_0000, FPSR_DIVIDE_BY_ZERO),
            (0x7f80_0000, false, 0, 0),
            (0xff80_0000, false, 0x8000_0000, 0),
            (0x7f80_0000, true, 0, 0),
            (0xbf80_0000, true, 0x7fc0_0000, FPSR_INVALID),
            (0x7f80_0001, false, 0x7fc0_0001, FPSR_INVALID),
        ] {
            assert_eq!(Reciprocal::estimate_lane(bits, single, sqrt, 0), (expected, flags));
        }
        assert_eq!(
            Reciprocal::estimate_lane(1, single, false, 1 << 24),
            (0x7f80_0000, FPSR_INPUT_DENORMAL | FPSR_DIVIDE_BY_ZERO)
        );
        assert_eq!(
            Reciprocal::estimate_lane(0xff80_0001, single, false, 1 << 25),
            (0x7fc0_0000, FPSR_INVALID)
        );
        assert_eq!(
            Reciprocal::estimate_lane(1, FpFormat::Half, false, 1 << 19),
            (0x7c00, FPSR_DIVIDE_BY_ZERO)
        );
        assert_eq!(
            Reciprocal::estimate_lane(0xfc01, FpFormat::Half, false, 1 << 25),
            (0x7e00, FPSR_INVALID)
        );
    }

    #[test]
    fn frontier_aliases() {
        let mut cpu = Aarch64CpuState {
            pc: 0x400000,
            fpsr: 1 << 27,
            nzcv: Nzcv::from_bits(0x6000_0000),
            ..Default::default()
        };
        cpu.set_vector(1, 0x4080_0000_4040_0000_4000_0000_3f80_0000);
        execute(&mut cpu, 0x4ea1_d820);
        assert_eq!(cpu.vector(0), 0x3e7f_8000_3eaa_8000_3eff_8000_3f7f_8000);
        assert_eq!(cpu.fpsr, 1 << 27);
        assert_eq!(cpu.nzcv.bits(), 0x6000_0000);
        cpu.pc = 0;
        cpu.set_vector(0, 0x4080_0000_4040_0000_4000_0000_3f80_0000);
        execute(&mut cpu, 0x4ea1_d800);
        assert_eq!(cpu.vector(0), 0x3e7f_8000_3eaa_8000_3eff_8000_3f7f_8000);
        cpu.pc = 0;
        cpu.set_vector(0, u128::MAX);
        cpu.set_vector(1, 0x4000_0000);
        execute(&mut cpu, 0x5ea1_d820);
        assert_eq!(cpu.vector(0), 0x3eff_8000);
    }

    #[test]
    fn steps() {
        let mut cpu = Aarch64CpuState::default();
        cpu.set_vector(1, 0x3f80_0000);
        cpu.set_vector(2, 0x3f80_0000);
        execute(&mut cpu, 0x5e22_fc20);
        assert_eq!(cpu.vector(0), 0x3f80_0000);
        cpu.pc = 0;
        execute(&mut cpu, 0x5ea2_fc20);
        assert_eq!(cpu.vector(0), 0x3f80_0000);
        cpu.pc = 0;
        cpu.set_vector(1, 0);
        cpu.set_vector(2, 0x7f80_0000);
        execute(&mut cpu, 0x5e22_fc20);
        assert_eq!(cpu.vector(0), 0x4000_0000);
        cpu.pc = 0;
        cpu.fpcr = 2;
        execute(&mut cpu, 0x5e22_fc20);
        assert_eq!(cpu.vector(0), 0x7fc0_0000);
        assert_ne!(cpu.fpsr & u64::from(FPSR_INVALID), 0);
        cpu.pc = 0;
        cpu.fpcr = 0;
        cpu.fpsr = 0;
        cpu.set_vector(1, 0x3c00);
        cpu.set_vector(2, 0x3c00);
        execute(&mut cpu, 0x5e42_3c20);
        assert_eq!(cpu.vector(0), 0x3c00);
        cpu.pc = 0;
        cpu.set_vector(0, u128::MAX);
        execute(&mut cpu, 0x5ef9_d820);
        assert_eq!(cpu.vector(0), 0x3bfc);
        cpu.pc = 0;
        cpu.set_vector(1, 0x4000_3c00_3800_3400);
        execute(&mut cpu, 0x0ef9_d820);
        assert_eq!(cpu.vector(0) >> 64, 0);
    }

    #[test]
    fn retained_step_digest() {
        const SINGLE: &[u64] = &[
            0,
            0x8000_0000,
            0x7f80_0000,
            0xff80_0000,
            0x7fc0_0000,
            0xffc0_0000,
            0x7f80_0001,
            0x7fff_ffff,
            0x0080_0000,
            0x007f_ffff,
            1,
            0x0040_0000,
            0x0020_0000,
            0x001f_ffff,
            0x7f7f_ffff,
            0xff7f_ffff,
            0x3f80_0000,
            0xbf80_0000,
            0x4000_0000,
            0x7e80_0000,
            0x7e7f_ffff,
            0x0100_0000,
            0x3f00_0000,
            0xc049_0fdb,
        ];
        const DOUBLE: &[u64] = &[
            0,
            0x8000_0000_0000_0000,
            0x7ff0_0000_0000_0000,
            0xfff0_0000_0000_0000,
            0x7ff8_0000_0000_0000,
            0xfff8_0000_0000_0000,
            0x7ff0_0000_0000_0001,
            0x0010_0000_0000_0000,
            0x000f_ffff_ffff_ffff,
            1,
            0x0008_0000_0000_0000,
            0x0003_ffff_ffff_ffff,
            0x7fef_ffff_ffff_ffff,
            0x3ff0_0000_0000_0000,
            0xbff0_0000_0000_0000,
            0x4000_0000_0000_0000,
            0x7fd0_0000_0000_0000,
            0x3fe0_0000_0000_0000,
        ];
        const HALF: &[u64] = &[
            0, 0x8000, 0x7c00, 0xfc00, 0x7e00, 0xfe00, 0x7c01, 0x0400, 0x03ff, 1, 0x0200, 0x00ff, 0x7bff, 0x3c00,
            0xbc00, 0x4000, 0x6800, 0x67ff, 0x3800, 0xc248,
        ];
        let forms = [
            (0x4e22_fc20, SINGLE, 32, 0xb406_00ca_94a8_7f90),
            (0x4e62_fc20, DOUBLE, 64, 0x28c6_b97d_9434_474c),
            (0x4e42_3c20, HALF, 16, 0xa35f_e5c7_84a4_6740),
            (0x4ea2_fc20, SINGLE, 32, 0x0228_54b9_972e_4a90),
            (0x4ee2_fc20, DOUBLE, 64, 0x2764_f407_a6ef_87d4),
            (0x4ec2_3c20, HALF, 16, 0xa593_3894_1a8e_fe44),
            (0x5e22_fc20, SINGLE, 32, 0xe445_073a_5830_62cc),
            (0x5e62_fc20, DOUBLE, 64, 0xf74f_89d8_9716_4302),
            (0x5e42_3c20, HALF, 16, 0x6dd8_d2fc_d12e_e536),
            (0x5ea2_fc20, SINGLE, 32, 0x640c_189e_c0ac_5a08),
            (0x5ee2_fc20, DOUBLE, 64, 0x6626_97a8_2898_d0a0),
            (0x5ec2_3c20, HALF, 16, 0x204f_f6b1_bb99_5d1a),
        ];
        let mut digest = 0x0892_a88b_4d7d_b11c_u64;
        for (word, inputs, bits, expected) in forms {
            for fpcr in [0, 1 << 22, 2 << 22, 3 << 22, 1 << 24, 1 << 25, 1 << 19, 0x03c8_0000] {
                for i in 0..inputs.len() {
                    for j in (1..inputs.len()).step_by(3) {
                        let mut cpu = Aarch64CpuState {
                            fpcr,
                            ..Default::default()
                        };
                        cpu.set_vector(1, fill(inputs, i, bits));
                        cpu.set_vector(2, fill(inputs, (i + j) % inputs.len(), bits));
                        cpu.set_vector(0, 0x5a5a_5a5a_5a5a_5a5a_a5a5_a5a5_a5a5_a5a5);
                        execute(&mut cpu, word);
                        let output = cpu.vector(0);
                        digest = (digest ^ output as u64 ^ (output >> 64) as u64 ^ (cpu.fpsr << 40))
                            .wrapping_mul(0x100_0000_01b3);
                    }
                }
            }
            assert_eq!(digest, expected, "word={word:#010x}");
        }
    }

    fn fill(inputs: &[u64], start: usize, bits: u8) -> u128 {
        let lanes = 128 / usize::from(bits);
        let mask = if bits == 64 { u64::MAX } else { (1_u64 << bits) - 1 };
        (0..lanes).fold(0_u128, |all, lane| {
            all | u128::from(inputs[(start + lane) % inputs.len()] & mask) << (lane * usize::from(bits))
        })
    }

    fn recip_reference(a: u32) -> u32 {
        ((1_u32 << 19) / (2 * a + 1)).div_ceil(2)
    }
    fn sqrt_reference(a: u32) -> u32 {
        let scale = if a < 256 { 2 * a + 1 } else { 2 * (2 * (a / 2) + 1) };
        let mut candidate = 512_u64;
        while u64::from(scale) * (candidate + 1).pow(2) < 1 << 28 {
            candidate += 1;
        }
        candidate.div_ceil(2) as u32
    }
    fn normal_reference(bits: u64, format: FpFormat, sqrt: bool) -> u64 {
        let mantissa = Reciprocal::mantissa(format);
        let bias = Reciprocal::bias(format);
        let exponent = (bits >> mantissa & Reciprocal::inf_exp(format)) as i32;
        let fraction = (bits & Reciprocal::fraction_mask(format)) << (52 - mantissa);
        if sqrt {
            let index = if exponent & 1 != 0 {
                128 + (fraction >> 45) as u32
            } else {
                256 + (fraction >> 44) as u32
            };
            return (((3 * bias - 1 - exponent) / 2) as u64) << mantissa
                | u64::from(sqrt_reference(index) & 255) << (mantissa - 8);
        }
        let estimate = recip_reference(256 + (fraction >> 44) as u32);
        let result_exponent = 2 * bias - 1 - exponent;
        (result_exponent as u64) << mantissa | u64::from(estimate & 255) << (mantissa - 8)
    }
    fn exponent_reference(bits: u64, format: FpFormat) -> u64 {
        let mantissa = Reciprocal::mantissa(format);
        let infinity = Reciprocal::inf_exp(format);
        let exponent = bits >> mantissa & infinity;
        bits & Reciprocal::sign(format)
            | (if exponent == 0 {
                infinity - 1
            } else {
                !exponent & infinity
            }) << mantissa
    }
    fn unsigned_reference(value: u32, sqrt: bool) -> u32 {
        if sqrt && value < 1 << 30 || !sqrt && value < 1 << 31 {
            return u32::MAX;
        }
        let index = value >> 23;
        if !sqrt {
            let divisor = index * 2 + 1;
            return ((1_u32 << 19) / divisor).div_ceil(2) << 23;
        }
        let scaled = if index < 256 {
            index * 2 + 1
        } else {
            (index / 2 * 2 + 1) * 2
        };
        let mut candidate = 512_u64;
        while u64::from(scaled) * (candidate + 1) * (candidate + 1) < 1 << 28 {
            candidate += 1;
        }
        (candidate.div_ceil(2) as u32) << 23
    }
    fn unsigned_indexes(sqrt: bool, tail: u32) {
        let first = if sqrt { 128 } else { 256 };
        for index in first..512 {
            let value = index << 23 | tail;
            assert_eq!(Reciprocal::unsigned_lane(value, sqrt), unsigned_reference(value, sqrt));
        }
    }
    fn exponent_reference_nan(bits: u64, format: FpFormat, fpcr: u32) -> (u64, u32) {
        let class = Reciprocal::class(bits, format);
        if class < 4 {
            return (exponent_reference(bits, format), 0);
        }
        let flags = if class == 5 { FPSR_INVALID } else { 0 };
        let value = if fpcr & 1 << 25 != 0 {
            Reciprocal::default_nan(format)
        } else {
            bits | 1 << (Reciprocal::mantissa(format) - 1)
        };
        (value, flags)
    }
    fn exponent_format(format: FpFormat, sign: u64) {
        let mantissa = Reciprocal::mantissa(format);
        let infinity = Reciprocal::inf_exp(format);
        for exponent in 0..=infinity {
            let bits = sign | exponent << mantissa | u64::from(exponent != infinity);
            assert_eq!(
                Reciprocal::exponent_lane(bits, format, 0),
                (exponent_reference(bits, format), 0)
            );
        }
    }
    fn exponent_samples(state: &mut u64, format: FpFormat, fpcr: u32) {
        for _ in 0..20_000 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            let width_mask = if format == FpFormat::Double {
                u64::MAX
            } else {
                (1_u64 << format.bits()) - 1
            };
            let bits = *state & width_mask;
            let (expected, mut flags) = exponent_reference_nan(bits, format, fpcr);
            let denormal = Reciprocal::class(bits, format) == 1 && Reciprocal::flush(fpcr, format);
            if denormal && format != FpFormat::Half {
                flags |= FPSR_INPUT_DENORMAL;
            }
            assert_eq!(Reciprocal::exponent_lane(bits, format, fpcr), (expected, flags));
        }
    }
    fn random_format(state: &mut u64, format: FpFormat, sqrt: bool) {
        for _ in 0..20_000 {
            *state ^= *state << 13;
            *state ^= *state >> 7;
            *state ^= *state << 17;
            let mantissa = Reciprocal::mantissa(format);
            let bias = Reciprocal::bias(format) as u64;
            let exponent = 2 + *state % (2 * bias - 4);
            let bits = exponent << mantissa | *state & Reciprocal::fraction_mask(format);
            assert_eq!(
                Reciprocal::estimate_lane(bits, format, sqrt, 0),
                (normal_reference(bits, format, sqrt), 0)
            );
        }
    }
    fn execute(cpu: &mut Aarch64CpuState, word: u32) {
        assert_eq!(
            Aarch64FpExecutor::execute_word(cpu, &mut Aarch64SoftFloat, word),
            Aarch64ExecutionExit::Continue
        );
    }
}
