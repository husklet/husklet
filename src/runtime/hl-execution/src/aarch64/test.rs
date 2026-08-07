use super::test_support::Coordinates;
use crate::{
    Aarch64CpuState, Aarch64DecodeError, Aarch64Decoder, Aarch64ExecutionExit, Aarch64Instruction, Aarch64Interpreter,
    LogicalOperation, MoveWideOperation, Nzcv, RegisterExtension,
};

const IDENTITY: Coordinates = Coordinates {
    low: 0,
    high: 0,
    size: 0,
};

trait ExecuteWord {
    fn execute_word(&mut self, word: u32) -> Aarch64ExecutionExit;
}

impl ExecuteWord for Aarch64CpuState {
    fn execute_word(&mut self, word: u32) -> Aarch64ExecutionExit {
        Aarch64Interpreter::execute_word(self, &IDENTITY, word)
    }
}

#[test]
fn decoder_matches_authoritative() {
    let cases = [
        (0x9104_8fe0, "add immediate"),
        (0xf100_043f, "cmp immediate"),
        (0x0b04_1462, "add shifted"),
        (0xcb87_30c5, "sub shifted"),
        (0x9208_9d28, "and immediate"),
        (0x3200_1fea, "orr immediate"),
        (0xcacd_1d8b, "eor shifted"),
        (0x0a70_0dee, "bic shifted"),
        (0xaa12_03f1, "mov register"),
        (0xd2d5_79b3, "movz"),
        (0x12a2_4694, "movn"),
        (0xf2ea_cf15, "movk"),
        (0x10ff_fe96, "adr"),
        (0x9000_0017, "adrp"),
    ];
    for (word, name) in cases {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{name}: {word:#010x}");
    }
    assert!(matches!(
        Aarch64Decoder::decode(0xaa12_03f1).unwrap().instruction,
        Aarch64Instruction::LogicalShifted {
            operation: LogicalOperation::Orr,
            source: 31,
            operand: 18,
            destination: 17,
            ..
        }
    ));
    assert_eq!(
        Aarch64Decoder::decode(0xf2ea_cf15).unwrap().instruction,
        Aarch64Instruction::MoveWide {
            operation: MoveWideOperation::Keep,
            destination: 21,
            immediate: 0x5678,
            shift: 48,
        }
    );
}

#[test]
fn decoder_matches_words() {
    let cases = [
        0x17ff_fff2,
        0x97ff_fff1,
        0xd61f_0300,
        0xd63f_0320,
        0xd65f_0340,
        0x54ff_fda1,
        0x34ff_fd9b,
        0xb5ff_fd7c,
        0xb64f_fd5d,
        0x373f_fd3e,
        0xd503_201f,
        0xd402_4681,
        0xd42a_cf00,
    ];
    for word in cases {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
    }
    assert_eq!(
        Aarch64Decoder::decode(0xd402_4681).unwrap().instruction,
        Aarch64Instruction::SupervisorCall { immediate: 0x1234 }
    );
    assert_eq!(
        Aarch64Decoder::decode(0xd42a_cf00).unwrap().instruction,
        Aarch64Instruction::Breakpoint { immediate: 0x5678 }
    );
}

#[test]
fn hint_behavior() {
    // NOP, YIELD, PACIASP, AUTIASP, and BTI c sample the complete hint
    // encoding space used by ordinary AArch64 toolchains.
    for word in [0xd503_201f, 0xd503_203f, 0xd503_233f, 0xd503_23bf, 0xd503_245f] {
        let mut cpu = Aarch64CpuState {
            pc: 0x4000,
            sp: 0x8000,
            nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY),
            ..Default::default()
        };
        cpu.set_register(30, 0x1234_5678);
        let before = cpu.clone();

        assert_eq!(cpu.execute_word(word), Aarch64ExecutionExit::Continue);
        assert_eq!(cpu.pc, before.pc + 4, "hint {word:#010x}");
        cpu.pc = before.pc;
        assert_eq!(cpu, before, "hint {word:#010x} mutated architectural state");
    }
}

#[test]
fn hint_encoding() {
    // Changing Rt leaves the hint space and must not silently become a NOP.
    assert_eq!(
        Aarch64Decoder::decode(0xd503_245e),
        Err(Aarch64DecodeError::Unsupported)
    );
    assert!(matches!(
        Aarch64Decoder::decode(0xd503_3bbf).unwrap().instruction,
        Aarch64Instruction::Barrier { .. }
    ));
}

#[test]
fn local_system_model() {
    let mut cpu = Aarch64CpuState {
        pc: 0x4800,
        tls: 0x1234,
        ..Default::default()
    };
    for (word, register, expected) in [
        (0xd53b_0020, 0, 0x9444_c004),
        (0xd53b_00e1, 1, 4),
        (0xd53b_d062, 2, 0x1234),
        (0xd53b_4223, 3, 0),
    ] {
        assert_eq!(cpu.execute_word(word), Aarch64ExecutionExit::Continue);
        assert_eq!(cpu.register(register), expected);
    }
    assert_eq!(cpu.pc, 0x4810);

    cpu.set_register(4, 0xfeed_face);
    assert_eq!(cpu.execute_word(0xd51b_d044), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.tls, 0xfeed_face);

    let before = cpu.clone();
    assert_eq!(
        cpu.execute_word(0xd538_0000),
        Aarch64ExecutionExit::UndefinedInstruction {
            instruction: before.pc,
            word: 0xd538_0000,
        }
    );
    assert_eq!(cpu, before);
}

