use crate::{Aarch64CpuState, Aarch64DecodeError, Aarch64Instruction, FpComparison, FpFormat};

pub(crate) struct Comparison;

impl Comparison {
    pub(crate) fn execute(
        cpu: &Aarch64CpuState,
        staged: &mut Aarch64CpuState,
        operation: FpComparison,
        format: FpFormat,
        left: u8,
        right: Option<u8>,
        destination: u8,
        lanes: u8,
        absolute: bool,
    ) {
        let mut value = 0_u128;
        let sign = 1_u64 << (format.bits() - 1);
        let magnitude_mask = u64::MAX ^ (u64::from(absolute) * sign);
        let lane_mask = u64::MAX >> (64 - format.bits());
        for lane in 0..lanes {
            let left_value = cpu.vector_lane(left, format.bits(), lane) & magnitude_mask;
            let right_value =
                right.map_or(0, |register| cpu.vector_lane(register, format.bits(), lane)) & magnitude_mask;
            let mut compared = cpu.clone();
            crate::Aarch64FpExecutor::compare(
                &mut compared,
                format,
                left_value,
                right_value,
                operation != FpComparison::Equal,
            );
            staged.fpsr |= compared.fpsr;
            value |= (u128::from(lane_mask) * u128::from(Self::matches(operation, compared.nzcv))) << (u32::from(lane) * u32::from(format.bits()));
        }
        staged.set_vector(destination, value);
    }

    fn matches(operation: FpComparison, flags: crate::Nzcv) -> bool {
        let unordered = !flags.negative() && !flags.zero() && flags.carry() && flags.overflow();
        match operation {
            FpComparison::Equal => flags.zero(),
            FpComparison::GreaterEqual => flags.negative() == flags.overflow(),
            FpComparison::Greater => !flags.zero() && flags.negative() == flags.overflow(),
            FpComparison::LessEqual => !unordered && (flags.zero() || flags.negative() != flags.overflow()),
            FpComparison::Less => !unordered && flags.negative() != flags.overflow(),
        }
    }

    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        let q = word >> 30 & 1 != 0;
        let scalar = word >> 28 & 1 != 0;
        let register_key = word & !(1 << 30 | 31 << 16 | 31 << 5 | 31);
        let zero_key = word & !(1 << 30 | 31 << 5 | 31);
        let (operation, format, absolute, zero) = Self::register(register_key).or_else(|| Self::zero(zero_key))?;
        if scalar && !q {
            return Some(Err(Aarch64DecodeError::Reserved));
        }
        if !scalar && format == FpFormat::Double && !q {
            return Some(Err(Aarch64DecodeError::Reserved));
        }
        let lanes = if scalar {
            1
        } else {
            (if q { 128 } else { 64 }) / format.bits()
        };
        Some(Ok(Aarch64Instruction::SimdFpCompare {
            operation,
            format,
            left: (word >> 5 & 31) as u8,
            right: (!zero).then_some((word >> 16 & 31) as u8),
            destination: (word & 31) as u8,
            lanes,
            absolute,
        }))
    }

    fn register(key: u32) -> Option<(FpComparison, FpFormat, bool, bool)> {
        let vector_key = key & !(1 << 28);
        let (operation, format, absolute) = match vector_key {
            0x0e40_2400 => (FpComparison::Equal, FpFormat::Half, false),
            0x2e40_2400 => (FpComparison::GreaterEqual, FpFormat::Half, false),
            0x2ec0_2400 => (FpComparison::Greater, FpFormat::Half, false),
            0x2e40_2c00 => (FpComparison::GreaterEqual, FpFormat::Half, true),
            0x2ec0_2c00 => (FpComparison::Greater, FpFormat::Half, true),
            0x0e20_e400 => (FpComparison::Equal, FpFormat::Single, false),
            0x2e20_e400 => (FpComparison::GreaterEqual, FpFormat::Single, false),
            0x2ea0_e400 => (FpComparison::Greater, FpFormat::Single, false),
            0x2e20_ec00 => (FpComparison::GreaterEqual, FpFormat::Single, true),
            0x2ea0_ec00 => (FpComparison::Greater, FpFormat::Single, true),
            0x0e60_e400 => (FpComparison::Equal, FpFormat::Double, false),
            0x2e60_e400 => (FpComparison::GreaterEqual, FpFormat::Double, false),
            0x2ee0_e400 => (FpComparison::Greater, FpFormat::Double, false),
            0x2e60_ec00 => (FpComparison::GreaterEqual, FpFormat::Double, true),
            0x2ee0_ec00 => (FpComparison::Greater, FpFormat::Double, true),
            _ => return None,
        };
        Some((operation, format, absolute, false))
    }

    fn zero(key: u32) -> Option<(FpComparison, FpFormat, bool, bool)> {
        let vector_key = key & !(1 << 28);
        let (operation, format) = match vector_key {
            0x0ef8_d800 => (FpComparison::Equal, FpFormat::Half),
            0x2ef8_c800 => (FpComparison::GreaterEqual, FpFormat::Half),
            0x0ef8_c800 => (FpComparison::Greater, FpFormat::Half),
            0x2ef8_d800 => (FpComparison::LessEqual, FpFormat::Half),
            0x0ef8_e800 => (FpComparison::Less, FpFormat::Half),
            0x0ea0_d800 => (FpComparison::Equal, FpFormat::Single),
            0x2ea0_c800 => (FpComparison::GreaterEqual, FpFormat::Single),
            0x0ea0_c800 => (FpComparison::Greater, FpFormat::Single),
            0x2ea0_d800 => (FpComparison::LessEqual, FpFormat::Single),
            0x0ea0_e800 => (FpComparison::Less, FpFormat::Single),
            0x0ee0_d800 => (FpComparison::Equal, FpFormat::Double),
            0x2ee0_c800 => (FpComparison::GreaterEqual, FpFormat::Double),
            0x0ee0_c800 => (FpComparison::Greater, FpFormat::Double),
            0x2ee0_d800 => (FpComparison::LessEqual, FpFormat::Double),
            0x0ee0_e800 => (FpComparison::Less, FpFormat::Double),
            _ => return None,
        };
        Some((operation, format, false, true))
    }
}

