use crate::{
    Aarch64CpuState, Aarch64DecodeError, Aarch64Decoder, Aarch64ExecutionExit, Aarch64FpExecutor, Aarch64Instruction,
    Aarch64Interpreter, Aarch64SoftFloat, FPSR_INEXACT, FpArithmetic, FpArithmeticPort, FpBinaryOperation, FpFormat,
    FpRequest, FpResult, Nzcv, PcCoordinatePort,
};

struct Coordinates;

impl PcCoordinatePort for Coordinates {
    fn architectural_pc(&self, execution_pc: u64) -> u64 {
        execution_pc
    }
}

#[derive(Default)]
struct ScriptedFp {
    requests: Vec<FpRequest>,
    result: Option<FpResult>,
}

impl FpArithmeticPort for ScriptedFp {
    fn evaluate(&mut self, request: FpRequest) -> FpResult {
        self.requests.push(request);
        self.result.take().expect("test must script one FP result")
    }
}

fn immediate_bits(format: FpFormat, immediate: u8) -> u64 {
    let sign = u64::from(immediate >> 7);
    let exponent = u64::from(immediate >> 4 & 7);
    let fraction = u64::from(immediate & 15);
    match format {
        FpFormat::Half => {
            sign << 15 | if exponent & 4 == 0 { 0x4000 } else { 0x3000 } | (exponent & 3) << 10 | fraction << 6
        }
        FpFormat::Single => {
            sign << 31
                | if exponent & 4 == 0 { 0x4000_0000 } else { 0x3e00_0000 }
                | (exponent & 3) << 23
                | fraction << 19
        }
        FpFormat::Double => {
            sign << 63
                | if exponent & 4 == 0 {
                    0x4000_0000_0000_0000
                } else {
                    0x3fc0_0000_0000_0000
                }
                | (exponent & 3) << 52
                | fraction << 48
        }
    }
}

#[test]
fn immediate_scalar() {
    for (format, encoded) in [(FpFormat::Half, 3_u32), (FpFormat::Single, 0), (FpFormat::Double, 1)] {
        verify_scalar_format(format, encoded);
    }
}

fn verify_scalar_format(format: FpFormat, encoded: u32) {
    for immediate in 0_u32..=u32::from(u8::MAX) {
        for destination in 0_u32..32 {
            verify_scalar(format, encoded, immediate, destination);
        }
    }
}

fn verify_scalar(format: FpFormat, encoded: u32, immediate: u32, destination: u32) {
    let word = 0x1e20_1000 | encoded << 22 | immediate << 13 | destination;
    let mut cpu = Aarch64CpuState {
        pc: 0x800,
        nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY),
        fpcr: 0x05c8_0000,
        fpsr: 0x0800_009f,
        ..Default::default()
    };
    cpu.set_vector(destination as u8, u128::MAX);
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut ScriptedFp::default(), word),
        Aarch64ExecutionExit::Continue,
        "format={format:?} imm={immediate:#04x} rd={destination}",
    );
    assert_eq!(
        cpu.vector(destination as u8),
        u128::from(immediate_bits(format, immediate as u8)),
    );
    assert_eq!(
        (cpu.pc, cpu.nzcv.bits(), cpu.fpcr, cpu.fpsr),
        (0x804, Nzcv::NEGATIVE | Nzcv::CARRY, 0x05c8_0000, 0x0800_009f),
    );
}

#[test]
fn immediate_vector() {
    for (format, operation, o2, wide) in [
        (FpFormat::Half, 0_u32, 1_u32, false),
        (FpFormat::Half, 0, 1, true),
        (FpFormat::Single, 0, 0, false),
        (FpFormat::Single, 0, 0, true),
        (FpFormat::Double, 1, 0, true),
    ] {
        verify_vector_format(format, operation, o2, wide);
    }
}

fn verify_vector_format(format: FpFormat, operation: u32, o2: u32, wide: bool) {
    for immediate in 0_u32..=u32::from(u8::MAX) {
        for destination in 0_u32..32 {
            verify_vector(format, operation, o2, wide, immediate, destination);
        }
    }
}

fn verify_vector(format: FpFormat, operation: u32, o2: u32, wide: bool, immediate: u32, destination: u32) {
    let word = 0x0f00_f400
        | u32::from(wide) << 30
        | operation << 29
        | o2 << 11
        | (immediate >> 5) << 16
        | (immediate & 31) << 5
        | destination;
    let mut cpu = Aarch64CpuState {
        pc: 0xa00,
        nzcv: Nzcv::from_bits(Nzcv::ZERO | Nzcv::OVERFLOW),
        fpcr: 0x0148_0000,
        fpsr: 0x0800_001f,
        ..Default::default()
    };
    cpu.set_vector(destination as u8, u128::MAX);
    assert_eq!(
        Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
        Aarch64ExecutionExit::Continue,
        "format={format:?} imm={immediate:#04x} rd={destination} q={wide}",
    );
    let lane = u128::from(immediate_bits(format, immediate as u8));
    let lanes = if wide { 128 } else { 64 } / u32::from(format.bits());
    let expected = (0..lanes).fold(0_u128, |value, index| {
        value | lane << (index * u32::from(format.bits()))
    });
    assert_eq!(cpu.vector(destination as u8), expected);
    assert_eq!(
        (cpu.pc, cpu.nzcv.bits(), cpu.fpcr, cpu.fpsr),
        (0xa04, Nzcv::ZERO | Nzcv::OVERFLOW, 0x0148_0000, 0x0800_001f),
    );
}

#[test]
fn immediate_rollback() {
    for word in [
        0x1ea0_1000, // Scalar type 10 is reserved.
        0x2f00_f400, // AdvSIMD double requires Q=1.
        0x6f00_fc00, // AdvSIMD double requires o2=0.
    ] {
        let mut cpu = Aarch64CpuState {
            registers: core::array::from_fn(|index| index as u64 * 0x0101_0101),
            vectors: core::array::from_fn(|index| u128::MAX - index as u128),
            sp: 0x1234,
            pc: 0xc00,
            nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::ZERO),
            tls: 0x5678,
            fpcr: 0x07c8_0000,
            fpsr: 0x0800_009f,
            exclusive: None,
        };
        let before = cpu.clone();
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
            Aarch64ExecutionExit::UndefinedInstruction {
                instruction: 0xc00,
                word
            },
        );
        assert_eq!(cpu, before, "word={word:#010x}");
    }
}