#[test]
fn immediate_and_shifted() {
    let mut cpu = Aarch64CpuState {
        sp: 0x1000,
        pc: 0x100,
        ..Default::default()
    };
    assert_eq!(cpu.execute_word(0x9104_8fe0), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(0), 0x1123);
    assert_eq!(cpu.pc, 0x104);

    cpu.set_register(1, 0);
    assert_eq!(cpu.execute_word(0xf100_043f), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(31), 0);
    assert_eq!(cpu.nzcv.bits(), Nzcv::NEGATIVE);

    cpu.set_register(3, u64::MAX);
    cpu.set_register(4, 1);
    assert_eq!(cpu.execute_word(0x0b04_1462), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(2), 31);
    assert_eq!(cpu.register(3), u64::MAX);

    cpu.set_register(6, 0x1000);
    cpu.set_register(7, u64::MAX << 12);
    assert_eq!(cpu.execute_word(0xcb87_30c5), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(5), 0x1001);
}

#[test]
fn extended_register_family() {
    let decoded = Aarch64Decoder::decode(0x8b34_cc40).unwrap();
    assert_eq!(
        decoded.instruction,
        Aarch64Instruction::AddSubtractExtended {
            subtract: false,
            set_flags: false,
            source: 2,
            operand: 20,
            destination: 0,
            extension: RegisterExtension::SignedWord,
            amount: 3,
        }
    );

    let mut cpu = Aarch64CpuState {
        pc: 0x1000,
        sp: 0x2000,
        ..Default::default()
    };
    cpu.set_register(2, 0x100);
    cpu.set_register(20, 0xffff_ffff);
    assert_eq!(cpu.execute_word(0x8b34_cc40), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(0), 0xf8);
    assert_eq!(cpu.pc, 0x1004);

    cpu.pc = 0x2000;
    cpu.set_register(3, 0x1ff);
    assert_eq!(cpu.execute_word(0x8b23_13e1), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(1), 0x2ff0);

    cpu.pc = 0x3000;
    cpu.set_register(4, 3);
    assert_eq!(cpu.execute_word(0xcb24_e7ff), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.sp, 0x1ffa);

    cpu.pc = 0x3500;
    assert_eq!(cpu.execute_word(0x8b3f_73ea), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(10), 0x1ffa);
}

#[test]
fn extended_flags_width() {
    let mut cpu = Aarch64CpuState {
        pc: 0x4000,
        sp: u64::MAX,
        ..Default::default()
    };
    cpu.set_register(5, 1);
    assert_eq!(cpu.execute_word(0xab25_abff), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.sp, u64::MAX);
    assert_eq!(cpu.nzcv.bits(), Nzcv::CARRY);

    cpu.pc = 0x5000;
    cpu.sp = 0xffff_ffff_0000_0001;
    cpu.set_register(7, 0xffff_0002);
    assert_eq!(cpu.execute_word(0x0b27_27e6), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(6), 5);

    cpu.pc = 0x6000;
    cpu.set_register(9, 0);
    cpu.set_register(10, 0xff);
    assert_eq!(cpu.execute_word(0x6b2a_9128), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(8), 16);
    assert_eq!(cpu.nzcv.bits(), 0);
}

#[test]
fn extended_reserved_fault() {
    let shift_five = (0x8b34_cc40 & !(7 << 10)) | (5 << 10);
    assert_eq!(Aarch64Decoder::decode(shift_five), Err(Aarch64DecodeError::Reserved));
    let narrow_double = 0x0b20_0000 | 3 << 13;
    assert_eq!(Aarch64Decoder::decode(narrow_double), Err(Aarch64DecodeError::Reserved));

    let mut cpu = Aarch64CpuState {
        pc: 0x1002,
        sp: 0x2000,
        ..Default::default()
    };
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_word(0x8b34_cc40),
        Aarch64ExecutionExit::AlignmentFault {
            instruction: 0x1002,
            target: 0x1002,
            access: crate::AccessKind::Execute,
        }
    );
    assert_eq!(cpu, before);
}

#[test]
fn add_sub_flag() {
    let vectors = [
        (
            0x7fff_ffff_ffff_ffff,
            1,
            false,
            0x8000_0000_0000_0000,
            Nzcv::NEGATIVE | Nzcv::OVERFLOW,
        ),
        (u64::MAX, 1, false, 0, Nzcv::ZERO | Nzcv::CARRY),
        (0, 1, true, u64::MAX, Nzcv::NEGATIVE),
        (
            0x8000_0000_0000_0000,
            1,
            true,
            0x7fff_ffff_ffff_ffff,
            Nzcv::CARRY | Nzcv::OVERFLOW,
        ),
    ];
    for (left, right, subtract, expected, flags) in vectors {
        let mut cpu = Aarch64CpuState {
            pc: 0x100,
            ..Default::default()
        };
        cpu.set_register(1, left);
        cpu.set_register(2, right);
        let base = if subtract { 0xeb00_0000 } else { 0xab00_0000 };
        let word = base | 2 << 16 | 1 << 5 | 3;
        assert_eq!(cpu.execute_word(word), Aarch64ExecutionExit::Continue);
        assert_eq!(cpu.register(3), expected);
        assert_eq!(cpu.nzcv.bits(), flags);
    }
}