#[cfg(test)]
mod test {
    use super::Comparison;
    use crate::{
        Aarch64CpuState, Aarch64DecodeError, Aarch64Decoder, Aarch64ExecutionExit, Aarch64FpExecutor,
        Aarch64Instruction, Aarch64SoftFloat, FPSR_INPUT_DENORMAL, FPSR_INVALID, FpComparison, FpFormat, Nzcv,
    };

    const REGISTER: [(u32, FpComparison, FpFormat, bool); 15] = [
        (0x0e40_2400, FpComparison::Equal, FpFormat::Half, false),
        (0x2e40_2400, FpComparison::GreaterEqual, FpFormat::Half, false),
        (0x2ec0_2400, FpComparison::Greater, FpFormat::Half, false),
        (0x2e40_2c00, FpComparison::GreaterEqual, FpFormat::Half, true),
        (0x2ec0_2c00, FpComparison::Greater, FpFormat::Half, true),
        (0x0e20_e400, FpComparison::Equal, FpFormat::Single, false),
        (0x2e20_e400, FpComparison::GreaterEqual, FpFormat::Single, false),
        (0x2ea0_e400, FpComparison::Greater, FpFormat::Single, false),
        (0x2e20_ec00, FpComparison::GreaterEqual, FpFormat::Single, true),
        (0x2ea0_ec00, FpComparison::Greater, FpFormat::Single, true),
        (0x0e60_e400, FpComparison::Equal, FpFormat::Double, false),
        (0x2e60_e400, FpComparison::GreaterEqual, FpFormat::Double, false),
        (0x2ee0_e400, FpComparison::Greater, FpFormat::Double, false),
        (0x2e60_ec00, FpComparison::GreaterEqual, FpFormat::Double, true),
        (0x2ee0_ec00, FpComparison::Greater, FpFormat::Double, true),
    ];

    const ZERO: [(u32, FpComparison, FpFormat); 15] = [
        (0x0ef8_d800, FpComparison::Equal, FpFormat::Half),
        (0x2ef8_c800, FpComparison::GreaterEqual, FpFormat::Half),
        (0x0ef8_c800, FpComparison::Greater, FpFormat::Half),
        (0x2ef8_d800, FpComparison::LessEqual, FpFormat::Half),
        (0x0ef8_e800, FpComparison::Less, FpFormat::Half),
        (0x0ea0_d800, FpComparison::Equal, FpFormat::Single),
        (0x2ea0_c800, FpComparison::GreaterEqual, FpFormat::Single),
        (0x0ea0_c800, FpComparison::Greater, FpFormat::Single),
        (0x2ea0_d800, FpComparison::LessEqual, FpFormat::Single),
        (0x0ea0_e800, FpComparison::Less, FpFormat::Single),
        (0x0ee0_d800, FpComparison::Equal, FpFormat::Double),
        (0x2ee0_c800, FpComparison::GreaterEqual, FpFormat::Double),
        (0x0ee0_c800, FpComparison::Greater, FpFormat::Double),
        (0x2ee0_d800, FpComparison::LessEqual, FpFormat::Double),
        (0x0ee0_e800, FpComparison::Less, FpFormat::Double),
    ];