#[test]
fn scalar_fp_decoder() {
    let words = [
        0x1eee_1000,
        0x1e2e_1000,
        0x1e6e_1000, // FMOV H/S/D immediate
        0x1e20_4020,
        0x1e60_c020,
        0x1ee1_4020, // FMOV/FABS/FNEG
        0x1e21_c020,
        0x1e22_2820,
        0x1e62_3820, // FSQRT/FADD/FSUB
        0x1e22_2020,
        0x1e22_2030,
        0x1e22_0420, // FCMP/FCMPE/FCCMP
        0x1f02_0c20,
        0x1f02_8c20,
        0x1f22_0c20,
        0x1f22_8c20, // fused
        0x1e22_4820,
        0x1e22_5820,
        0x1e22_6820,
        0x1e22_7820, // min/max
        0x1e24_4020,
        0x1e24_c020,
        0x1e27_4020,
        0x1e27_c020, // FRINT
        0x1e22_c020,
        0x1e62_4020,
        0x1ee2_4020,
        0x1ee2_c020, // FCVT
        0x1e26_0020,
        0x1e27_0020,
        0x9e66_0020,
        0x9e67_0020, // FMOV GPR
        0x1e22_0020,
        0x1e23_0020,
        0x9e38_0020,
        0x9e39_0020, // conversions
        0x7ea2_d420,
        0x7ee5_d483, // scalar SIMD FABD S/D
    ];
    for word in words {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
    }
    assert!(matches!(
        Aarch64Decoder::decode(0x1e22_2820).unwrap().instruction,
        Aarch64Instruction::FpBinary {
            operation: FpBinaryOperation::Add,
            format: FpFormat::Single,
            left: 1,
            right: 2,
            destination: 0,
        }
    ));
}

#[test]
fn simd_sign_operations_preserve_payloads_and_clear_inactive_half() {
    let source = 0x7ff8_0123_4567_89ab_u128 << 64 | 0x8000_0000_0000_0000;
    for (word, expected) in [
        (0x6ee0_fbff, source ^ (1_u128 << 127 | 1_u128 << 63)),
        (0x4ea0_fbff, source & !(1_u128 << 127 | 1_u128 << 63)),
    ] {
        let mut cpu = Aarch64CpuState {
            pc: 0x1000,
            fpsr: 0x9f,
            fpcr: 0x05c8_0000,
            ..Default::default()
        };
        cpu.set_vector(31, source);
        assert_eq!(
            Aarch64FpExecutor::execute_word(&mut cpu, &mut Aarch64SoftFloat, word),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector(31), expected, "word={word:#010x}");
        assert_eq!((cpu.pc, cpu.fpsr, cpu.fpcr), (0x1004, 0x9f, 0x05c8_0000));
    }
    let mut cpu = Aarch64CpuState {
        pc: 0x2000,
        ..Default::default()
    };
    cpu.set_vector(31, u128::MAX);
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut Aarch64SoftFloat, 0x2ea0_fbff),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(31) >> 64, 0);
}

#[test]
fn simd_round_integral_decodes_modes_and_accumulates_exceptions() {
    for word in [0x4e21_8bff, 0x6e61_9bff] {
        assert!(Aarch64Decoder::decode(word).is_ok(), "word={word:#010x}");
    }
    let mut cpu = Aarch64CpuState {
        pc: 0x3000,
        fpcr: 3_u64 << 22,
        fpsr: 0x80,
        ..Default::default()
    };
    cpu.set_vector(
        31,
        u128::from(2.5_f64.to_bits()) | (u128::from((-1.25_f64).to_bits()) << 64),
    );
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut Aarch64SoftFloat, 0x6e61_9bff),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector_lane(31, 64, 0), 2.0_f64.to_bits());
    assert_eq!(cpu.vector_lane(31, 64, 1), (-1.0_f64).to_bits());
    assert_ne!(cpu.fpsr & u64::from(FPSR_INEXACT), 0);
    assert_eq!(cpu.pc, 0x3004);
}

#[test]
fn scalar_absolute_difference() {
    for &(word, format, left, right, expected) in &[
        (0x7ea2_d420, FpFormat::Single, 1_u8, 2_u8, 3.5_f32.to_bits() as u64),
        (0x7ee5_d483, FpFormat::Double, 4, 5, 3.5_f64.to_bits()),
    ] {
        let mut cpu = Aarch64CpuState {
            pc: 0x80,
            fpsr: 0x20,
            ..Default::default()
        };
        cpu.set_vector(
            left,
            u128::from(match format {
                FpFormat::Single => (-1.25_f32).to_bits() as u64,
                FpFormat::Double => (-1.25_f64).to_bits(),
                FpFormat::Half => unreachable!(),
            }),
        );
        cpu.set_vector(
            right,
            u128::from(match format {
                FpFormat::Single => 2.25_f32.to_bits() as u64,
                FpFormat::Double => 2.25_f64.to_bits(),
                FpFormat::Half => unreachable!(),
            }),
        );
        assert_eq!(
            Aarch64FpExecutor::execute_word(&mut cpu, &mut Aarch64SoftFloat, word),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector((word & 31) as u8), u128::from(expected));
        assert_eq!((cpu.pc, cpu.fpsr), (0x84, 0x20));
    }
}

#[test]
fn conditional_select_decoder() {
    for index in 0_u32..2 * 16 * 32 * 32 * 32 {
        let format = index / (16 * 32 * 32 * 32);
        let condition = index / (32 * 32 * 32) % 16;
        let alternate = index / (32 * 32) % 32;
        let source = index / 32 % 32;
        let destination = index % 32;
        let word = 0x1e20_0c00 | format << 22 | alternate << 16 | condition << 12 | source << 5 | destination;
        assert_eq!(
            Aarch64Decoder::decode(word).unwrap().instruction,
            Aarch64Instruction::FpSelect {
                format: if format == 0 {
                    FpFormat::Single
                } else {
                    FpFormat::Double
                },
                source: source as u8,
                alternate: alternate as u8,
                destination: destination as u8,
                condition: crate::Aarch64BranchCondition(condition as u8),
            }
        );
    }
}