#[test]
fn carry_family() {
    for index in 0_u32..768 {
        let mut choice = index;
        let destination = choice % 3;
        choice /= 3;
        let right_index = choice % 4;
        choice /= 4;
        let left_index = choice % 4;
        choice /= 4;
        let carry = choice & 1 != 0;
        let set_flags = choice >> 1 & 1 != 0;
        let subtract = choice >> 2 & 1 != 0;
        let wide = choice >> 3 & 1 != 0;
        let bits = if wide { 64 } else { 32 };
        let mask = if wide { u64::MAX } else { u64::from(u32::MAX) };
        let sign = 1_u64 << (bits - 1);
        let values = [0, 1, sign, mask];
        let left = values[left_index as usize];
        let right = values[right_index as usize];
        let mut cpu = Aarch64CpuState {
            pc: 0xa000,
            nzcv: Nzcv::from_bits(if carry { Nzcv::CARRY } else { 0 }),
            ..Default::default()
        };
        cpu.set_register(1, left);
        cpu.set_register(2, right);
        let operand = if subtract { !right & mask } else { right };
        let sum = u128::from(left) + u128::from(operand) + u128::from(carry);
        let expected = sum as u64 & mask;
        let overflow = (!(left ^ operand) & (left ^ expected) & sign) != 0;
        let word = u32::from(wide) << 31
            | u32::from(subtract) << 30
            | u32::from(set_flags) << 29
            | 0x1a00_0000
            | 2 << 16
            | 1 << 5
            | destination;
        assert_eq!(cpu.execute_word(word), Aarch64ExecutionExit::Continue);
        assert_eq!(cpu.register(destination as u8), expected, "{word:#x}");
        let flags = if set_flags {
            (u32::from(expected & sign != 0) * Nzcv::NEGATIVE)
                | (u32::from(expected == 0) * Nzcv::ZERO)
                | (u32::from(sum >> bits != 0) * Nzcv::CARRY)
                | (u32::from(overflow) * Nzcv::OVERFLOW)
        } else if carry {
            Nzcv::CARRY
        } else {
            0
        };
        assert_eq!(cpu.nzcv.bits(), flags, "{word:#x}");
        assert_eq!(cpu.pc, 0xa004);
    }
    let mut cpu = Aarch64CpuState {
        pc: 0xb000,
        nzcv: Nzcv::from_bits(Nzcv::CARRY),
        ..Default::default()
    };
    assert_eq!(cpu.execute_word(0x9a1f_03e0), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(0), 1);
    assert_eq!(cpu.execute_word(0x9a1f_03ff), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(31), 0);
}

#[test]
fn logical_masks_aliases() {
    let mut cpu = Aarch64CpuState {
        pc: 0x200,
        ..Default::default()
    };
    cpu.set_register(9, u64::MAX);
    assert_eq!(cpu.execute_word(0x9208_9d28), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(8), 0xff00_ff00_ff00_ff00);

    assert_eq!(cpu.execute_word(0x3200_1fea), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(10), 0xff);

    cpu.set_register(12, 0xaaaa_aaaa_aaaa_aaaa);
    cpu.set_register(13, 0x80);
    assert_eq!(cpu.execute_word(0xcacd_1d8b), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(11), 0xaaaa_aaaa_aaaa_aaab);

    cpu.set_register(18, 0xfeed_face_cafe_beef);
    assert_eq!(cpu.execute_word(0xaa12_03f1), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(17), 0xfeed_face_cafe_beef);
}

#[test]
fn logical_immediate_masks() {
    let cases = [
        (0x9240_0020, 0x0000_0000_0000_0001),
        (0x9200_f062, 0x5555_5555_5555_5555),
        (0xb210_3ca4, 0xffff_0000_ffff_0000),
        (0x5201_04e6, 0x0000_0000_8000_0001),
        (0x7200_9d28, 0x0000_0000_00ff_00ff),
    ];
    for (word, expected) in cases {
        let instruction = Aarch64Decoder::decode(word).unwrap().instruction;
        let Aarch64Instruction::LogicalImmediate { mask, .. } = instruction else {
            panic!("{word:#010x} did not decode as logical immediate");
        };
        assert_eq!(mask, expected, "{word:#010x}");
    }
}

#[test]
fn move_wide_builds() {
    let mut cpu = Aarch64CpuState {
        pc: 0x300,
        ..Default::default()
    };
    assert_eq!(cpu.execute_word(0xd2d5_79b3), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(19), 0x0000_abcd_0000_0000);
    assert_eq!(cpu.execute_word(0x12a2_4694), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(20), 0xedcb_ffff);
    cpu.set_register(21, 0x1122_3344_5566_7788);
    assert_eq!(cpu.execute_word(0xf2ea_cf15), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(21), 0x5678_3344_5566_7788);

    let movz_xzr = 0xd280_0000 | 31;
    assert_eq!(cpu.execute_word(movz_xzr), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(31), 0);
}

#[test]
fn every_mov_alias() {
    let mut cpu = Aarch64CpuState {
        pc: 0x380,
        ..Default::default()
    };
    cpu.set_register(1, 0x1111_2222_3333_4444);
    assert_eq!(cpu.execute_word(0x9100_003f), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.sp, 0x1111_2222_3333_4444);
    assert_eq!(cpu.execute_word(0x9100_03e2), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(2), 0x1111_2222_3333_4444);

    cpu.set_register(4, 0xffff_ffff_89ab_cdef);
    assert_eq!(cpu.execute_word(0x2a04_03e3), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(3), 0x89ab_cdef);
    cpu.set_register(5, 0xffff_ffff_7654_3210);
    assert_eq!(cpu.execute_word(0x1100_00bf), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.sp, 0x7654_3210);
}

