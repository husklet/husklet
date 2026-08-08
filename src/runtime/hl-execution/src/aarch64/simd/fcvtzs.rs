use crate::{Aarch64DecodeError, Aarch64Instruction, FpFormat, FpRoundingMode};

pub(crate) struct Fcvtzs;

impl Fcvtzs {
    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        let scalar = word & 1 << 28 != 0;
        if scalar && word & 1 << 30 == 0 {
            return None;
        }
        let rounded = match word & 0x8fbf_fc00 {
            0x0e21_a800 => Some(FpRoundingMode::NearestEven),
            0x0e21_b800 => Some(FpRoundingMode::NegativeInfinity),
            0x0ea1_a800 => Some(FpRoundingMode::PositiveInfinity),
            0x0e21_c800 => Some(FpRoundingMode::NearestAway),
            0x0ea1_b800 => Some(FpRoundingMode::Zero),
            _ => None,
        };
        if let Some(rounding) = rounded {
            let double = word & 1 << 22 != 0;
            let wide = word & 1 << 30 != 0;
            if double && !wide {
                return Some(Err(Aarch64DecodeError::Reserved));
            }
            return Some(Ok(Self::instruction(
                word,
                double,
                wide,
                scalar,
                word & 1 << 29 == 0,
                0,
                rounding,
            )));
        }
        if word & 0xbf80_fc00 != 0x0f00_fc00 {
            return None;
        }
        let immediate = (word >> 16 & 0x7f) as u8;
        // immh == 0000 belongs to AdvSIMD modified-immediate encodings. In
        // particular, o2 selects the FP16 FMOV family in this overlapping
        // class; let that decoder decide whether the complete word is valid.
        if immediate < 32 {
            return None;
        }
        let double = immediate & 0x40 != 0;
        let wide = word & 1 << 30 != 0;
        if double && !wide {
            return Some(Err(Aarch64DecodeError::Reserved));
        }
        let scale = if double { 128 - immediate } else { 64 - immediate };
        Some(Ok(Self::instruction(
            word,
            double,
            wide,
            false,
            true,
            scale,
            FpRoundingMode::Zero,
        )))
    }

    fn instruction(
        word: u32,
        double: bool,
        wide: bool,
        scalar: bool,
        signed: bool,
        scale: u8,
        rounding: FpRoundingMode,
    ) -> Aarch64Instruction {
        Aarch64Instruction::SimdFpInteger {
            format: if double { FpFormat::Double } else { FpFormat::Single },
            lanes: if scalar {
                1
            } else if double {
                2
            } else if wide {
                4
            } else {
                2
            },
            signed,
            scale,
            rounding,
            source: (word >> 5 & 31) as u8,
            destination: (word & 31) as u8,
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        Aarch64CpuState, Aarch64ExecutionExit, Aarch64FpExecutor, Aarch64SoftFloat, FPSR_INEXACT, FPSR_INVALID,
        FpArithmeticPort, Nzcv,
    };

    #[test]
    fn encodings() {
        for (base, format, lanes, signed) in [
            (0x0ea1_b800, FpFormat::Single, 2, true),
            (0x4ea1_b800, FpFormat::Single, 4, true),
            (0x4ee1_b800, FpFormat::Double, 2, true),
            (0x5ea1_b800, FpFormat::Single, 1, true),
            (0x5ee1_b800, FpFormat::Double, 1, true),
            (0x2ea1_b800, FpFormat::Single, 2, false),
            (0x6ea1_b800, FpFormat::Single, 4, false),
            (0x6ee1_b800, FpFormat::Double, 2, false),
            (0x7ea1_b800, FpFormat::Single, 1, false),
            (0x7ee1_b800, FpFormat::Double, 1, false),
        ] {
            for index in 0_u32..1024 {
                let source = index / 32;
                let destination = index % 32;
                assert_eq!(
                    Fcvtzs::decode(base | source << 5 | destination),
                    Some(Ok(Aarch64Instruction::SimdFpInteger {
                        format,
                        lanes,
                        signed,
                        scale: 0,
                        rounding: FpRoundingMode::Zero,
                        source: source as u8,
                        destination: destination as u8
                    }))
                );
            }
        }
        for arguments in [
            (false, FpFormat::Single, 32_u8, 2_u8),
            (true, FpFormat::Single, 32, 4),
            (true, FpFormat::Double, 64, 2),
        ] {
            fixed_encodings(arguments);
        }
        for (base, signed, rounding) in [
            (0x0e21_a800, true, FpRoundingMode::NearestEven),
            (0x2e21_a800, false, FpRoundingMode::NearestEven),
            (0x0e21_b800, true, FpRoundingMode::NegativeInfinity),
            (0x2e21_b800, false, FpRoundingMode::NegativeInfinity),
            (0x0ea1_a800, true, FpRoundingMode::PositiveInfinity),
            (0x2ea1_a800, false, FpRoundingMode::PositiveInfinity),
            (0x0e21_c800, true, FpRoundingMode::NearestAway),
            (0x2e21_c800, false, FpRoundingMode::NearestAway),
        ] {
            rounded_encodings(base, signed, rounding);
        }
        for word in [0x0ee1_b800, 0x0f7f_fc00] {
            assert_eq!(Fcvtzs::decode(word), Some(Err(Aarch64DecodeError::Reserved)));
        }
        for word in [0x0f00_fc00, 0x0f1f_fc00] {
            assert_eq!(Fcvtzs::decode(word), None);
        }
        assert_eq!(Fcvtzs::decode(0x4ea1_ac00), None);
    }

    fn fixed_encodings((wide, format, maximum, lanes): (bool, FpFormat, u8, u8)) {
        let offset = if format == FpFormat::Double { 128 } else { 64 };
        for scale in 1..=maximum {
            for index in 0_u32..1024 {
                let source = index / 32;
                let destination = index % 32;
                let word =
                    0x0f00_fc00 | u32::from(wide) << 30 | u32::from(offset - scale) << 16 | source << 5 | destination;
                assert_eq!(
                    Fcvtzs::decode(word),
                    Some(Ok(Aarch64Instruction::SimdFpInteger {
                        format,
                        lanes,
                        signed: true,
                        scale,
                        rounding: FpRoundingMode::Zero,
                        source: source as u8,
                        destination: destination as u8
                    }))
                );
            }
        }
    }

    fn rounded_encodings(base: u32, signed: bool, rounding: FpRoundingMode) {
        for (shape, format, lanes) in [
            (0, FpFormat::Single, 2),
            (1 << 30, FpFormat::Single, 4),
            (1 << 30 | 1 << 22, FpFormat::Double, 2),
            (1 << 30 | 1 << 28, FpFormat::Single, 1),
            (1 << 30 | 1 << 28 | 1 << 22, FpFormat::Double, 1),
        ] {
            for index in 0_u32..1024 {
                let source = index / 32;
                let destination = index % 32;
                assert_eq!(
                    Fcvtzs::decode(base | shape | source << 5 | destination),
                    Some(Ok(Aarch64Instruction::SimdFpInteger {
                        format,
                        lanes,
                        signed,
                        scale: 0,
                        rounding,
                        source: source as u8,
                        destination: destination as u8
                    }))
                );
            }
        }
    }

    #[test]
    fn state_aliases() {
        let mut cpu = Aarch64CpuState {
            pc: 0x400,
            nzcv: Nzcv::from_bits(0xa000_0000),
            fpsr: 0x80,
            ..Default::default()
        };
        let input = [
            1.75_f32.to_bits(),
            (-2.75_f32).to_bits(),
            f32::NAN.to_bits(),
            f32::INFINITY.to_bits(),
        ];
        cpu.set_vector(1, pack32(input));
        let ir = Fcvtzs::decode(0x4ea1_b821).unwrap().unwrap();
        assert_eq!(
            Aarch64FpExecutor::execute(
                &mut cpu,
                &mut Aarch64SoftFloat,
                &crate::Aarch64Ir {
                    word: 0x4ea1_b821,
                    wide: true,
                    instruction: ir
                }
            ),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(lanes32(cpu.vector(1)), [1, -2_i32 as u32, 0, i32::MAX as u32]);
        assert_eq!(cpu.fpsr, 0x80 | u64::from(FPSR_INVALID | FPSR_INEXACT));
        assert_eq!(cpu.pc, 0x404);
        assert_eq!(cpu.nzcv.bits(), 0xa000_0000);

        cpu.set_vector(
            2,
            u128::from(1.5_f64.to_bits()) | u128::from((-1.5_f64).to_bits()) << 64,
        );
        let ir = Fcvtzs::decode(0x4f7f_fc42).unwrap().unwrap();
        Aarch64FpExecutor::execute(
            &mut cpu,
            &mut Aarch64SoftFloat,
            &crate::Aarch64Ir {
                word: 0x4f7f_fc42,
                wide: true,
                instruction: ir,
            },
        );
        assert_eq!(cpu.vector(2), u128::from(3_u64) | u128::from((-3_i64) as u64) << 64);
        assert_eq!(cpu.fpsr, 0x80 | u64::from(FPSR_INVALID | FPSR_INEXACT));
    }

    #[test]
    fn reference() {
        let mut random = 0x9e37_79b9_u32;
        let mut values = vec![
            0,
            (-0.0_f32).to_bits(),
            0x0000_0001,
            0x007f_ffff,
            0x7f80_0000,
            0xff80_0000,
            0x7fc0_1234,
            0x4f00_0000,
            0xcf00_0000,
        ];
        for _ in 0..10_000 {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            values.push(random);
        }
        for scale in [0, 1, 7, 16, 31, 32] {
            for bits in &values {
                let (expected, flags) = reference32(*bits, scale);
                let direct = Aarch64SoftFloat.evaluate(crate::FpRequest {
                    operation: crate::FpArithmetic::FloatToScaled {
                        signed: true,
                        width: 32,
                        scale,
                        rounding: FpRoundingMode::Zero,
                    },
                    format: FpFormat::Single,
                    left: u64::from(*bits),
                    right: 0,
                    addend: 0,
                    fpcr: 0,
                });
                let mut cpu = Aarch64CpuState::default();
                cpu.set_vector(1, u128::from(*bits));
                let instruction = Aarch64Instruction::SimdFpInteger {
                    format: FpFormat::Single,
                    lanes: 2,
                    signed: true,
                    scale,
                    rounding: FpRoundingMode::Zero,
                    source: 1,
                    destination: 0,
                };
                Aarch64FpExecutor::execute(
                    &mut cpu,
                    &mut Aarch64SoftFloat,
                    &crate::Aarch64Ir {
                        word: 0,
                        wide: false,
                        instruction,
                    },
                );
                assert_eq!(
                    cpu.vector(0) as u32,
                    expected,
                    "bits={bits:08x} scale={scale} direct={direct:?}"
                );
                assert_eq!(
                    cpu.fpsr as u32 & (FPSR_INVALID | FPSR_INEXACT),
                    flags,
                    "bits={bits:08x} scale={scale}"
                );
            }
        }
        check64();
    }

    #[test]
    fn rounded_reference() {
        let configurations = [
            (true, FpRoundingMode::NearestEven),
            (false, FpRoundingMode::NearestEven),
            (true, FpRoundingMode::NegativeInfinity),
            (false, FpRoundingMode::NegativeInfinity),
            (true, FpRoundingMode::PositiveInfinity),
            (false, FpRoundingMode::PositiveInfinity),
            (true, FpRoundingMode::NearestAway),
            (false, FpRoundingMode::NearestAway),
        ];
        let mut random = 0xa341_316c_u32;
        let mut singles = vec![
            0,
            (-0.0_f32).to_bits(),
            0x3f00_0000,
            0x3fc0_0000,
            0xbf00_0000,
            0xbfc0_0000,
            0x4f00_0000,
            0xcf00_0000,
            0x7fc0_1234,
        ];
        for _ in 0..3_000 {
            random ^= random << 13;
            random ^= random >> 17;
            random ^= random << 5;
            singles.push(random);
        }
        for (signed, rounding) in configurations {
            for bits in &singles {
                let (expected, flags) = rounded32(*bits, signed, rounding);
                let (actual, actual_flags) = evaluate(*bits as u64, FpFormat::Single, signed, rounding);
                assert_eq!(
                    actual as u32, expected,
                    "bits={bits:08x} signed={signed} rounding={rounding:?}"
                );
                assert_eq!(
                    actual_flags, flags,
                    "bits={bits:08x} signed={signed} rounding={rounding:?}"
                );
            }
        }
        check_rounded64(configurations);
    }

    fn check_rounded64(configurations: [(bool, FpRoundingMode); 8]) {
        let mut random = 0x94d0_49bb_1331_11eb_u64;
        let mut doubles = vec![
            0,
            (-0.0_f64).to_bits(),
            0.5_f64.to_bits(),
            1.5_f64.to_bits(),
            (-0.5_f64).to_bits(),
            (-1.5_f64).to_bits(),
            (i64::MIN as f64).to_bits(),
            f64::NAN.to_bits(),
        ];
        for _ in 0..3_000 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            doubles.push(random);
        }
        for (signed, rounding) in configurations {
            for bits in &doubles {
                let (expected, flags) = rounded64(*bits, signed, rounding);
                let (actual, actual_flags) = evaluate(*bits, FpFormat::Double, signed, rounding);
                assert_eq!(
                    actual, expected,
                    "bits={bits:016x} signed={signed} rounding={rounding:?}"
                );
                assert_eq!(
                    actual_flags, flags,
                    "bits={bits:016x} signed={signed} rounding={rounding:?}"
                );
            }
        }
    }

    fn evaluate(bits: u64, format: FpFormat, signed: bool, rounding: FpRoundingMode) -> (u64, u32) {
        let result = Aarch64SoftFloat.evaluate(crate::FpRequest {
            operation: crate::FpArithmetic::FloatToScaled {
                signed,
                width: format.bits(),
                scale: 0,
                rounding,
            },
            format,
            left: bits,
            right: 0,
            addend: 0,
            fpcr: 0,
        });
        (result.value, result.exceptions & (FPSR_INVALID | FPSR_INEXACT))
    }

    fn check64() {
        let mut random = 0xd1b5_4a32_d192_ed03_u64;
        let mut values = vec![
            0,
            (-0.0_f64).to_bits(),
            1,
            0x000f_ffff_ffff_ffff,
            f64::INFINITY.to_bits(),
            f64::NEG_INFINITY.to_bits(),
            f64::NAN.to_bits(),
            (i64::MAX as f64).to_bits(),
            (i64::MIN as f64).to_bits(),
        ];
        for _ in 0..10_000 {
            random ^= random << 13;
            random ^= random >> 7;
            random ^= random << 17;
            values.push(random);
        }
        for scale in [0, 1, 11, 32, 63, 64] {
            for bits in &values {
                let (expected, flags) = reference64(*bits, scale);
                let mut cpu = Aarch64CpuState::default();
                cpu.set_vector(1, u128::from(*bits));
                let instruction = Aarch64Instruction::SimdFpInteger {
                    format: FpFormat::Double,
                    lanes: 2,
                    signed: true,
                    scale,
                    rounding: FpRoundingMode::Zero,
                    source: 1,
                    destination: 0,
                };
                Aarch64FpExecutor::execute(
                    &mut cpu,
                    &mut Aarch64SoftFloat,
                    &crate::Aarch64Ir {
                        word: 0,
                        wide: true,
                        instruction,
                    },
                );
                assert_eq!(cpu.vector(0) as u64, expected, "bits={bits:016x} scale={scale}");
                assert_eq!(
                    cpu.fpsr as u32 & (FPSR_INVALID | FPSR_INEXACT),
                    flags,
                    "bits={bits:016x} scale={scale}"
                );
            }
        }
    }

    // These reference implementations detect FPSR inexactness, which is exact equality by definition.
    #[allow(clippy::float_cmp)]
    fn reference32(bits: u32, scale: u8) -> (u32, u32) {
        let value = f32::from_bits(bits) as f64;
        if value.is_nan() {
            return (0, FPSR_INVALID);
        }
        let scaled = value * 2_f64.powi(i32::from(scale));
        if scaled >= 2_147_483_648.0 {
            return (i32::MAX as u32, FPSR_INVALID);
        }
        if scaled < -2_147_483_648.0 {
            return (i32::MIN as u32, FPSR_INVALID);
        }
        let truncated = scaled.trunc();
        (
            truncated as i32 as u32,
            if truncated == scaled { 0 } else { FPSR_INEXACT },
        )
    }

    #[allow(clippy::float_cmp)]
    fn reference64(bits: u64, scale: u8) -> (u64, u32) {
        let value = f64::from_bits(bits);
        if value.is_nan() {
            return (0, FPSR_INVALID);
        }
        let scaled = value * 2_f64.powi(i32::from(scale));
        if scaled >= 9_223_372_036_854_775_808.0 {
            return (i64::MAX as u64, FPSR_INVALID);
        }
        if scaled < -9_223_372_036_854_775_808.0 {
            return (i64::MIN as u64, FPSR_INVALID);
        }
        let truncated = scaled.trunc();
        (
            truncated as i64 as u64,
            if truncated == scaled { 0 } else { FPSR_INEXACT },
        )
    }

    #[allow(clippy::float_cmp)]
    fn rounded32(bits: u32, signed: bool, rounding: FpRoundingMode) -> (u32, u32) {
        let value = f64::from(f32::from_bits(bits));
        let rounded = round(value, rounding);
        if value.is_nan() {
            return (0, FPSR_INVALID);
        }
        if signed {
            if rounded >= 2_147_483_648.0 {
                return (i32::MAX as u32, FPSR_INVALID);
            }
            if rounded < -2_147_483_648.0 {
                return (i32::MIN as u32, FPSR_INVALID);
            }
            (rounded as i32 as u32, if rounded == value { 0 } else { FPSR_INEXACT })
        } else {
            if rounded >= 4_294_967_296.0 {
                return (u32::MAX, FPSR_INVALID);
            }
            if rounded < 0.0 {
                return (0, FPSR_INVALID);
            }
            (rounded as u32, if rounded == value { 0 } else { FPSR_INEXACT })
        }
    }

    #[allow(clippy::float_cmp)]
    fn rounded64(bits: u64, signed: bool, rounding: FpRoundingMode) -> (u64, u32) {
        let value = f64::from_bits(bits);
        let rounded = round(value, rounding);
        if value.is_nan() {
            return (0, FPSR_INVALID);
        }
        if signed {
            if rounded >= 9_223_372_036_854_775_808.0 {
                return (i64::MAX as u64, FPSR_INVALID);
            }
            if rounded < -9_223_372_036_854_775_808.0 {
                return (i64::MIN as u64, FPSR_INVALID);
            }
            (rounded as i64 as u64, if rounded == value { 0 } else { FPSR_INEXACT })
        } else {
            if rounded >= 18_446_744_073_709_551_616.0 {
                return (u64::MAX, FPSR_INVALID);
            }
            if rounded < 0.0 {
                return (0, FPSR_INVALID);
            }
            (rounded as u64, if rounded == value { 0 } else { FPSR_INEXACT })
        }
    }

    fn round(value: f64, rounding: FpRoundingMode) -> f64 {
        match rounding {
            FpRoundingMode::NearestEven => value.round_ties_even(),
            FpRoundingMode::NegativeInfinity => value.floor(),
            FpRoundingMode::PositiveInfinity => value.ceil(),
            FpRoundingMode::NearestAway => value.round(),
            _ => value.trunc(),
        }
    }

    fn pack32(lanes: [u32; 4]) -> u128 {
        lanes
            .into_iter()
            .enumerate()
            .fold(0, |value, (lane, bits)| value | u128::from(bits) << (lane * 32))
    }
    fn lanes32(value: u128) -> [u32; 4] {
        std::array::from_fn(|lane| (value >> (lane * 32)) as u32)
    }
}