    #[test]
    fn register_decode() {
        for (base, operation, format, absolute) in REGISTER {
            check_register(base, operation, format, absolute);
        }
    }

    fn check_register(base: u32, operation: FpComparison, format: FpFormat, absolute: bool) {
        for raw in 0_u32..(4 * 32 * 32 * 32) {
            let destination = raw & 31;
            let left = raw >> 5 & 31;
            let right = raw >> 10 & 31;
            let q = raw >> 15 & 1 != 0;
            let scalar = raw >> 16 & 1 != 0;
            let word = base | u32::from(scalar) << 28 | u32::from(q) << 30 | right << 16 | left << 5 | destination;
            let decoded = Comparison::decode(word).expect("owned class");
            let reserved = scalar && !q || !scalar && format == FpFormat::Double && !q;
            if reserved {
                assert_eq!(decoded, Err(Aarch64DecodeError::Reserved));
                continue;
            }
            assert_eq!(
                decoded,
                Ok(Aarch64Instruction::SimdFpCompare {
                    operation,
                    format,
                    left: left as u8,
                    right: Some(right as u8),
                    destination: destination as u8,
                    lanes: if scalar {
                        1
                    } else {
                        (if q { 128 } else { 64 }) / format.bits()
                    },
                    absolute
                })
            );
        }
    }

    #[test]
    fn zero_decode() {
        for (base, operation, format) in ZERO {
            check_zero(base, operation, format);
        }
    }

    fn check_zero(base: u32, operation: FpComparison, format: FpFormat) {
        for raw in 0_u32..(4 * 32 * 32) {
            let destination = raw & 31;
            let source = raw >> 5 & 31;
            let q = raw >> 10 & 1 != 0;
            let scalar = raw >> 11 & 1 != 0;
            let word = base | u32::from(scalar) << 28 | u32::from(q) << 30 | source << 5 | destination;
            let decoded = Comparison::decode(word).expect("owned class");
            let reserved = scalar && !q || !scalar && format == FpFormat::Double && !q;
            if reserved {
                assert_eq!(decoded, Err(Aarch64DecodeError::Reserved));
                continue;
            }
            assert_eq!(
                decoded,
                Ok(Aarch64Instruction::SimdFpCompare {
                    operation,
                    format,
                    left: source as u8,
                    right: None,
                    destination: destination as u8,
                    lanes: if scalar {
                        1
                    } else {
                        (if q { 128 } else { 64 }) / format.bits()
                    },
                    absolute: false
                })
            );
        }
    }

    #[test]
    fn frontier_advances() {
        let word = 0x6e3e_e7bf;
        assert!(matches!(
            Aarch64Decoder::decode(word),
            Ok(crate::Aarch64Ir {
                instruction: Aarch64Instruction::SimdFpCompare {
                    operation: FpComparison::GreaterEqual,
                    format: FpFormat::Single,
                    left: 29,
                    right: Some(30),
                    destination: 31,
                    lanes: 4,
                    absolute: false,
                },
                ..
            })
        ));
        let mut cpu = Aarch64CpuState {
            pc: 0x400858,
            ..Default::default()
        };
        cpu.set_vector(29, 0x4080_0000_3f80_0000_c000_0000_0000_0000);
        cpu.set_vector(30, 0x4040_0000_3f80_0000_bf80_0000_8000_0000);
        assert_eq!(
            Aarch64FpExecutor::execute_word(&mut cpu, &mut Aarch64SoftFloat, word),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector(31), u128::MAX ^ u128::from(u32::MAX) << 32);
        assert_eq!(cpu.pc, 0x40085c);
    }