#[test]
fn pc_coordinate_port() {
    let coordinates = Coordinates {
        low: 0x400000,
        high: 0x7000_0000_0000,
        size: 0x20_0000,
    };
    let mut cpu = Aarch64CpuState {
        pc: coordinates.high + 0x1030,
        ..Default::default()
    };
    assert_eq!(
        Aarch64Interpreter::execute_word(&mut cpu, &coordinates, 0x10ff_fe96),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(22), coordinates.low + 0x1000);

    cpu.pc = coordinates.high + 0x1234;
    assert_eq!(
        Aarch64Interpreter::execute_word(&mut cpu, &coordinates, 0x9000_0017),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(23), (coordinates.low + 0x1234) & !0xfff);

    cpu.pc = coordinates.high + 0x100;
    let exit = Aarch64Interpreter::execute_word(&mut cpu, &coordinates, 0x9400_0002);
    assert_eq!(
        exit,
        Aarch64ExecutionExit::Branch {
            target: coordinates.high + 0x108
        }
    );
    assert_eq!(cpu.register(30), coordinates.low + 0x104);
    assert_eq!(cpu.pc, coordinates.high + 0x108);
}

#[test]
fn conditional_control_flow() {
    for condition in 0_u8..16 {
        for bits in 0_u32..16 {
            let mut cpu = Aarch64CpuState {
                pc: 0x1000,
                nzcv: Nzcv::from_bits(bits << 28),
                ..Default::default()
            };
            let word = 0x5400_0040 | u32::from(condition);
            let exit = cpu.execute_word(word);
            assert!(matches!(exit, Aarch64ExecutionExit::Branch { .. }));
            assert!(matches!(cpu.pc, 0x1004 | 0x1008));
        }
    }

    let mut cpu = Aarch64CpuState {
        pc: 0x2000,
        ..Default::default()
    };
    cpu.set_register(27, 0x1_0000_0000);
    assert_eq!(
        cpu.execute_word(0x3400_005b),
        Aarch64ExecutionExit::Branch { target: 0x2008 }
    );
    cpu.pc = 0x3000;
    cpu.set_register(29, 1_u64 << 41);
    let tbz_forward = 0xb648_005d;
    assert_eq!(
        cpu.execute_word(tbz_forward),
        Aarch64ExecutionExit::Branch { target: 0x3004 }
    );
}

#[test]
fn indirect_branch_alignment() {
    let mut cpu = Aarch64CpuState {
        pc: 0x4000,
        ..Default::default()
    };
    cpu.set_register(25, 0x123);
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_word(0xd63f_0320),
        Aarch64ExecutionExit::AlignmentFault {
            instruction: 0x4000,
            target: 0x123,
            access: crate::AccessKind::Execute,
        }
    );
    assert_eq!(cpu, before);

    let mut misaligned = Aarch64CpuState {
        pc: 0x4002,
        ..Default::default()
    };
    let before = misaligned.clone();
    assert_eq!(
        misaligned.execute_word(0xd503_201f),
        Aarch64ExecutionExit::AlignmentFault {
            instruction: 0x4002,
            target: 0x4002,
            access: crate::AccessKind::Execute,
        }
    );
    assert_eq!(misaligned, before);

    for (word, expected) in [
        (
            0xd402_4681,
            Aarch64ExecutionExit::Syscall {
                instruction: 0x4000,
                immediate: 0x1234,
            },
        ),
        (
            0xd42a_cf00,
            Aarch64ExecutionExit::Breakpoint {
                instruction: 0x4000,
                immediate: 0x5678,
            },
        ),
        (
            0,
            Aarch64ExecutionExit::UndefinedInstruction {
                instruction: 0x4000,
                word: 0,
            },
        ),
    ] {
        let before = cpu.clone();
        assert_eq!(cpu.execute_word(word), expected);
        assert_eq!(cpu, before);
    }
}

#[test]
fn authenticated_returns_use_unsigned_link_register() {
    for word in [0xd65f_0bff, 0xd65f_0fff] {
        let mut cpu = Aarch64CpuState {
            pc: 0x4000,
            ..Default::default()
        };
        cpu.set_register(30, 0x8000);
        assert_eq!(cpu.execute_word(word), Aarch64ExecutionExit::Branch { target: 0x8000 });
        assert_eq!(cpu.pc, 0x8000);
        assert_eq!(cpu.register(30), 0x8000);
    }
}

#[test]
fn reserved_encoding_sweeps() {
    for index in 0_u32..32 {
        let wide = index / 16 != 0;
        let operation = index / 4 % 4;
        let halfword = index % 4;
        let word = u32::from(wide) << 31 | operation << 29 | 0x1280_0000 | halfword << 21;
        let reserved = operation == 1 || (!wide && halfword >= 2);
        assert_eq!(Aarch64Decoder::decode(word).is_err(), reserved, "{word:#010x}");
    }
    for amount in 32_u32..64 {
        let add = 0x0b00_0000 | amount << 10;
        let logical = 0x0a00_0000 | amount << 10;
        assert_eq!(Aarch64Decoder::decode(add), Err(Aarch64DecodeError::Reserved));
        assert_eq!(Aarch64Decoder::decode(logical), Err(Aarch64DecodeError::Reserved));
    }
    for shift in 0_u32..4 {
        let word = 0x0b00_0000 | shift << 22;
        assert_eq!(Aarch64Decoder::decode(word).is_err(), shift == 3, "{word:#010x}");
    }
    let op2_values = [0_u32, 1, 30, 31];
    let op3_values = [0_u32, 1, 63];
    let op4_values = [0_u32, 1, 31];
    for index in 0_usize..576 {
        let opcode = (index / 36) as u32;
        let op2 = op2_values[index / 9 % 4];
        let op3 = op3_values[index / 3 % 3];
        let op4 = op4_values[index % 3];
        let word = 0xd600_0000 | opcode << 21 | op2 << 16 | op3 << 10 | 1 << 5 | op4;
        let valid = op2 == 31 && op3 == 0 && op4 == 0 && matches!(opcode, 0..=2);
        assert_eq!(Aarch64Decoder::decode(word).is_ok(), valid, "{word:#010x}");
    }

    let mut cpu = Aarch64CpuState {
        pc: 0x5000,
        ..Default::default()
    };
    let before = cpu.clone();
    let reserved_move = 0x3280_0000;
    assert_eq!(
        cpu.execute_word(reserved_move),
        Aarch64ExecutionExit::UndefinedInstruction {
            instruction: 0x5000,
            word: reserved_move,
        }
    );
    assert_eq!(cpu, before);
}