#[test]
fn conditional_select_bits() {
    let patterns = [
        (0x7fc0_1234_u64, 0xff80_0001_u64),
        (0x8000_0000, 0),
        (0x7ff8_1234_5678_9abc, 0xfff0_0000_0000_0001),
        (0x8000_0000_0000_0000, 0),
    ];
    for case in 0_u32..16 * 16 * patterns.len() as u32 {
        let condition = case / (16 * patterns.len() as u32);
        let flags = case / patterns.len() as u32 % 16;
        let index = case as usize % patterns.len();
        let (source, alternate) = patterns[index];
        let double = index >= 2;
        let word = 0x1e20_0c20 | u32::from(double) << 22 | 2 << 16 | condition << 12;
        let mut cpu = Aarch64CpuState {
            pc: 0x300,
            nzcv: Nzcv::from_bits(flags << 28),
            fpsr: 0x0800_009f,
            ..Default::default()
        };
        cpu.set_vector(1, u128::MAX << 64 | u128::from(source));
        cpu.set_vector(2, u128::MAX << 64 | u128::from(alternate));
        assert_eq!(
            Aarch64FpExecutor::execute_word(&mut cpu, &mut Aarch64SoftFloat, word),
            Aarch64ExecutionExit::Continue
        );
        let expected = if crate::Aarch64BranchCondition(condition as u8).holds(Nzcv::from_bits(flags << 28)) {
            source
        } else {
            alternate
        };
        let mask = if double { u64::MAX } else { u64::from(u32::MAX) };
        assert_eq!(cpu.vector(0), u128::from(expected & mask));
        assert_eq!((cpu.pc, cpu.nzcv.bits(), cpu.fpsr), (0x304, flags << 28, 0x0800_009f));
    }
}

#[test]
fn general_moves_preserve() {
    let mut cpu = Aarch64CpuState {
        pc: 0x80,
        fpcr: 0x1122,
        fpsr: 0x3344,
        ..Default::default()
    };
    let mut fp = ScriptedFp::default();
    cpu.set_register(3, 0xffff_ffff_7fc0_1234);
    cpu.set_vector(2, u128::MAX);
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x1e27_0062),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(2), 0x7fc0_1234);

    cpu.set_register(0, u64::MAX);
    cpu.set_vector(1, 0xffff_ffff_ffff_ffff_0123_4567_dead_beef);
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x1e26_0020),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(0), 0xdead_beef);

    cpu.set_register(7, 0x7ff8_abcd_0123_4567);
    cpu.set_vector(6, u128::MAX);
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x9e67_00e6),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(6), 0x7ff8_abcd_0123_4567);

    cpu.set_vector(5, u128::from(0x7ff8_1234_5678_9abc_u64));
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x9e66_00a4),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(4), 0x7ff8_1234_5678_9abc);

    cpu.set_register(11, 0xfeed_face_dead_beef);
    cpu.set_vector(10, 0x0123_4567_89ab_cdef_1122_3344_5566_7788);
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x9eaf_016a),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(10), 0xfeed_face_dead_beef_1122_3344_5566_7788);
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x9eae_014c),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(12), 0xfeed_face_dead_beef);
    assert_eq!((cpu.pc, cpu.fpcr, cpu.fpsr), (0x98, 0x1122, 0x3344));
}

#[test]
fn general_move_zr() {
    let mut cpu = Aarch64CpuState {
        pc: 0x90,
        ..Default::default()
    };
    let mut fp = ScriptedFp::default();
    cpu.set_vector(31, u128::MAX);
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x1e27_03ff),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(31), 0);
    cpu.set_vector(31, 0xffff_ffff_89ab_cdef);
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x1e26_03ff),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(31), 0);
}

#[test]
fn general_move_reserved() {
    for word in [
        0x9e26_0020,
        0x1e66_0020,
        0x1e2e_0020,
        0x1ea6_0020,
        0x1eae_0020,
        0x9ead_0020,
    ] {
        assert_eq!(Aarch64Decoder::decode(word), Err(crate::Aarch64DecodeError::Reserved));
    }
}

#[test]
fn moves_compare_and() {
    let mut cpu = Aarch64CpuState {
        pc: 0x100,
        ..Default::default()
    };
    let mut fp = ScriptedFp::default();
    cpu.set_vector(1, 0xffff_ffff_7fa0_1234);
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x1e20_4020),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), 0x7fa0_1234);

    cpu.set_vector(1, u128::from(0x7fc0_0001_u32));
    cpu.set_vector(2, u128::from(1.0_f32.to_bits()));
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x1e22_2020),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.nzcv.bits(), Nzcv::CARRY | Nzcv::OVERFLOW);
    assert_eq!(cpu.fpsr & 1, 0);

    cpu.nzcv = Nzcv::default();
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x1e22_0425),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.nzcv.bits(), 5 << 28);
}

#[test]
fn arithmetic_port_receives() {
    let mut cpu = Aarch64CpuState {
        pc: 0x200,
        fpcr: 3 << 22,
        fpsr: 1,
        ..Default::default()
    };
    cpu.set_vector(1, u128::from(0x3f80_0000_u32));
    cpu.set_vector(2, u128::from(0x4000_0000_u32));
    let mut fp = ScriptedFp {
        requests: Vec::new(),
        result: Some(FpResult {
            value: 0x4040_0000,
            exceptions: FPSR_INEXACT,
        }),
    };
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x1e22_2820),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), 0x4040_0000);
    assert_eq!(cpu.fpsr, 1 | u64::from(FPSR_INEXACT));
    assert_eq!(
        fp.requests,
        [FpRequest {
            operation: FpArithmetic::Binary(FpBinaryOperation::Add),
            format: FpFormat::Single,
            left: 0x3f80_0000,
            right: 0x4000_0000,
            addend: 0,
            fpcr: 3 << 22,
        }]
    );
}

#[test]
fn software_float_adapter() {
    let mut cpu = Aarch64CpuState {
        pc: 0x280,
        ..Default::default()
    };
    let mut arithmetic = Aarch64SoftFloat;
    cpu.set_vector(1, u128::from(0x3f80_0000_u32));
    cpu.set_vector(2, u128::from(0x4000_0000_u32));
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut arithmetic, 0x1e22_2820),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), 0x4040_0000);
    assert_eq!(cpu.fpsr, 0);

    cpu.set_register(1, u64::from(u32::MAX));
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut arithmetic, 0x1e22_0020),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), 0xbf80_0000);
}