    #[test]
    fn compare_specials() {
        let mut cpu = Aarch64CpuState {
            pc: 0x800,
            nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY),
            fpsr: 1 << 27,
            ..Default::default()
        };
        // +0 == -0, qNaN is false without IOC, sNaN is false with IOC.
        cpu.set_vector(1, 0x7f80_0001_7fc0_0001_8000_0000_0000_0000);
        cpu.set_vector(2, 0x3f80_0000_3f80_0000_0000_0000_8000_0000);
        Aarch64FpExecutor::execute_word(&mut cpu, &mut Aarch64SoftFloat, 0x4e22_e420);
        assert_eq!(cpu.vector(0), u128::from(u32::MAX) << 32 | u128::from(u32::MAX));
        assert_eq!(cpu.fpsr, 1 << 27 | u64::from(FPSR_INVALID));
        assert_eq!(cpu.nzcv.bits(), Nzcv::NEGATIVE | Nzcv::CARRY);

        // Absolute comparison treats -2 as +2 and snapshots aliases.
        cpu.set_vector(0, 0xc000_0000_3f80_0000_c000_0000_3f80_0000);
        cpu.set_vector(2, 0x3f80_0000_4000_0000_3f80_0000_4000_0000);
        Aarch64FpExecutor::execute_word(&mut cpu, &mut Aarch64SoftFloat, 0x6e22_ec00);
        assert_eq!(cpu.vector(0), u128::from(u32::MAX) << 96 | u128::from(u32::MAX) << 32);
    }

    #[test]
    fn zero_nan_false() {
        for (word, invalid) in [
            (0x4ea0_d820, false),
            (0x6ea0_c820, true),
            (0x4ea0_c820, true),
            (0x6ea0_d820, true),
            (0x4ea0_e820, true),
        ] {
            let mut cpu = Aarch64CpuState::default();
            cpu.set_vector(1, 0x7fc0_0001_7fc0_0001_7fc0_0001_7fc0_0001);
            Aarch64FpExecutor::execute_word(&mut cpu, &mut Aarch64SoftFloat, word);
            assert_eq!(cpu.vector(0), 0, "{word:#010x}");
            assert_eq!(cpu.fpsr, u64::from(invalid) * u64::from(FPSR_INVALID), "{word:#010x}");
        }
    }

    #[test]
    fn flush_input() {
        let mut cpu = Aarch64CpuState {
            fpcr: 1 << 24,
            ..Default::default()
        };
        cpu.set_vector(1, 0x0000_0001_8000_0001);
        Aarch64FpExecutor::execute_word(&mut cpu, &mut Aarch64SoftFloat, 0x0ea0_d820);
        assert_eq!(cpu.vector(0), u128::from(u64::MAX));
        assert_eq!(cpu.fpsr, u64::from(FPSR_INPUT_DENORMAL));
    }

    #[test]
    fn format_execution() {
        let mut half = Aarch64CpuState::default();
        half.set_vector(1, 0x3c00_c000_3c00_c000);
        half.set_vector(2, 0x3c00_bc00_bc00_c000);
        Aarch64FpExecutor::execute_word(&mut half, &mut Aarch64SoftFloat, 0x2e42_2420);
        assert_eq!(half.vector(0), 0xffff_0000_ffff_ffff);

        let mut double = Aarch64CpuState::default();
        double.set_vector(1, 0x3ff0_0000_0000_0000_bff0_0000_0000_0000);
        double.set_vector(2, 0x4000_0000_0000_0000_bff0_0000_0000_0000);
        Aarch64FpExecutor::execute_word(&mut double, &mut Aarch64SoftFloat, 0x6e62_e420);
        assert_eq!(double.vector(0), u128::from(u64::MAX));

        double.set_vector(0, u128::MAX);
        Aarch64FpExecutor::execute_word(&mut double, &mut Aarch64SoftFloat, 0x7e62_e420);
        assert_eq!(double.vector(0), u128::from(u64::MAX));
    }

    #[test]
    fn reserved_rollback() {
        for word in [0x2e62_e420, 0x3e62_e420] {
            let mut cpu = Aarch64CpuState {
                pc: 0x900,
                nzcv: Nzcv::from_bits(0xb000_0000),
                fpsr: 0x0800_009f,
                ..Default::default()
            };
            cpu.set_vector(0, u128::MAX);
            cpu.set_vector(1, 0x1234);
            cpu.set_vector(2, 0x5678);
            let before = cpu.clone();
            assert_eq!(
                Aarch64FpExecutor::execute_word(&mut cpu, &mut Aarch64SoftFloat, word),
                Aarch64ExecutionExit::UndefinedInstruction {
                    instruction: 0x900,
                    word
                }
            );
            assert_eq!(cpu, before);
        }
    }
}