#[test]
fn logical_immediate_decode() {
    for index in 0_u32..16_384 {
        let wide = index >> 13 != 0;
        let n = index >> 12 & 1;
        let imms = index >> 6 & 63;
        let immr = index & 63;
        let word = u32::from(wide) << 31 | 0x1200_0000 | n << 22 | immr << 16 | imms << 10;
        let decoded = Aarch64Decoder::decode(word);
        if !wide && n == 1 {
            assert_eq!(decoded, Err(Aarch64DecodeError::Reserved));
        }
    }
}

#[test]
fn bitfield_family() {
    let mut cpu = Aarch64CpuState {
        pc: 0x1000,
        nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY),
        ..Default::default()
    };
    cpu.set_register(0, 0x1234_5678_89ab_cdef);
    assert_eq!(cpu.execute_word(0xd37c_7c03), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(3), 0x0000_0008_9abc_def0);

    cpu.set_register(0, 0xffff_ffff_8000_0001);
    assert_eq!(cpu.execute_word(0x531f_7804), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(4), 2);
    assert_eq!(cpu.nzcv.bits(), Nzcv::NEGATIVE | Nzcv::CARRY);
    assert_eq!(cpu.pc, 0x1008);
}

#[test]
fn bitfield_boundaries() {
    let source = 0x89ab_cdef_0123_4567_u64;
    let destination = 0xf0e1_d2c3_b4a5_9687_u64;
    for index in 0_u32..15_360 {
        let (wide, width, local) = if index < 3_072 {
            (false, 32_u32, index)
        } else {
            (true, 64_u32, index - 3_072)
        };
        let operation = local / (width * width);
        let rotate = local / width % width;
        let sign_bit = local % width;
        let word = u32::from(wide) << 31
            | operation << 29
            | 0x1300_0000
            | u32::from(wide) << 22
            | rotate << 16
            | sign_bit << 10
            | 1 << 5;
        let mut cpu = Aarch64CpuState::default();
        cpu.set_register(0, destination);
        cpu.set_register(1, source);
        assert_eq!(cpu.execute_word(word), Aarch64ExecutionExit::Continue);

        let mask = u64::MAX >> (64 - width);
        let narrowed = source & mask;
        let rotated = (narrowed >> rotate | narrowed.wrapping_shl(width - rotate)) & mask;
        let ones = |bits: u32| u64::MAX >> (64 - bits);
        let field_source = ones(sign_bit + 1);
        let field = (field_source >> rotate | field_source.wrapping_shl(width - rotate)) & mask;
        let top = ones((sign_bit.wrapping_sub(rotate) & (width - 1)) + 1) & mask;
        let bottom = if operation == 1 {
            destination & !field | rotated & field
        } else {
            rotated & field
        };
        let fill = if operation == 1 {
            destination
        } else if operation == 0 && source >> sign_bit & 1 != 0 {
            u64::MAX
        } else {
            0
        };
        let expected = (fill & !top | bottom & top) & mask;
        assert_eq!(cpu.register(0), expected, "{word:#010x}");
    }
}

#[test]
fn bitfield_reserved() {
    for word in [0x7300_0000, 0x1340_0000, 0x1320_0000, 0x1300_8000] {
        assert_eq!(Aarch64Decoder::decode(word), Err(Aarch64DecodeError::Reserved));
    }
}

#[test]
fn multiply_family() {
    let encode = |wide: bool, operation: u32, subtract: bool, addend: u32| {
        u32::from(wide) << 31
            | 0x1b00_0000
            | operation << 21
            | 2 << 16
            | u32::from(subtract) << 15
            | addend << 10
            | 1 << 5
    };
    let mut cpu = Aarch64CpuState {
        pc: 0x2000,
        nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY),
        ..Default::default()
    };
    cpu.set_register(1, u64::MAX - 1);
    cpu.set_register(2, 3);
    cpu.set_register(3, 10);
    for (word, expected) in [
        (encode(true, 0, false, 3), 4),
        (encode(true, 0, true, 3), 16),
        (encode(false, 0, false, 3), 4),
        (encode(false, 0, true, 3), 16),
        (encode(true, 1, false, 3), 4),
        (encode(true, 1, true, 3), 16),
        (encode(true, 5, false, 3), 12_884_901_892),
        (encode(true, 5, true, 3), 0xffff_fffd_0000_0010),
    ] {
        assert_eq!(cpu.execute_word(word), Aarch64ExecutionExit::Continue);
        assert_eq!(cpu.register(0), expected, "{word:#010x}");
    }
    assert_eq!(cpu.nzcv.bits(), Nzcv::NEGATIVE | Nzcv::CARRY);
    assert_eq!(cpu.pc, 0x2020);
}

#[test]
fn multiply_high() {
    let mut cpu = Aarch64CpuState {
        pc: 0x3000,
        ..Default::default()
    };
    cpu.set_register(1, 0x8000_0000_0000_0000);
    cpu.set_register(2, 2);
    assert_eq!(cpu.execute_word(0x9b42_7c20), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(0), u64::MAX);
    cpu.set_register(1, u64::MAX);
    assert_eq!(cpu.execute_word(0x9bc2_7c20), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(0), 1);
}

