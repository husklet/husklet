use crate::{Aarch64CpuState, Aarch64Instruction, FpArithmetic, FpArithmeticPort, FpFormat, FpRequest};

pub(crate) struct Convert;

impl Convert {
    pub(crate) fn decode(word: u32) -> Option<Aarch64Instruction> {
        let (format, high, widen) = match word & 0xffff_fc00 {
            0x0e21_7800 => (FpFormat::Half, false, true),
            0x4e21_7800 => (FpFormat::Half, true, true),
            0x0e61_7800 => (FpFormat::Single, false, true),
            0x4e61_7800 => (FpFormat::Single, true, true),
            0x0e21_6800 => (FpFormat::Half, false, false),
            0x4e21_6800 => (FpFormat::Half, true, false),
            0x0e61_6800 => (FpFormat::Single, false, false),
            0x4e61_6800 => (FpFormat::Single, true, false),
            _ => return None,
        };
        Some(Aarch64Instruction::SimdFpConvert {
            format,
            source: (word >> 5 & 31) as u8,
            destination: (word & 31) as u8,
            high,
            widen,
        })
    }

    pub(crate) fn execute<P: FpArithmeticPort>(
        cpu: &mut Aarch64CpuState,
        port: &mut P,
        format: FpFormat,
        source: u8,
        destination: u8,
        high: bool,
        widen: bool,
    ) {
        let wide = match format {
            FpFormat::Half => FpFormat::Single,
            FpFormat::Single => FpFormat::Double,
            FpFormat::Double => unreachable!("vector conversion format is narrow"),
        };
        let lanes = if format == FpFormat::Half { 4 } else { 2 };
        let (source_format, destination_format) = if widen { (format, wide) } else { (wide, format) };
        let first = if widen && high { lanes } else { 0 };
        let mut converted = 0_u128;
        for lane in 0..lanes {
            let result = port.evaluate(FpRequest {
                operation: FpArithmetic::ConvertFormat {
                    destination: destination_format,
                },
                format: source_format,
                left: cpu.vector_lane(source, source_format.bits(), first + lane),
                right: 0,
                addend: 0,
                fpcr: cpu.fpcr as u32,
            });
            cpu.fpsr |= u64::from(result.exceptions);
            converted |= u128::from(result.value) << (u32::from(lane) * u32::from(destination_format.bits()));
        }
        let value = if !widen && high {
            u128::from(cpu.vector(destination) as u64) | converted << 64
        } else {
            converted
        };
        cpu.set_vector(destination, value);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::{
        Aarch64ExecutionExit, Aarch64FpExecutor, Aarch64SoftFloat, FPSR_INEXACT, FPSR_INPUT_DENORMAL, FPSR_INVALID,
        Nzcv,
    };

    #[test]
    fn encodings() {
        for (base, format, high, widen) in [
            (0x0e21_7800, FpFormat::Half, false, true),
            (0x4e21_7800, FpFormat::Half, true, true),
            (0x0e61_7800, FpFormat::Single, false, true),
            (0x4e61_7800, FpFormat::Single, true, true),
            (0x0e21_6800, FpFormat::Half, false, false),
            (0x4e21_6800, FpFormat::Half, true, false),
            (0x0e61_6800, FpFormat::Single, false, false),
            (0x4e61_6800, FpFormat::Single, true, false),
        ] {
            for encoded in 0_u32..1024 {
                assert_eq!(
                    Convert::decode(base | encoded),
                    Some(Aarch64Instruction::SimdFpConvert {
                        format,
                        source: (encoded >> 5) as u8,
                        destination: (encoded & 31) as u8,
                        high,
                        widen,
                    })
                );
            }
        }
        for word in [0x2e61_7800, 0x2e61_6800, 0x5e61_7800, 0x0e61_7c00] {
            assert_eq!(Convert::decode(word), None);
        }
    }

    #[test]
    fn widening() {
        let source = pack32([
            1.0_f32.to_bits(),
            (-2.0_f32).to_bits(),
            3.0_f32.to_bits(),
            4.0_f32.to_bits(),
        ]);
        let mut cpu = Aarch64CpuState {
            pc: 0x4006_4c,
            nzcv: Nzcv::from_bits(0x9000_0000),
            ..Default::default()
        };
        cpu.set_vector(1, source);
        execute(&mut cpu, 0x0e61_7821);
        assert_eq!(cpu.vector_lane(1, 64, 0), 1.0_f64.to_bits());
        assert_eq!(cpu.vector_lane(1, 64, 1), (-2.0_f64).to_bits());
        assert_eq!(cpu.nzcv.bits(), 0x9000_0000);
        assert_eq!(cpu.pc, 0x4006_50);

        cpu.pc = 0;
        cpu.set_vector(1, source);
        execute(&mut cpu, 0x4e61_7821);
        assert_eq!(cpu.vector_lane(1, 64, 0), 3.0_f64.to_bits());
        assert_eq!(cpu.vector_lane(1, 64, 1), 4.0_f64.to_bits());
    }

    #[test]
    fn narrowing() {
        let mut cpu = Aarch64CpuState {
            fpsr: 1 << 27,
            ..Default::default()
        };
        cpu.set_vector(
            1,
            u128::from(1.0_f64.to_bits()) | u128::from(0x3ff0_0000_1000_0000_u64) << 64,
        );
        execute(&mut cpu, 0x0e61_6820);
        assert_eq!(cpu.vector(0), 0x3f80_0000_3f80_0000);
        assert_ne!(cpu.fpsr & u64::from(FPSR_INEXACT), 0);

        cpu.pc = 0;
        cpu.set_vector(0, 0xfeed_face_cafe_beef_0123_4567_89ab_cdef);
        execute(&mut cpu, 0x4e61_6820);
        assert_eq!(cpu.vector(0), 0x3f80_0000_3f80_0000_0123_4567_89ab_cdef);
    }

    #[test]
    fn control_state() {
        let mut cpu = Aarch64CpuState {
            fpcr: 1 << 24,
            fpsr: 1 << 27,
            ..Default::default()
        };
        cpu.set_vector(1, pack32([1, 0x7f80_0001, 0x7fc1_2345, 0x8000_0000]));
        execute(&mut cpu, 0x0e61_7820);
        assert_ne!(cpu.fpsr & u64::from(FPSR_INPUT_DENORMAL), 0);
        assert_ne!(cpu.fpsr & u64::from(FPSR_INVALID), 0);
        assert_eq!(cpu.vector_lane(0, 64, 0), 0);
        execute(&mut cpu, 0x4e61_7820);
        assert_eq!(cpu.vector_lane(0, 64, 1), 0x8000_0000_0000_0000);
    }

    fn execute(cpu: &mut Aarch64CpuState, word: u32) {
        assert_eq!(
            Aarch64FpExecutor::execute_word(cpu, &mut Aarch64SoftFloat, word),
            Aarch64ExecutionExit::Continue
        );
    }

    fn pack32(values: [u32; 4]) -> u128 {
        values
            .into_iter()
            .enumerate()
            .fold(0, |bits, (lane, value)| bits | u128::from(value) << (lane * 32))
    }
}