#[test]
fn software_float_fpsr() {
    let mut arithmetic = Aarch64SoftFloat;
    let mut cpu = Aarch64CpuState {
        pc: 0x2c0,
        fpcr: (1 << 25) | (1 << 24),
        fpsr: u64::from(FPSR_INEXACT),
        ..Default::default()
    };
    cpu.set_vector(1, u128::from(0x7f80_0001_u32));
    cpu.set_vector(2, u128::from(0x3f80_0000_u32));
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut arithmetic, 0x1e22_2820),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), 0x7fc0_0000);
    assert_eq!(cpu.fpsr & 1, 1);
    assert_ne!(cpu.fpsr & u64::from(FPSR_INEXACT), 0);

    cpu.fpcr = 1 << 24;
    cpu.set_vector(1, 1);
    cpu.set_vector(2, u128::from(0x3f80_0000_u32));
    Aarch64FpExecutor::execute_word(&mut cpu, &mut arithmetic, 0x1e22_2820);
    assert_eq!(cpu.vector(0), 0x3f80_0000);
    assert_ne!(cpu.fpsr & 1 << 7, 0);

    cpu.fpcr = 1 << 19;
    cpu.fpsr = 0;
    cpu.set_vector(1, 1);
    cpu.set_vector(2, 0x3c00);
    Aarch64FpExecutor::execute_word(&mut cpu, &mut arithmetic, 0x1ee2_2820);
    assert_eq!(cpu.vector(0), 0x3c00);
    assert_eq!(cpu.fpsr & 1 << 7, 0);
}

#[test]
fn fused_selection_rounding() {
    let mut arithmetic = Aarch64SoftFloat;
    let mut cpu = Aarch64CpuState {
        pc: 0x340,
        ..Default::default()
    };
    cpu.set_vector(1, 0x3f80_0001);
    cpu.set_vector(2, 0x3f7f_fffe);
    cpu.set_vector(3, 0xbf80_0000);
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut arithmetic, 0x1f02_0c20),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), 0xa880_0000);

    cpu.set_vector(1, 0x4000_0000);
    cpu.set_vector(2, 0x4040_0000);
    cpu.set_vector(3, 0x40a0_0000);
    for (word, expected) in [
        (0x1f02_0c20, 0x4130_0000),
        (0x1f02_8c20, 0xbf80_0000),
        (0x1f22_0c20, 0xc130_0000),
        (0x1f22_8c20, 0x3f80_0000),
    ] {
        Aarch64FpExecutor::execute_word(&mut cpu, &mut arithmetic, word);
        assert_eq!(cpu.vector(0), expected);
    }

    cpu.set_vector(1, 0x8000_0000);
    cpu.set_vector(2, 0);
    Aarch64FpExecutor::execute_word(&mut cpu, &mut arithmetic, 0x1e22_5820);
    assert_eq!(cpu.vector(0), 0x8000_0000);
    Aarch64FpExecutor::execute_word(&mut cpu, &mut arithmetic, 0x1e22_4820);
    assert_eq!(cpu.vector(0), 0);

    cpu.set_vector(1, 0x3fc0_0000);
    Aarch64FpExecutor::execute_word(&mut cpu, &mut arithmetic, 0x1e24_4020);
    assert_eq!(cpu.vector(0), 0x4000_0000);
    Aarch64FpExecutor::execute_word(&mut cpu, &mut arithmetic, 0x1e22_c020);
    assert_eq!(cpu.vector(0), 0x3ff8_0000_0000_0000);
}

#[test]
fn simd_integer_lane() {
    let mut cpu = Aarch64CpuState {
        pc: 0x300,
        ..Default::default()
    };
    cpu.set_vector(1, u128::MAX);
    cpu.set_vector(2, 0x0101_0101_0101_0101_0101_0101_0101_0101);
    assert_eq!(
        Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e22_8420),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), 0);

    cpu.set_vector(1, 0xaaaa);
    cpu.set_vector(2, 0x0f0f);
    assert_eq!(
        Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e22_1c20),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), 0x0a0a);
}

#[test]
fn simd_immediate_duplicate() {
    let mut cpu = Aarch64CpuState {
        pc: 0x400,
        ..Default::default()
    };
    assert_eq!(
        Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4f00_e640),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), u128::from_le_bytes([0x12; 16]));

    cpu.set_register(1, 0x1122_3344);
    assert_eq!(
        Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e04_0c20),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), 0x1122_3344_1122_3344_1122_3344_1122_3344);

    cpu.set_vector(2, 0x8000_0000);
    assert_eq!(
        Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e04_2c43),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(3), 0xffff_ffff_8000_0000);
}

#[test]
fn simd_extract_table() {
    for word in [
        0x6e02_1820,
        0x4e02_0020,
        0x4e02_1020,
        0x4e02_1820,
        0x4e02_5820,
        0x4e02_2820,
        0x4e02_6820,
        0x4e02_3820,
        0x4e02_7820,
    ] {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:08x}");
    }

    let mut cpu = Aarch64CpuState::default();
    cpu.set_vector(1, u128::from_le_bytes(core::array::from_fn(|index| index as u8)));
    cpu.set_vector(2, u128::from_le_bytes(core::array::from_fn(|index| (index + 16) as u8)));
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x6e02_1820);
    assert_eq!(
        cpu.vector(0).to_le_bytes(),
        core::array::from_fn(|index| (index + 3) as u8)
    );

    let indexes = [15, 0, 16, 31, 32, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    cpu.set_vector(2, u128::from_le_bytes(indexes));
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e02_0020);
    assert_eq!(
        cpu.vector(0).to_le_bytes(),
        [15, 0, 0, 0, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]
    );

    cpu.set_vector(1, 0x0007_0006_0005_0004_0003_0002_0001_0000);
    cpu.set_vector(2, 0x000f_000e_000d_000c_000b_000a_0009_0008);
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e42_3820);
    assert_eq!(cpu.vector(0), 0x000b_0003_000a_0002_0009_0001_0008_0000);
}