#[test]
fn multiply_reserved() {
    for word in [
        0x1b20_0000,
        0x1b40_7c00,
        0x9b40_fc00,
        0x9b40_7800,
        0x9b60_0000,
        0x9b80_0000,
        0x9be0_0000,
    ] {
        assert_eq!(Aarch64Decoder::decode(word), Err(Aarch64DecodeError::Reserved));
    }
}

#[test]
fn variable_shift_family() {
    let source = 0x8000_0001_89ab_cdef_u64;
    for index in 0_u32..2_048 {
        let wide = index >= 1_024;
        let local = index % 1_024;
        let operation = local / 256;
        let count = local % 256;
        let word = u32::from(wide) << 31 | 0x1ac0_2000 | operation << 10 | 2 << 16 | 1 << 5;
        let mut cpu = Aarch64CpuState::default();
        cpu.set_register(1, source);
        cpu.set_register(2, u64::from(count));
        assert_eq!(cpu.execute_word(word), Aarch64ExecutionExit::Continue);
        let amount = count & if wide { 63 } else { 31 };
        let expected = match (wide, operation) {
            (true, 0) => source << amount,
            (true, 1) => source >> amount,
            (true, 2) => ((source as i64) >> amount) as u64,
            (true, _) => source.rotate_right(amount),
            (false, 0) => u64::from((source as u32) << amount),
            (false, 1) => u64::from((source as u32) >> amount),
            (false, 2) => u64::from(((source as i32) >> amount) as u32),
            (false, _) => u64::from((source as u32).rotate_right(amount)),
        };
        assert_eq!(cpu.register(0), expected, "{word:#010x} count={count}");
    }
}

#[test]
fn variable_shift_state() {
    let mut cpu = Aarch64CpuState {
        pc: 0x4000,
        nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY),
        ..Default::default()
    };
    cpu.set_register(1, 7);
    assert_eq!(cpu.execute_word(0x1adf_203f), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(31), 0);
    assert_eq!(cpu.nzcv.bits(), Nzcv::NEGATIVE | Nzcv::CARRY);
    assert_eq!(cpu.pc, 0x4004);
    for word in [0x3ac0_2020, 0x5ac0_2020] {
        assert_eq!(Aarch64Decoder::decode(word), Err(Aarch64DecodeError::Reserved));
    }
}

#[test]
fn reverse_family() {
    let expected = |wide: bool, operation: u32, source: u64| match (wide, operation) {
        (false, 1) => u64::from((source as u32).swap_bytes().rotate_left(16)),
        (false, 2) => u64::from((source as u32).swap_bytes()),
        (true, 1) => {
            u64::from((source as u16).swap_bytes())
                | u64::from(((source >> 16) as u16).swap_bytes()) << 16
                | u64::from(((source >> 32) as u16).swap_bytes()) << 32
                | u64::from(((source >> 48) as u16).swap_bytes()) << 48
        }
        (true, 2) => u64::from((source as u32).swap_bytes()) | u64::from(((source >> 32) as u32).swap_bytes()) << 32,
        (true, 3) => source.swap_bytes(),
        _ => unreachable!(),
    };
    for (wide, operation, word) in [
        (false, 1_u32, 0x5ac0_0420),
        (false, 2, 0x5ac0_0862),
        (true, 1, 0xdac0_04a4),
        (true, 2, 0xdac0_08e6),
        (true, 3, 0xdac0_0d28),
    ] {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
        for source in [0, u64::MAX, 0x0123_4567_89ab_cdef, 0x8001_7ffe_55aa_a55a] {
            let mut cpu = Aarch64CpuState::default();
            cpu.set_register((word >> 5 & 31) as u8, source);
            assert_eq!(cpu.execute_word(word), Aarch64ExecutionExit::Continue);
            assert_eq!(
                cpu.register((word & 31) as u8),
                expected(wide, operation, source),
                "{word:#010x}"
            );
        }
    }
}

#[test]
fn reverse_state() {
    let mut cpu = Aarch64CpuState {
        pc: 0x6000,
        nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY),
        ..Default::default()
    };
    cpu.set_register(4, 0x0123_4567_89ab_cdef);
    assert_eq!(cpu.execute_word(0xdac0_0c84), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(4), 0xefcd_ab89_6745_2301);
    assert_eq!(cpu.nzcv.bits(), Nzcv::NEGATIVE | Nzcv::CARRY);
    assert_eq!(cpu.pc, 0x6004);
    assert_eq!(cpu.execute_word(0xdac0_0fff), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(31), 0);
}

#[test]
fn reverse_reserved() {
    assert_eq!(Aarch64Decoder::decode(0x5ac0_0c20), Err(Aarch64DecodeError::Reserved));
    assert_eq!(
        Aarch64Decoder::decode(0x5ac1_0420),
        Err(Aarch64DecodeError::Unsupported)
    );
}

