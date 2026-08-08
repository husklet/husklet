use crate::{Aarch64DecodeError, Aarch64Instruction, FpFormat};

pub(crate) struct Scvtf;

impl Scvtf {
    pub(crate) fn decode(word: u32) -> Option<Result<Aarch64Instruction, Aarch64DecodeError>> {
        let scalar = word & 0xdfbf_fc00 == 0x5e21_d800;
        let vector = word & 0x9fbf_fc00 == 0x0e21_d800;
        if !scalar && !vector {
            return None;
        }
        let double = word & 1 << 22 != 0;
        let wide = word & 1 << 30 != 0;
        if vector && double && !wide {
            return Some(Err(Aarch64DecodeError::Reserved));
        }
        Some(Ok(Aarch64Instruction::SimdIntegerFp {
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
            signed: word & 1 << 29 == 0,
            source: (word >> 5 & 31) as u8,
            destination: (word & 31) as u8,
        }))
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{Aarch64CpuState, Aarch64ExecutionExit, Aarch64FpExecutor, Aarch64Ir, Aarch64SoftFloat, FPSR_INEXACT};

    #[test]
    fn exhaustive_register_encodings_and_shapes() {
        for (base, format, lanes, signed) in [
            (0x0e21_d800, FpFormat::Single, 2, true),
            (0x4e21_d800, FpFormat::Single, 4, true),
            (0x4e61_d800, FpFormat::Double, 2, true),
            (0x2e21_d800, FpFormat::Single, 2, false),
            (0x6e21_d800, FpFormat::Single, 4, false),
            (0x6e61_d800, FpFormat::Double, 2, false),
            (0x5e21_d800, FpFormat::Single, 1, true),
            (0x5e61_d800, FpFormat::Double, 1, true),
            (0x7e21_d800, FpFormat::Single, 1, false),
            (0x7e61_d800, FpFormat::Double, 1, false),
        ] {
            for registers in 0..1024_u32 {
                let source = registers >> 5;
                let destination = registers & 31;
                assert_eq!(
                    Scvtf::decode(base | source << 5 | destination),
                    Some(Ok(Aarch64Instruction::SimdIntegerFp {
                        format,
                        lanes,
                        signed,
                        source: source as u8,
                        destination: destination as u8,
                    }))
                );
            }
        }
        assert_eq!(Scvtf::decode(0x0e61_d800), Some(Err(Aarch64DecodeError::Reserved)));
        assert_eq!(Scvtf::decode(0x4ea1_d800), None);
    }

    #[test]
    fn signed_unsigned_lanes_aliasing_flags_and_upper_clear() {
        let mut cpu = Aarch64CpuState {
            pc: 0x800,
            fpsr: 0x80,
            ..Default::default()
        };
        cpu.set_vector(
            1,
            u128::from(1_u32)
                | u128::from((-2_i32) as u32) << 32
                | u128::from(16_777_217_u32) << 64
                | u128::from(u32::MAX) << 96,
        );
        let instruction = Scvtf::decode(0x4e21_d821).unwrap().unwrap();
        assert_eq!(
            Aarch64FpExecutor::execute(
                &mut cpu,
                &mut Aarch64SoftFloat,
                &Aarch64Ir {
                    word: 0x4e21_d821,
                    wide: true,
                    instruction
                },
            ),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector_lane(1, 32, 0), 1_f32.to_bits() as u64);
        assert_eq!(cpu.vector_lane(1, 32, 1), (-2_f32).to_bits() as u64);
        assert_eq!(cpu.vector_lane(1, 32, 2), 16_777_216_f32.to_bits() as u64);
        assert_eq!(cpu.vector_lane(1, 32, 3), (-1_f32).to_bits() as u64);
        assert_eq!(cpu.fpsr, 0x80 | u64::from(FPSR_INEXACT));
        assert_eq!(cpu.pc, 0x804);

        cpu.pc = 0x900;
        cpu.fpsr = 0;
        cpu.set_vector(2, u128::MAX);
        let instruction = Scvtf::decode(0x7e61_d842).unwrap().unwrap();
        Aarch64FpExecutor::execute(
            &mut cpu,
            &mut Aarch64SoftFloat,
            &Aarch64Ir {
                word: 0x7e61_d842,
                wide: true,
                instruction,
            },
        );
        assert_eq!(cpu.vector(2), u128::from((u64::MAX as f64).to_bits()));
        assert_eq!(cpu.fpsr, u64::from(FPSR_INEXACT));
        assert_eq!(cpu.pc, 0x904);
    }
}