#[test]
fn simd_unary_integer() {
    let mut cpu = Aarch64CpuState::default();
    cpu.set_vector(
        1,
        u128::from_le_bytes([
            0x01, 0x02, 0x04, 0x08, 0x10, 0x20, 0x40, 0x80, 0xff, 0, 3, 5, 7, 9, 11, 13,
        ]),
    );
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e20_5820);
    assert_eq!(
        cpu.vector(0).to_le_bytes(),
        [1, 1, 1, 1, 1, 1, 1, 1, 8, 0, 2, 2, 3, 2, 3, 3]
    );
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x6e60_5820);
    assert_eq!(
        cpu.vector(0).to_le_bytes(),
        [
            0x80, 0x40, 0x20, 0x10, 8, 4, 2, 1, 0xff, 0, 0xc0, 0xa0, 0xe0, 0x90, 0xd0, 0xb0
        ]
    );
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e20_0820);
    assert_eq!(
        cpu.vector(0).to_le_bytes(),
        [0x80, 0x40, 0x20, 0x10, 8, 4, 2, 1, 13, 11, 9, 7, 5, 3, 0, 0xff]
    );

    cpu.set_vector(1, 0x8000_0000_ffff_ffff_0000_0001_0000_0000);
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x6ea0_4820);
    assert_eq!(cpu.vector(0), 0x0000_0000_0000_0000_0000_001f_0000_0020);
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4ea0_b820);
    assert_eq!(cpu.vector(0), 0x8000_0000_0000_0001_0000_0001_0000_0000);
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x6ea0_b820);
    assert_eq!(cpu.vector(0), 0x8000_0000_0000_0001_ffff_ffff_0000_0000);
}

#[test]
fn simd_saturating_add() {
    let mut cpu = Aarch64CpuState::default();
    cpu.set_vector(
        1,
        u128::from_le_bytes([
            0x7f, 0x80, 10, 0, 0x7f, 0x80, 10, 0, 0x7f, 0x80, 10, 0, 0x7f, 0x80, 10, 0,
        ]),
    );
    cpu.set_vector(
        2,
        u128::from_le_bytes([1, 1, 20, 1, 1, 1, 20, 1, 1, 1, 20, 1, 1, 1, 20, 1]),
    );
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e22_0c20);
    assert_eq!(
        cpu.vector(0).to_le_bytes(),
        [
            0x7f, 0x81, 30, 1, 0x7f, 0x81, 30, 1, 0x7f, 0x81, 30, 1, 0x7f, 0x81, 30, 1
        ]
    );
    assert_ne!(cpu.fpsr & 1 << 27, 0);

    cpu.fpsr = 0;
    cpu.set_vector(
        1,
        u128::from_le_bytes([0, 255, 10, 200, 0, 255, 10, 200, 0, 255, 10, 200, 0, 255, 10, 200]),
    );
    cpu.set_vector(
        2,
        u128::from_le_bytes([1, 1, 20, 100, 1, 1, 20, 100, 1, 1, 20, 100, 1, 1, 20, 100]),
    );
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x6e22_2c20);
    assert_eq!(
        cpu.vector(0).to_le_bytes(),
        [0, 254, 0, 100, 0, 254, 0, 100, 0, 254, 0, 100, 0, 254, 0, 100]
    );
    assert_ne!(cpu.fpsr & 1 << 27, 0);
}

#[test]
fn simd_immediate_shifts() {
    let mut cpu = Aarch64CpuState::default();
    cpu.set_vector(
        1,
        u128::from_le_bytes([1, 2, 0x20, 0x80, 0xff, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13]),
    );
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4f0b_5420);
    assert_eq!(
        cpu.vector(0).to_le_bytes(),
        [8, 16, 0, 0, 0xf8, 24, 32, 40, 48, 56, 64, 72, 80, 88, 96, 104]
    );
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x6f0e_0420);
    assert_eq!(
        cpu.vector(0).to_le_bytes(),
        [0, 0, 8, 32, 63, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3]
    );
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4f0e_2420);
    assert_eq!(
        cpu.vector(0).to_le_bytes(),
        [0, 1, 8, 0xe0, 0, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3]
    );
}

#[test]
fn variable_shift() {
    fn pack(bits: u8, lanes: &[u64]) -> u128 {
        let mask = if bits == 64 {
            u128::from(u64::MAX)
        } else {
            (1_u128 << bits) - 1
        };
        lanes.iter().enumerate().fold(0, |value, (lane, element)| {
            value | (u128::from(*element) & mask) << (lane as u32 * u32::from(bits))
        })
    }
    fn expected(value: u64, count: i8, bits: u8) -> u64 {
        let mask = if bits == 64 { u64::MAX } else { (1_u64 << bits) - 1 };
        if count >= 0 {
            let amount = count as u32;
            return if amount >= u32::from(bits) {
                0
            } else {
                value << amount & mask
            };
        }
        let shift = 64 - bits;
        let signed = ((value << shift) as i64) >> shift;
        let amount = count.unsigned_abs() as u32;
        if amount >= u32::from(bits) {
            return if signed < 0 { mask } else { 0 };
        }
        (signed >> amount) as u64 & mask
    }
    fn sample(bits: u8, lane: u8) -> u64 {
        let value = 3 + u64::from(lane);
        if lane & 1 == 0 {
            value | 1_u64 << (bits - 1)
        } else {
            value + 2
        }
    }
    fn results(values: &[u64], counts: &[i8], bits: u8) -> Vec<u64> {
        values
            .iter()
            .zip(counts)
            .map(|(value, count)| expected(*value, *count, bits))
            .collect()
    }
    fn verify(bits: u8, wide: bool, chunk: &[i8]) {
        let lane_count = if wide { 128 } else { 64 } / bits;
        let values: Vec<_> = (0..lane_count).map(|lane| sample(bits, lane)).collect();
        let lane_counts: Vec<_> = (0..lane_count as usize)
            .map(|lane| *chunk.get(lane).unwrap_or(&1))
            .collect();
        let count_values: Vec<_> = lane_counts.iter().map(|count| *count as u8 as u64).collect();
        let result = results(&values, &lane_counts, bits);
        let size = bits.trailing_zeros() - 3;
        let word = 0x0e20_4400 | u32::from(wide) << 30 | size << 22 | 2 << 16 | 1 << 5;
        let flags = Nzcv::NEGATIVE | Nzcv::CARRY;
        let mut cpu = Aarch64CpuState {
            pc: 0x900,
            nzcv: Nzcv::from_bits(flags),
            ..Default::default()
        };
        cpu.set_vector(1, pack(bits, &values));
        cpu.set_vector(2, pack(bits, &count_values));
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(
            cpu.vector(0),
            pack(bits, &result),
            "bits={bits} wide={wide} counts={chunk:?}"
        );
        assert_eq!(cpu.nzcv.bits(), flags);
        assert_eq!(cpu.pc, 0x904);
    }
    let boundaries = |bits: u8| {
        [
            0,
            1,
            bits as i8 - 1,
            bits as i8,
            bits as i8 + 1,
            -1,
            -(bits as i8 - 1),
            -(bits as i8),
            -(bits as i8) - 1,
            i8::MIN,
        ]
    };
    for (bits, wide) in [
        (8, false),
        (8, true),
        (16, false),
        (16, true),
        (32, false),
        (32, true),
        (64, true),
    ] {
        let counts = boundaries(bits);
        let lanes = if wide { 128 } else { 64 } / bits;
        for chunk in counts.chunks(lanes as usize) {
            verify(bits, wide, chunk);
        }
    }
}