#[test]
fn rbit_family() {
    for index in 0_u8..96 {
        let wide = index >= 32;
        let width = if wide { 64 } else { 32 };
        let bit = index - u8::from(wide) * 32;
        let source = 1_u64 << bit;
        let expected = 1_u64 << (width - 1 - bit);
        let word = u32::from(wide) << 31 | 0x5ac0_0000 | 1 << 5;
        let mut cpu = Aarch64CpuState::default();
        cpu.set_register(1, source | if wide { 0 } else { 0xffff_ffff_0000_0000 });
        assert_eq!(cpu.execute_word(word), Aarch64ExecutionExit::Continue);
        assert_eq!(cpu.register(0), expected, "{word:#010x} bit={bit}");
    }
    for (source, expected) in [
        (0_u64, 0_u64),
        (u64::MAX, u64::MAX),
        (0x0123_4567_89ab_cdef, 0xf7b3_d591_e6a2_c480),
        (0x8001_7ffe_55aa_a55a, 0x5aa5_55aa_7ffe_8001),
    ] {
        let mut cpu = Aarch64CpuState::default();
        cpu.set_register(1, source);
        assert_eq!(cpu.execute_word(0xdac0_0020), Aarch64ExecutionExit::Continue);
        assert_eq!(cpu.register(0), expected);
    }
}

#[test]
fn rbit_state() {
    let mut cpu = Aarch64CpuState {
        pc: 0x6200,
        nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY),
        ..Default::default()
    };
    cpu.set_register(2, 0x0123_4567_89ab_cdef);
    assert_eq!(cpu.execute_word(0xdac0_0042), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(2), 0xf7b3_d591_e6a2_c480);
    assert_eq!(cpu.nzcv.bits(), Nzcv::NEGATIVE | Nzcv::CARRY);
    assert_eq!(cpu.pc, 0x6204);
    assert_eq!(cpu.execute_word(0xdac0_03ff), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(31), 0);
    assert_eq!(cpu.pc, 0x6208);
}

#[test]
fn rbit_decode() {
    for word in [0x5ac0_0020, 0xdac0_0062, 0x5ac0_03ff, 0xdac0_03ff] {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
    }
    assert!(Aarch64Decoder::decode(0x5ac0_0420).is_ok());
    assert_eq!(
        Aarch64Decoder::decode(0x5ac1_0020),
        Err(Aarch64DecodeError::Unsupported)
    );
}

#[test]
fn clz_family() {
    let cases = [(false, 32_u32), (true, 64)]
        .into_iter()
        .flat_map(|(wide, width)| (0..=width).map(move |leading| (wide, width, leading)));
    for (wide, width, leading) in cases {
        let word = u32::from(wide) << 31 | 0x5ac0_1000 | 1 << 5;
        let source = if leading == width {
            0
        } else {
            1_u64 << (width - leading - 1)
        };
        let mut cpu = Aarch64CpuState::default();
        let source = source | source.saturating_sub(1);
        cpu.set_register(1, source | if wide { 0 } else { 0xffff_ffff_0000_0000 });
        assert_eq!(cpu.execute_word(word), Aarch64ExecutionExit::Continue);
        assert_eq!(cpu.register(0), u64::from(leading), "{word:#010x} {leading}");
    }
}

#[test]
fn clz_state() {
    let mut cpu = Aarch64CpuState {
        pc: 0x6100,
        nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY),
        ..Default::default()
    };
    cpu.set_register(4, 0x0000_0000_0000_1000);
    assert_eq!(cpu.execute_word(0xdac0_1084), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(4), 51);
    assert_eq!(cpu.nzcv.bits(), Nzcv::NEGATIVE | Nzcv::CARRY);
    assert_eq!(cpu.pc, 0x6104);
    assert_eq!(cpu.execute_word(0xdac0_13ff), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(31), 0);
}

#[test]
fn clz_decode_boundaries() {
    for word in [0x5ac0_1020, 0xdac0_1062, 0x5ac0_13ff, 0xdac0_13ff] {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
    }
    // CLS is an adjacent valid one-source operation and remains unsupported,
    // while a nonzero opcode field is outside the CLZ encoding.
    for word in [0x5ac0_1420, 0x5ac1_1020] {
        assert_eq!(Aarch64Decoder::decode(word), Err(Aarch64DecodeError::Unsupported));
    }
}

#[test]
fn divide_boundaries() {
    let cases = [
        (false, false, 10, 3, 3),
        (false, false, u32::MAX as u64, 2, 0x7fff_ffff),
        (false, false, 10, 0, 0),
        (false, true, (-10_i32) as u32 as u64, 3, (-3_i32) as u32 as u64),
        (
            false,
            true,
            i32::MIN as u32 as u64,
            u32::MAX as u64,
            i32::MIN as u32 as u64,
        ),
        (false, true, i32::MIN as u32 as u64, 0, 0),
        (true, false, 10, 3, 3),
        (true, false, u64::MAX, 2, 0x7fff_ffff_ffff_ffff),
        (true, false, 10, 0, 0),
        (true, true, (-10_i64) as u64, 3, (-3_i64) as u64),
        (true, true, i64::MIN as u64, u64::MAX, i64::MIN as u64),
        (true, true, i64::MIN as u64, 0, 0),
    ];
    for (wide, signed, left, right, expected) in cases {
        let word = u32::from(wide) << 31 | 0x1ac0_0800 | u32::from(signed) << 10 | 2 << 16 | 1 << 5;
        let mut cpu = Aarch64CpuState::default();
        cpu.set_register(1, left);
        cpu.set_register(2, right);
        assert_eq!(cpu.execute_word(word), Aarch64ExecutionExit::Continue);
        assert_eq!(cpu.register(0), expected, "{word:#010x}");
    }
}

#[test]
fn divide_state() {
    let mut cpu = Aarch64CpuState {
        pc: 0x5000,
        nzcv: Nzcv::from_bits(Nzcv::NEGATIVE | Nzcv::CARRY),
        ..Default::default()
    };
    assert_eq!(cpu.execute_word(0x1adf_0bff), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(31), 0);
    assert_eq!(cpu.nzcv.bits(), Nzcv::NEGATIVE | Nzcv::CARRY);
    assert_eq!(cpu.pc, 0x5004);
    assert_eq!(Aarch64Decoder::decode(0x3ac0_0820), Err(Aarch64DecodeError::Reserved));
}

