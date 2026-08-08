use crate::{Aarch64CpuState, Aarch64DecodeError, Aarch64Decoder, Aarch64ExecutionExit, BarrierKind, GuestSystemPort};

use super::Executor;

#[derive(Default)]
struct System {
    barriers: Vec<(BarrierKind, u8)>,
    invalidations: Vec<u64>,
    frequency: u64,
    counter: u64,
}

impl GuestSystemPort for System {
    fn barrier(&mut self, kind: BarrierKind, option: u8) {
        self.barriers.push((kind, option));
    }

    fn invalidate_instruction(&mut self, address: u64) {
        self.invalidations.push(address);
    }

    fn counter_frequency(&self) -> u64 {
        self.frequency
    }

    fn counter_value(&self) -> u64 {
        self.counter
    }
}

fn execute(cpu: &mut Aarch64CpuState, system: &mut System, word: u32) -> Aarch64ExecutionExit {
    match Aarch64Decoder::decode(word) {
        Ok(ir) => Executor::execute(cpu, system, ir),
        Err(Aarch64DecodeError::Reserved) => Aarch64ExecutionExit::UndefinedInstruction {
            instruction: cpu.pc,
            word,
        },
        Err(Aarch64DecodeError::Unsupported) => Aarch64ExecutionExit::UnsupportedInstruction {
            instruction: cpu.pc,
            word,
        },
    }
}

#[test]
fn decoder_words() {
    let words = [
        0xd503_3bbf,
        0xd503_3f9f,
        0xd503_3fdf,
        0xd53b_4200,
        0xd51b_4201,
        0xd53b_4402,
        0xd51b_4403,
        0xd53b_4424,
        0xd51b_4425,
        0xd53b_d046,
        0xd51b_d047,
        0xd53b_e048,
        0xd53b_e009,
    ];
    for word in words {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
    }
}

#[test]
fn data_cache_clean_is_a_translated_noop() {
    // cvau, cvac, cvap, cvadp and civac: every EL0-permitted clean variant.
    for base in [0xd50b_7b20_u32, 0xd50b_7a20, 0xd50b_7c20, 0xd50b_7d20, 0xd50b_7e20] {
        for source in 0..32_u32 {
            let word = base | source;
            let ir = Aarch64Decoder::decode(word).unwrap();
            assert_eq!(ir.instruction, crate::Aarch64Instruction::Nop, "{word:#010x}");
        }
    }
}

#[test]
fn barriers_and_registers() {
    let mut system = System {
        frequency: 1_000_000_000,
        counter: 0x1234_5678,
        ..Default::default()
    };
    let mut cpu = Aarch64CpuState {
        pc: 0x6000,
        ..Default::default()
    };
    for word in [0xd503_3bbf, 0xd503_3f9f, 0xd503_3fdf] {
        execute(&mut cpu, &mut system, word);
    }
    assert_eq!(
        system.barriers,
        [
            (BarrierKind::DataMemory, 11),
            (BarrierKind::DataSynchronization, 15),
            (BarrierKind::InstructionSynchronization, 15),
        ]
    );

    cpu.set_register(1, u64::MAX);
    execute(&mut cpu, &mut system, 0xd51b_4201);
    execute(&mut cpu, &mut system, 0xd53b_4200);
    assert_eq!(cpu.register(0), 0xf000_0000);
    cpu.set_register(3, u64::MAX);
    execute(&mut cpu, &mut system, 0xd51b_4403);
    execute(&mut cpu, &mut system, 0xd53b_4402);
    assert_eq!(cpu.register(2), 0x07c8_0000);
    cpu.set_register(5, u64::MAX);
    execute(&mut cpu, &mut system, 0xd51b_4425);
    execute(&mut cpu, &mut system, 0xd53b_4424);
    assert_eq!(cpu.register(4), 0x0800_009f);
    cpu.set_register(7, 0xfeed_face);
    execute(&mut cpu, &mut system, 0xd51b_d047);
    execute(&mut cpu, &mut system, 0xd53b_d046);
    assert_eq!(cpu.register(6), 0xfeed_face);
    execute(&mut cpu, &mut system, 0xd53b_e048);
    execute(&mut cpu, &mut system, 0xd53b_e009);
    assert_eq!((cpu.register(8), cpu.register(9)), (0x1234_5678, 1_000_000_000));

    execute(&mut cpu, &mut system, 0xd53b_002a);
    execute(&mut cpu, &mut system, 0xd53b_00eb);
    execute(&mut cpu, &mut system, 0xd53b_d06c);
    execute(&mut cpu, &mut system, 0xd53b_422d);
    assert_eq!(cpu.register(10), 0x9444_c004);
    assert_eq!(cpu.register(11), 4);
    assert_eq!(cpu.register(12), 0xfeed_face);
    assert_eq!(cpu.register(13), 0);

    for (word, register) in [(0xd53b_e02e, 14), (0xd53b_e04f, 15), (0xd53b_e0d0, 16)] {
        execute(&mut cpu, &mut system, word);
        assert_eq!(cpu.register(register), 0x1234_5678);
    }
}

/// The portless interpreter entry points have no counter, so counter reads must
/// refuse rather than answer with a third timebase of their own.
#[test]
fn portless_counter_reads_refuse_instead_of_inventing_a_timebase() {
    for word in [0xd53b_e000_u32, 0xd53b_e040, 0xd53b_e020, 0xd53b_e0c0] {
        let mut cpu = Aarch64CpuState {
            pc: 0x6000,
            ..Default::default()
        };
        assert_eq!(
            crate::Aarch64Interpreter::execute_word(&mut cpu, &crate::aarch64::coordinate::Identity, word),
            Aarch64ExecutionExit::UnsupportedInstruction {
                instruction: 0x6000,
                word
            }
        );
        assert_eq!(cpu.pc, 0x6000);
    }
}

#[test]
fn instruction_cache() {
    let mut system = System::default();
    let mut cpu = Aarch64CpuState {
        pc: 0x7000,
        nzcv: crate::Nzcv::from_bits(0xf000_0000),
        ..Default::default()
    };
    for source in 0_u32..32 {
        cpu.pc = 0x7000;
        cpu.set_register(source as u8, 0x1000 + u64::from(source));
        assert_eq!(
            execute(&mut cpu, &mut system, 0xd50b_7520 | source),
            Aarch64ExecutionExit::Continue,
        );
        assert_eq!(cpu.pc, 0x7004);
        assert_eq!(cpu.nzcv.bits(), 0xf000_0000);
    }
    let expected = (0_u64..31)
        .map(|source| 0x1000 + source)
        .chain(core::iter::once(0))
        .collect::<Vec<_>>();
    assert_eq!(system.invalidations, expected);
}

#[test]
fn reserved_and_unsupported() {
    let unsupported = 0xd53b_0040;
    assert_eq!(
        Aarch64Decoder::decode(unsupported),
        Err(Aarch64DecodeError::Unsupported)
    );
    assert_eq!(Aarch64Decoder::decode(0xd538_0000), Err(Aarch64DecodeError::Reserved));
    assert_eq!(
        Aarch64Decoder::decode(0xd51b_00e0),
        Err(Aarch64DecodeError::Unsupported)
    );
    let mut system = System::default();
    let mut cpu = Aarch64CpuState {
        pc: 0x7000,
        ..Default::default()
    };
    let before = cpu.clone();
    assert_eq!(
        execute(&mut cpu, &mut system, unsupported),
        Aarch64ExecutionExit::UnsupportedInstruction {
            instruction: 0x7000,
            word: unsupported,
        }
    );
    assert_eq!(cpu, before);
}