#[test]
fn variable_aliases() {
    let mut cpu = Aarch64CpuState::default();
    cpu.set_vector(1, u128::from_le_bytes([1; 16]));
    cpu.set_vector(
        2,
        u128::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]),
    );
    assert_eq!(
        Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e22_4421),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vector(1).to_le_bytes(),
        [2, 4, 8, 16, 32, 64, 128, 0, 0, 0, 0, 0, 0, 0, 0, 0]
    );
    cpu.set_vector(1, u128::from_le_bytes([2; 16]));
    cpu.set_vector(2, u128::from_le_bytes([1; 16]));
    assert_eq!(
        Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e22_4422),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(2), u128::from_le_bytes([4; 16]));
    assert_eq!(
        Aarch64Decoder::decode(0x0ee2_4420),
        Err(crate::Aarch64DecodeError::Reserved)
    );
    assert!(matches!(
        Aarch64Decoder::decode(0x6e22_4420),
        Ok(crate::Aarch64Ir {
            instruction: Aarch64Instruction::SimdVariable {
                signed: false,
                saturating: false,
                rounding: false,
                lane_bits: 8,
                source: 1,
                counts: 2,
                destination: 0,
                wide: true,
            },
            ..
        })
    ));
}

#[test]
fn simd_bic_immediate() {
    for word in [
        0x6f07_9600,
        0x6f07_b601,
        0x6f00_1642,
        0x6f01_3683,
        0x6f02_56c4,
        0x6f03_7705,
        0x2f04_9746,
        0x2f05_5787,
    ] {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
    }
    let mut cpu = Aarch64CpuState {
        pc: 0x700,
        nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY),
        ..Default::default()
    };
    cpu.set_vector(4, 0xffff_eeee_dddd_cccc_bbbb_aaaa_9999_8888);
    assert_eq!(
        Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x6f07_9604),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(4), 0xff0f_ee0e_dd0d_cc0c_bb0b_aa0a_9909_8808);
    assert_eq!(cpu.nzcv.bits(), Nzcv::NEGATIVE | Nzcv::CARRY);
    assert_eq!(cpu.pc, 0x704);

    cpu.set_vector(6, u128::MAX);
    assert_eq!(
        Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x2f04_9746),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(6), 0xff65_ff65_ff65_ff65);
}

#[test]
fn simd_bic_reserved() {
    assert_eq!(
        Aarch64Decoder::decode(0x6f07_9e04),
        Err(crate::Aarch64DecodeError::Reserved)
    );
    // Adjacent even cmode and selector-six/seven encodings are MVNI/FMOV,
    // not BIC, and remain independently decoded.
    for word in [0x6f07_8604, 0x6f07_d604, 0x6f07_f604] {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
    }
}

#[test]
fn simd_shift_narrow() {
    let vectors = |narrow_bits: u8, amount: u8| {
        let wide_bits = narrow_bits * 2;
        let lanes = 64 / narrow_bits;
        let mut source = 0_u128;
        let mut expected = 0_u128;
        for lane in 0..lanes {
            let wide_mask = u64::MAX >> (64 - wide_bits);
            let value = wide_mask.wrapping_sub(u64::from(lane) * 0x0101_0101);
            source |= u128::from(value & wide_mask) << (u32::from(lane) * u32::from(wide_bits));
            let narrow_mask = (1_u64 << narrow_bits) - 1;
            expected |= u128::from((value >> amount) & narrow_mask) << (u32::from(lane) * u32::from(narrow_bits));
        }
        (source, expected)
    };
    for index in 0_u8..56 {
        let (narrow_bits, amount) = match index {
            0..=7 => (8, index + 1),
            8..=23 => (16, index - 7),
            _ => (32, index - 23),
        };
        let (source, expected) = vectors(narrow_bits, amount);
        let combined = 2 * narrow_bits - amount;
        let word = 0x0f00_8400 | u32::from(combined >> 3) << 19 | u32::from(combined & 7) << 16 | 1 << 5 | 1;
        let mut cpu = Aarch64CpuState::default();
        cpu.set_vector(1, source);
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
            Aarch64ExecutionExit::Continue
        );
        assert_eq!(cpu.vector(1), expected, "bits={narrow_bits} shift={amount}");
    }
}