#[test]
fn conditional_compare_family() {
    let truth = [
        0xf0f0_u16, 0x0f0f, 0xcccc, 0x3333, 0xff00, 0x00ff, 0xaaaa, 0x5555, 0x0c0c, 0xf3f3, 0xaa55, 0x55aa, 0x0a05,
        0xf5fa, 0xffff, 0xffff,
    ];
    for index in 0_u32..2_048 {
        let flags = index & 15;
        let condition = index >> 4 & 15;
        let subtract = index >> 8 & 1;
        let immediate = index >> 9 & 1;
        let wide = index >> 10 & 1;
        let literal = condition ^ 15;
        let word =
            wide << 31 | subtract << 30 | 0x3a40_0000 | 10 << 16 | condition << 12 | immediate << 11 | 1 << 5 | literal;
        let mut cpu = Aarch64CpuState {
            pc: 0x6000,
            nzcv: Nzcv::from_bits(flags << 28),
            ..Default::default()
        };
        cpu.set_register(1, 5);
        cpu.set_register(10, 10);
        let registers = cpu.registers;
        assert_eq!(cpu.execute_word(word), Aarch64ExecutionExit::Continue);
        let holds = truth[condition as usize] >> flags & 1 != 0;
        let expected = match (holds, subtract) {
            (true, 1) => 8,
            (true, _) => 0,
            (false, _) => literal,
        };
        assert_eq!(cpu.nzcv.bits(), expected << 28, "{word:#010x} flags={flags:x}");
        assert_eq!(cpu.registers, registers);
        assert_eq!(cpu.pc, 0x6004);
    }
}

#[test]
fn conditional_compare_zr() {
    let mut cpu = Aarch64CpuState {
        pc: 0x7000,
        ..Default::default()
    };
    assert_eq!(cpu.execute_word(0xfa5f_e3ea), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.nzcv.bits(), Nzcv::ZERO | Nzcv::CARRY);
    for word in [0x1a40_0000, 0x3a40_0400, 0x3a40_0010] {
        assert_eq!(Aarch64Decoder::decode(word), Err(Aarch64DecodeError::Reserved));
    }
}

#[test]
fn conditional_select_family() {
    let mut cpu = Aarch64CpuState {
        pc: 0x1000,
        ..Default::default()
    };
    cpu.set_register(1, 7);
    cpu.set_register(2, 11);
    cpu.nzcv = crate::Nzcv::from_bits(1 << 30);

    for (word, expected) in [
        (0x9a82_0020, 7),
        (0x9a82_1020, 11),
        (0x9a82_1420, 12),
        (0xda82_1020, !11),
        (0xda82_1420, (!11_u64).wrapping_add(1)),
    ] {
        cpu.pc = 0x1000;
        assert_eq!(cpu.execute_word(word), Aarch64ExecutionExit::Continue);
        assert_eq!(cpu.register(0), expected, "{word:#010x}");
    }

    cpu.nzcv = crate::Nzcv::from_bits(0);
    cpu.pc = 0x1000;
    assert_eq!(cpu.execute_word(0x9a9f_07e0), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(0), 1);
}

#[test]
fn extract_registers() {
    let mut cpu = Aarch64CpuState {
        pc: 0x8000,
        ..Default::default()
    };
    cpu.set_register(0, 0x8000_0001);
    assert_eq!(cpu.execute_word(0x1380_0800), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(0), 0x6000_0000);

    cpu.set_register(1, 0x1122_3344_1122_3344);
    cpu.set_register(2, 0x5566_7788_5566_7788);
    assert_eq!(cpu.execute_word(0x1382_2023), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(3), 0x4455_6677);
    assert_eq!(cpu.execute_word(0x93c2_2023), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(3), 0x4455_6677_8855_6677);
    for word in [0x13c0_0000, 0x9380_0000, 0x1380_8000] {
        assert_eq!(Aarch64Decoder::decode(word), Err(Aarch64DecodeError::Reserved));
    }
}

#[test]
fn integer_step_commits_scalars_and_leaves_vectors() {
    let mut cpu = Aarch64CpuState {
        pc: 0x1000,
        sp: 0x7ff0,
        tls: 0xdead_beef,
        ..Default::default()
    };
    cpu.vectors[0] = u128::MAX;
    cpu.vectors[31] = 0x0123_4567_89ab_cdef;
    let vectors = cpu.vectors;
    cpu.set_register(1, 5);
    // ADD X0, X1, #7
    assert_eq!(cpu.execute_word(0x9100_1c20), Aarch64ExecutionExit::Continue);
    assert_eq!(cpu.register(0), 12);
    assert_eq!(cpu.pc, 0x1004);
    assert_eq!(cpu.sp, 0x7ff0);
    assert_eq!(cpu.vectors, vectors);
}

#[test]
fn misaligned_indirect_branch_commits_nothing() {
    let mut cpu = Aarch64CpuState {
        pc: 0x1000,
        ..Default::default()
    };
    cpu.vectors[2] = 0xfeed;
    cpu.set_register(9, 0x2002);
    cpu.set_register(30, 0xaaaa);
    let before = cpu.clone();
    // BLR X9 to a misaligned target: the link register must not be written.
    assert_eq!(
        cpu.execute_word(0xd63f_0120),
        Aarch64ExecutionExit::AlignmentFault {
            instruction: 0x1000,
            target: 0x2002,
            access: crate::AccessKind::Execute,
        }
    );
    assert_eq!(cpu, before);
}
