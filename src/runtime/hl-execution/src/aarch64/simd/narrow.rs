use crate::{Aarch64CpuState, Aarch64Instruction, FPSR_INEXACT, FpArithmetic, FpArithmeticPort, FpFormat, FpRequest};

pub(crate) struct NarrowOdd;

impl NarrowOdd {
    pub(crate) fn decode(word: u32) -> Option<Aarch64Instruction> {
        let (high, scalar) = match word & 0xffff_fc00 {
            0x2e61_6800 => (false, false),
            0x6e61_6800 => (true, false),
            0x7e61_6800 => (false, true),
            _ => return None,
        };
        Some(Aarch64Instruction::SimdFpNarrowOdd {
            source: (word >> 5 & 31) as u8,
            destination: (word & 31) as u8,
            high,
            scalar,
        })
    }

    pub(crate) fn execute<P: FpArithmeticPort>(
        cpu: &Aarch64CpuState,
        staged: &mut Aarch64CpuState,
        port: &mut P,
        source: u8,
        destination: u8,
        high: bool,
        scalar: bool,
    ) {
        let lanes = if scalar { 1 } else { 2 };
        let mut narrowed = 0_u64;
        for lane in 0..lanes {
            let result = port.evaluate(FpRequest {
                operation: FpArithmetic::ConvertFormat {
                    destination: FpFormat::Single,
                },
                format: FpFormat::Double,
                left: cpu.vector_lane(source, 64, lane),
                right: 0,
                addend: 0,
                fpcr: (cpu.fpcr as u32 & !(3 << 22)) | 3 << 22,
            });
            let value = result.value as u32 | u32::from(result.exceptions & FPSR_INEXACT != 0);
            narrowed |= u64::from(value) << (lane * 32);
            staged.fpsr |= u64::from(result.exceptions);
        }
        let value = if high {
            u128::from(cpu.vector(destination) as u64) | u128::from(narrowed) << 64
        } else {
            u128::from(narrowed)
        };
        staged.set_vector(destination, value);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Aarch64ExecutionExit, Aarch64FpExecutor, Aarch64SoftFloat, Nzcv};

    #[test]
    fn encodings() {
        for (base, high, scalar) in [
            (0x2e61_6800, false, false),
            (0x6e61_6800, true, false),
            (0x7e61_6800, false, true),
        ] {
            for encoded in 0_u32..1024 {
                assert_eq!(
                    NarrowOdd::decode(base | encoded),
                    Some(Aarch64Instruction::SimdFpNarrowOdd {
                        source: (encoded >> 5) as u8,
                        destination: (encoded & 31) as u8,
                        high,
                        scalar,
                    })
                );
            }
        }
        for word in [0x0e61_6800, 0x2e21_6800, 0x2e61_7800, 0x7e61_6c00] {
            assert_eq!(NarrowOdd::decode(word), None);
        }
    }

    #[test]
    fn lanes_alias_and_round_odd() {
        let inexact = 0x3ff0_0000_1000_0000_u64;
        let mut cpu = Aarch64CpuState {
            pc: 0x401000,
            fpcr: u64::MAX,
            fpsr: 0x80,
            nzcv: Nzcv::from_bits(0x9000_0000),
            ..Default::default()
        };
        cpu.set_vector(1, u128::from(inexact) << 64 | u128::from(1.0_f64.to_bits()));
        execute(&mut cpu, 0x2e61_6820);
        assert_eq!(cpu.vector(0), 0x3f80_0001_3f80_0000);
        assert_ne!(cpu.fpsr & u64::from(FPSR_INEXACT), 0);
        assert_eq!(cpu.nzcv.bits(), 0x9000_0000);

        cpu.pc = 0;
        cpu.set_vector(0, 0xfeed_face_cafe_beef_0123_4567_89ab_cdef);
        execute(&mut cpu, 0x6e61_6820);
        assert_eq!(cpu.vector(0), 0x3f80_0001_3f80_0000_0123_4567_89ab_cdef);

        cpu.pc = 0;
        cpu.set_vector(0, u128::MAX);
        execute(&mut cpu, 0x7e61_6820);
        assert_eq!(cpu.vector(0), 0x3f80_0000);
    }

    #[test]
    fn specials_ties_and_modes() {
        let cases: &[(u64, u32, u32, u32)] = &[
            (0x7ff0_0000_0000_0000, 0, 0x7f80_0000, 0),
            (0xfff0_0000_0000_0000, 0, 0xff80_0000, 0),
            (0x8000_0000_0000_0000, 0, 0x8000_0000, 0),
            (0x3ff0_0000_0000_0000, 0, 0x3f80_0000, 0),
            (0x3ff0_0000_1000_0000, 0, 0x3f80_0001, FPSR_INEXACT),
            (0xbff0_0000_1000_0000, 0, 0xbf80_0001, FPSR_INEXACT),
            (0x3ff0_0000_2000_0000, 0, 0x3f80_0001, 0),
            (0x7fe0_0000_0000_0000, 0, 0x7f7f_ffff, FPSR_INEXACT),
            (1, 0, 1, FPSR_INEXACT),
            (0x7ff8_1234_5678_9abc, 1 << 25, 0x7fc0_0000, 0),
            (0x7ff0_1234_5678_9abc, 0, 0x7fc0_91a2, crate::FPSR_INVALID),
        ];
        for &(bits, fpcr, expected, required) in cases {
            let mut cpu = Aarch64CpuState {
                fpcr: fpcr.into(),
                ..Default::default()
            };
            cpu.set_vector(1, u128::from(bits));
            execute(&mut cpu, 0x7e61_6820);
            assert_eq!(cpu.vector(0), u128::from(expected), "bits={bits:#018x} fpcr={fpcr:#x}");
            assert_eq!(cpu.fpsr as u32 & required, required, "bits={bits:#018x} fpcr={fpcr:#x}");
        }
    }

    #[test]
    fn retained_digest() {
        const INPUTS: &[u64] = &[
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
        let mut digest = 0x204f_f6b1_bb99_5d1a_u64;
        for fpcr in [0, 1 << 22, 2 << 22, 3 << 22, 1 << 24, 1 << 25, 1 << 19, 0x03c8_0000] {
            for i in 0..INPUTS.len() {
                let source = u128::from(INPUTS[i]) | u128::from(INPUTS[(i + 1) % INPUTS.len()]) << 64;
                for (word, seed, high) in [
                    (0x2e61_6820, 0_u128, false),
                    (0x6e61_6820, 0x0123_4567_89ab_cdef_u128, true),
                    (0x7e61_6820, 0x5a5a_5a5a_5a5a_5a5a_a5a5_a5a5_a5a5_a5a5_u128, false),
                ] {
                    let mut cpu = Aarch64CpuState {
                        fpcr,
                        ..Default::default()
                    };
                    cpu.set_vector(1, source);
                    cpu.set_vector(0, seed);
                    execute(&mut cpu, word);
                    let output = if high {
                        (cpu.vector(0) >> 64) as u64
                    } else {
                        cpu.vector(0) as u64
                    };
                    digest = (digest ^ output ^ (cpu.fpsr << 40)).wrapping_mul(0x100_0000_01b3);
                }
            }
        }
        assert_eq!(digest, 0x2eb6_af06_d504_9d82);
    }

    fn execute(cpu: &mut Aarch64CpuState, word: u32) {
        assert_eq!(
            Aarch64FpExecutor::execute_word(cpu, &mut Aarch64SoftFloat, word),
            Aarch64ExecutionExit::Continue
        );
    }
}