#[test]
fn simd_shift_long() {
    for index in 0_u16..224 {
        let (narrow_bits, local) = match index {
            0..32 => (8_u8, index),
            32..96 => (16, index - 32),
            _ => (32, index - 96),
        };
        let amount = (local / 4) as u8;
        let signed = local >> 1 & 1 != 0;
        let high = local & 1 != 0;
        let lanes = 128 / narrow_bits;
        let narrow_mask = (1_u64 << narrow_bits) - 1;
        let wide_bits = narrow_bits * 2;
        let wide_mask = if wide_bits == 64 {
            u64::MAX
        } else {
            (1_u64 << wide_bits) - 1
        };
        let mut source = 0_u128;
        for lane in 0..lanes {
            let high_bit = u64::from(1 ^ (lane & 1)) << (narrow_bits - 1);
            let raw = (high_bit | u64::from(lane + 1)) & narrow_mask;
            source |= u128::from(raw) << (u32::from(lane) * u32::from(narrow_bits));
        }
        let combined = narrow_bits + amount;
        let word = 0x0f00_a400
            | u32::from(high) << 30
            | u32::from(!signed) << 29
            | u32::from(combined >> 3) << 19
            | u32::from(combined & 7) << 16
            | 31 << 5
            | 31;
        let mut expected = 0_u128;
        for lane in 0..(64 / narrow_bits) {
            let source_lane = lane + u8::from(high) * (64 / narrow_bits);
            let raw = (source >> (u32::from(source_lane) * u32::from(narrow_bits))) as u64 & narrow_mask;
            let sign = u64::from(signed) * (raw >> (narrow_bits - 1));
            let extended = raw | 0_u64.wrapping_sub(sign) & !narrow_mask;
            expected |= u128::from((extended << amount) & wide_mask) << (u32::from(lane) * u32::from(wide_bits));
        }
        let mut cpu = Aarch64CpuState::default();
        cpu.set_vector(31, source);
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
            Aarch64ExecutionExit::Continue,
            "bits={narrow_bits} shift={amount} signed={signed} high={high}",
        );
        assert_eq!(
            cpu.vector(31),
            expected,
            "bits={narrow_bits} shift={amount} signed={signed} high={high}",
        );
    }
    for (word, high) in [(0x2e61_39ff, false), (0x6e61_39ff, true)] {
        let mut cpu = Aarch64CpuState::default();
        cpu.set_vector(15, 0x0008_0007_0006_0005_0004_0003_0002_0001);
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
            Aarch64ExecutionExit::Continue
        );
        for lane in 0..4 {
            assert_eq!(
                cpu.vector_lane(31, 32, lane),
                u64::from(lane + 1 + u8::from(high) * 4) << 16
            );
        }
    }
}

#[test]
fn shift_long_reserved() {
    for word in [0x0f40_a400, 0x2f40_a400, 0x4f40_a400, 0x6f40_a400] {
        assert_eq!(Aarch64Decoder::decode(word), Err(crate::Aarch64DecodeError::Reserved));
        let mut cpu = Aarch64CpuState {
            pc: 0x900,
            ..Default::default()
        };
        cpu.set_vector(0, u128::MAX);
        assert_eq!(
            Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
            Aarch64ExecutionExit::UndefinedInstruction {
                instruction: 0x900,
                word
            },
        );
        assert_eq!(cpu.vector(0), u128::MAX);
        assert_eq!(cpu.pc, 0x900);
    }
    for word in [0x0e21_3800, 0x2ee1_3800] {
        assert_eq!(Aarch64Decoder::decode(word), Err(crate::Aarch64DecodeError::Reserved));
    }
}

#[test]
fn simd_shrn2_state() {
    let mut cpu = Aarch64CpuState {
        pc: 0x800,
        nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY),
        ..Default::default()
    };
    cpu.set_vector(0, 0xffff_eeee_dddd_cccc_bbaa_9988_7766_5544);
    cpu.set_vector(1, 0x1234_5678_9abc_def0_0fed_cba9_8765_4321);
    assert_eq!(
        Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4f0c_8420),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0) & u128::from(u64::MAX), 0xbbaa_9988_7766_5544);
    assert_eq!(cpu.vector(0) >> 64, 0x2367_abef_feba_7632);
    assert_eq!(cpu.nzcv.bits(), Nzcv::NEGATIVE | Nzcv::CARRY);
    assert_eq!(cpu.pc, 0x804);
}

#[test]
fn simd_shrn_reserved() {
    assert_eq!(
        Aarch64Decoder::decode(0x0f40_8420),
        Err(crate::Aarch64DecodeError::Reserved)
    );
    // The adjacent unsigned encoding is SQSHRUN, part of the saturating
    // narrowing family rather than a reserved SHRN shape.
    assert!(Aarch64Decoder::decode(0x2f0c_8420).is_ok());
}

#[test]
fn simd_multiply_compare() {
    let mut cpu = Aarch64CpuState::default();
    cpu.set_vector(
        1,
        u128::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]),
    );
    cpu.set_vector(2, u128::from_le_bytes([2; 16]));
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e22_9c20);
    assert_eq!(
        cpu.vector(0).to_le_bytes(),
        [2, 4, 6, 8, 10, 12, 14, 16, 18, 20, 22, 24, 26, 28, 30, 32]
    );

    cpu.set_vector(0, u128::from_le_bytes([1; 16]));
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e22_9420);
    assert_eq!(
        cpu.vector(0).to_le_bytes(),
        [3, 5, 7, 9, 11, 13, 15, 17, 19, 21, 23, 25, 27, 29, 31, 33]
    );
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e22_3420);
    assert_eq!(
        cpu.vector(0).to_le_bytes(),
        [
            0, 0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff
        ]
    );

    cpu.set_vector(
        2,
        u128::from_le_bytes([16, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1]),
    );
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e22_bc20);
    assert_eq!(
        cpu.vector(0).to_le_bytes(),
        [3, 7, 11, 15, 19, 23, 27, 31, 31, 27, 23, 19, 15, 11, 7, 3]
    );
}

#[test]
fn simd_widen_long() {
    let mut cpu = Aarch64CpuState::default();
    cpu.set_vector(
        1,
        u128::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]),
    );
    cpu.set_vector(2, u128::from_le_bytes([2; 16]));
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x0e22_0020);
    assert_eq!(cpu.vector(0), 0x000a_0009_0008_0007_0006_0005_0004_0003);
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e22_0020);
    assert_eq!(cpu.vector(0), 0x0012_0011_0010_000f_000e_000d_000c_000b);
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x0e22_c020);
    assert_eq!(cpu.vector(0), 0x0010_000e_000c_000a_0008_0006_0004_0002);

    cpu.set_vector(1, 0x0008_0007_0006_0005_0004_0003_0002_0001);
    cpu.set_vector(2, 0x0001_0001_0001_0001_0001_0001_0001_0001);
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x0e22_4020);
    assert_eq!(cpu.vector(0), 0);
    cpu.set_vector(0, 0x8877_6655_4433_2211);
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x6e22_4020);
    assert_eq!(cpu.vector(0) & u128::from(u64::MAX), 0x8877_6655_4433_2211);
}

#[test]
fn simd_horizontal_reductions() {
    let mut cpu = Aarch64CpuState::default();
    cpu.set_vector(
        1,
        u128::from_le_bytes([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]),
    );
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e31_b820);
    assert_eq!(cpu.vector(0), 136);
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x6e30_a820);
    assert_eq!(cpu.vector(0), 16);
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e30_3820);
    assert_eq!(cpu.vector(0), 136);

    cpu.set_vector(0, 0xaaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa);
    cpu.set_vector(1, u128::MAX);
    cpu.set_vector(2, 0);
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x6e62_1c20);
    assert_eq!(cpu.vector(0), 0xaaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa);
}

#[test]
fn scalar_pair_add() {
    for source in 0_u32..32 {
        for destination in 0_u32..32 {
            let mut cpu = Aarch64CpuState {
                pc: 0x9000,
                nzcv: crate::Nzcv::from_bits(0xb000_0000),
                fpsr: 0x0800_009f,
                ..Default::default()
            };
            cpu.set_vector(source as u8, 0xffff_ffff_ffff_ffff_0000_0000_0000_0001);
            let word = 0x5ef1_b800 | source << 5 | destination;
            assert_eq!(
                Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, word),
                crate::Aarch64ExecutionExit::Continue,
            );
            assert_eq!(cpu.vector(destination as u8), 0, "{word:#010x}");
            assert_eq!(cpu.pc, 0x9004);
            assert_eq!(cpu.nzcv.bits(), 0xb000_0000);
            assert_eq!(cpu.fpsr, 0x0800_009f);
        }
    }
    for word in [0x5eb1_b800, 0x7ef1_b800] {
        assert_eq!(Aarch64Decoder::decode(word), Err(Aarch64DecodeError::Reserved));
    }
}

#[test]
fn simd_saturating_narrow() {
    let mut cpu = Aarch64CpuState::default();
    cpu.set_vector(1, 0xffff_00ff_ff80_0001_0080_ff7f_8000_7fff);
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x0e21_4820);
    assert_eq!(cpu.vector(0), 0xff7f_8001_7f80_807f);
    assert_ne!(cpu.fpsr & 1 << 27, 0);

    cpu.set_vector(0, 0x8877_6655_4433_2211);
    cpu.fpsr = 0;
    Aarch64Interpreter::execute_word(&mut cpu, &Coordinates, 0x4e21_4820);
    assert_eq!(cpu.vector(0) & u128::from(u64::MAX), 0x8877_6655_4433_2211);
    assert_ne!(cpu.fpsr & 1 << 27, 0);
}

fn single_lanes(lanes: [f32; 4]) -> u128 {
    lanes
        .iter()
        .enumerate()
        .fold(0, |packed, (lane, value)| packed | u128::from(value.to_bits()) << (32 * lane))
}

#[test]
fn fp_step_commits_only_pc_fpsr_and_destination() {
    let mut cpu = Aarch64CpuState {
        pc: 0x2000,
        sp: 0x7fff_0000,
        tls: 0xdead_beef,
        fpcr: 0,
        fpsr: 0,
        nzcv: Nzcv::from_bits(Nzcv::CARRY),
        ..Default::default()
    };
    for register in 0..31 {
        cpu.set_register(register, 0x1000 + u64::from(register));
    }
    for register in 0..32 {
        cpu.set_vector(register, u128::from(register) << 96 | 0x5555);
    }
    cpu.set_vector(1, single_lanes([1.0, 2.0, 3.0, 4.0]));
    cpu.set_vector(2, single_lanes([0.5, 0.5, 0.5, 0.5]));
    let before = cpu.clone();

    // FADD V0.4S, V1.4S, V2.4S
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut Aarch64SoftFloat, 0x4e22_d420),
        Aarch64ExecutionExit::Continue
    );

    assert_eq!(cpu.vector(0), single_lanes([1.5, 2.5, 3.5, 4.5]));
    assert_eq!(cpu.pc, before.pc + 4);
    assert_eq!(cpu.registers, before.registers);
    assert_eq!(
        (cpu.sp, cpu.tls, cpu.fpcr, cpu.nzcv, cpu.exclusive),
        (before.sp, before.tls, before.fpcr, before.nzcv, before.exclusive)
    );
    for register in 1..32 {
        assert_eq!(cpu.vector(register), before.vector(register), "V{register} changed");
    }
}

#[test]
fn fp_destination_aliasing_reads_before_writing() {
    let mut fp = Aarch64SoftFloat;

    // FMLA V0.4S, V0.4S, V0.4S -- the destination is also the addend and both products.
    let mut cpu = Aarch64CpuState::default();
    cpu.set_vector(0, single_lanes([1.0, 2.0, 3.0, 4.0]));
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x4e20_cc00),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), single_lanes([2.0, 6.0, 12.0, 20.0]));

    // FMLA V0.4S, V0.4S, V0.S[0] -- every lane multiplies by lane zero of the destination.
    let mut cpu = Aarch64CpuState::default();
    cpu.set_vector(0, single_lanes([1.0, 2.0, 3.0, 4.0]));
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x4f80_1000),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), single_lanes([2.0, 4.0, 6.0, 8.0]));

    // FADD V0.4S, V0.4S, V0.4S
    let mut cpu = Aarch64CpuState::default();
    cpu.set_vector(0, single_lanes([1.0, 2.0, 3.0, 4.0]));
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x4e20_d400),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), single_lanes([2.0, 4.0, 6.0, 8.0]));

    // FABS V0.4S, V0.4S
    let mut cpu = Aarch64CpuState::default();
    cpu.set_vector(0, single_lanes([-1.0, -2.0, -3.0, -4.0]));
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x4ea0_f800),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), single_lanes([1.0, 2.0, 3.0, 4.0]));

    // FMAXV S0, V0.4S -- every lane is read before the scalar result lands in the same register.
    let mut cpu = Aarch64CpuState::default();
    cpu.set_vector(0, single_lanes([1.0, 4.0, 3.0, 2.0]));
    assert_eq!(
        Aarch64FpExecutor::execute_word(&mut cpu, &mut fp, 0x6e30_f800),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), u128::from(4.0_f32.to_bits()));
}
