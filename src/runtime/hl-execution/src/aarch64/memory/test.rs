use std::collections::HashMap;

use crate::{
    Aarch64CpuState, Aarch64DecodeError, Aarch64Decoder, Aarch64ExecutionExit, Aarch64Instruction, Aarch64Interpreter,
    AccessKind, FaultAccess, GuestOperandMemory, IndexExtension, LoadExtension, MemoryAddress, MemoryFault,
    MemoryWidth, PcCoordinatePort, Writeback, aarch64::test_support::Coordinates,
};

const IDENTITY: Coordinates = Coordinates {
    low: 0,
    high: 0,
    size: 0,
};

#[derive(Default)]
struct Memory {
    bytes: HashMap<u64, u8>,
    read_fault: Option<u64>,
    write_fault: Option<u64>,
    writes: Vec<(u64, u8, u64)>,
}

impl Memory {
    fn put(&mut self, address: u64, width: u8, value: u64) {
        for offset in 0..width {
            self.bytes
                .insert(address.wrapping_add(u64::from(offset)), (value >> (offset * 8)) as u8);
        }
    }

    fn get(&self, address: u64, width: u8) -> u64 {
        let mut value = 0;
        for offset in 0..width {
            value |= u64::from(
                self.bytes
                    .get(&address.wrapping_add(u64::from(offset)))
                    .copied()
                    .unwrap_or(0),
            ) << (offset * 8);
        }
        value
    }

    fn touches(address: u64, width: u8, fault: Option<u64>) -> bool {
        fault.is_some_and(|fault| (0..width).any(|offset| address.wrapping_add(u64::from(offset)) == fault))
    }
}

impl GuestOperandMemory for Memory {
    type Reservation = (u64, u8);
    type BatchReservation = Vec<(u64, u8)>;

    fn read(&self, address: u64, bytes: u8) -> Result<u64, ()> {
        if Self::touches(address, bytes, self.read_fault) {
            Err(())
        } else {
            Ok(self.get(address, bytes))
        }
    }

    fn reserve_write(&self, address: u64, bytes: u8) -> Result<Self::Reservation, ()> {
        if Self::touches(address, bytes, self.write_fault) {
            Err(())
        } else {
            Ok((address, bytes))
        }
    }

    fn commit_write(&mut self, reservation: Self::Reservation, value: u64) -> Result<(), ()> {
        self.put(reservation.0, reservation.1, value);
        self.writes.push((reservation.0, reservation.1, value));
        Ok(())
    }
    fn reserve_write_batch(&self, writes: &[(u64, u8)]) -> Result<Self::BatchReservation, u64> {
        writes
            .iter()
            .map(|(address, bytes)| self.reserve_write(*address, *bytes).map_err(|()| *address))
            .collect()
    }
    fn commit_write_batch(&mut self, reservations: Self::BatchReservation, values: &[u64]) -> Result<(), ()> {
        if reservations.len() != values.len() {
            return Err(());
        }
        for (reservation, value) in reservations.into_iter().zip(values) {
            self.commit_write(reservation, *value)?;
        }
        Ok(())
    }
}

trait ExecuteMemoryWord {
    fn execute_memory(
        &mut self,
        memory: &mut Memory,
        coordinates: &dyn PcCoordinatePort,
        word: u32,
    ) -> Aarch64ExecutionExit;
}

impl ExecuteMemoryWord for Aarch64CpuState {
    fn execute_memory(
        &mut self,
        memory: &mut Memory,
        coordinates: &dyn PcCoordinatePort,
        word: u32,
    ) -> Aarch64ExecutionExit {
        Aarch64Interpreter::execute_memory(self, memory, coordinates, word)
    }
}

#[test]
fn decoder_matches_gnu() {
    let cases = [
        0x3900_0c20,
        0x3940_1062,
        0x3980_14a4,
        0x39c0_18e6,
        0x7900_1528,
        0x7940_196a,
        0x7980_1dac,
        0x79c0_21ee,
        0xb900_1630,
        0xb940_1a72,
        0xb980_1eb4,
        0xf900_13f6,
        0xf940_17f7,
        0xf81f_8020,
        0xb85f_c062,
        0xf801_0ca4,
        0xf85f_04e6,
        0xf86a_5928,
        0xb82d_d98b,
        0x3870_69ee,
        0x78b3_fa51,
    ];
    for word in cases {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
    }
    assert_eq!(
        Aarch64Decoder::decode(0xf86a_5928).unwrap().instruction,
        Aarch64Instruction::Load {
            destination: 8,
            width: MemoryWidth::Double,
            extension: LoadExtension::Zero,
            address: MemoryAddress::Register {
                base: 9,
                index: 10,
                extension: IndexExtension::Unsigned32,
                shift: 3,
            },
        }
    );
}

#[test]
fn decoder_matches_words() {
    let cases = [
        0x58ff_fd74,
        0x98ff_fd55,
        0xa9bf_07e0,
        0xa8c1_0fe2,
        0x2901_94c4,
        0x297f_2127,
        0x6941_2d8a,
    ];
    for word in cases {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
    }
    assert_eq!(
        Aarch64Decoder::decode(0xa9bf_07e0).unwrap().instruction,
        Aarch64Instruction::StorePair {
            first: 0,
            second: 1,
            width: MemoryWidth::Double,
            address: MemoryAddress::Base {
                register: 31,
                displacement: -16,
                writeback: Writeback::PreIndex,
            },
        }
    );
}

#[test]
fn scaled_load_store() {
    let mut memory = Memory::default();
    let mut cpu = Aarch64CpuState {
        pc: 0x1000,
        ..Default::default()
    };
    cpu.set_register(1, 0x2000);
    cpu.set_register(0, 0x1122_3344_5566_7788);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x3900_0c20),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(memory.get(0x2003, 1), 0x88);

    memory.put(0x2005, 1, 0x80);
    cpu.set_register(5, 0x2000);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x3980_14a4),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(4), u64::MAX - 0x7f);

    memory.put(0x2006, 1, 0x80);
    cpu.set_register(7, 0x2000);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x39c0_18e6),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(6), 0xffff_ff80);
}

#[test]
fn vector_scaled_family() {
    let words = [
        0x3d00_0c20,
        0x3d40_1062,
        0x7d00_0ca4,
        0x7d40_10e6,
        0xbd00_0d28,
        0xbd40_116a,
        0xfd00_0dac,
        0xfd40_11ee,
        0x3d80_0e30,
        0x3dc0_13f2,
    ];
    for word in words {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
    }
    assert_eq!(
        Aarch64Decoder::decode(0x3d80_0e30).unwrap().instruction,
        Aarch64Instruction::VectorStore {
            source: 16,
            bytes: 16,
            address: MemoryAddress::Base {
                register: 17,
                displacement: 48,
                writeback: Writeback::None,
            },
        }
    );

    let mut memory = Memory::default();
    let mut cpu = Aarch64CpuState {
        pc: 0x7000,
        sp: 0x9000,
        ..Default::default()
    };
    cpu.set_register(17, 0x8000);
    cpu.set_vector(16, 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x3d80_0e30),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(memory.get(0x8030, 8), 0x99aa_bbcc_ddee_ff00);
    assert_eq!(memory.get(0x8038, 8), 0x1122_3344_5566_7788);
    assert_eq!(cpu.pc, 0x7004);

    cpu.set_vector(18, u128::MAX);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x3dc0_13f2),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(18), 0);

    memory.put(0x8104, 1, 0x5a);
    cpu.set_register(3, 0x8100);
    cpu.set_vector(2, u128::MAX);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x3d40_1062),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(2), 0x5a);
}

#[test]
fn vector_register_offsets() {
    assert_eq!(
        Aarch64Decoder::decode(0x3ca3_6800).unwrap().instruction,
        Aarch64Instruction::VectorStore {
            source: 0,
            bytes: 16,
            address: MemoryAddress::Register {
                base: 0,
                index: 3,
                extension: IndexExtension::Unsigned64,
                shift: 0,
            },
        }
    );
    for word in [
        0x3c22_4820,
        0x3c62_6820,
        0x7c22_5820,
        0x7c62_d820,
        0xbc22_d820,
        0xbc62_7820,
        0xfc22_f820,
        0xfc62_5820,
        0x3ca2_6820,
        0x3ce2_7820,
    ] {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
    }

    let mut memory = Memory::default();
    let mut cpu = Aarch64CpuState {
        pc: 0x7200,
        sp: 0x8000,
        ..Default::default()
    };
    cpu.set_register(3, 2);
    cpu.set_vector(0, 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x3ca3_7be0),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(memory.get(0x8020, 8), 0x99aa_bbcc_ddee_ff00);
    assert_eq!(memory.get(0x8028, 8), 0x1122_3344_5566_7788);

    memory.put(0x8000, 8, 0x99aa_bbcc_ddee_ff00);
    memory.put(0x8008, 8, 0x1122_3344_5566_7788);
    cpu.set_vector(1, u128::MAX);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x3cff_6be1),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(1), 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
}

#[test]
fn vector_offset_faults() {
    for option in [0_u32, 1, 4, 5] {
        let word = 0x3ca0_0800 | option << 13;
        assert_eq!(Aarch64Decoder::decode(word), Err(Aarch64DecodeError::Reserved));
    }
    // The 128-bit operation requires size == 0.
    assert_eq!(Aarch64Decoder::decode(0x7ca3_6800), Err(Aarch64DecodeError::Reserved));

    let mut memory = Memory {
        write_fault: Some(0x9008),
        ..Default::default()
    };
    let mut cpu = Aarch64CpuState {
        pc: 0x7300,
        ..Default::default()
    };
    cpu.set_register(0, 0x9000);
    cpu.set_register(3, 0);
    cpu.set_vector(0, u128::MAX);
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x3ca3_6800),
        Aarch64ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x7300,
                    address: 0x9008,
                    access: AccessKind::Write,
                },
                8
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!(cpu, before);
    assert!(memory.writes.is_empty());
}

#[test]
fn vector_unscaled_family() {
    for word in [
        0x3c1f_f020,
        0x3c40_1020,
        0x7c1f_e020,
        0x7c40_2020,
        0xbc1f_c020,
        0xbc40_4020,
        0xfc1f_8020,
        0xfc40_8020,
        0x3c9f_0020,
        0x3cc1_0020,
        0x3c9f_0fe0,
        0x3cc1_07e0,
    ] {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
    }
    assert_eq!(
        Aarch64Decoder::decode(0x3c9f_00a0).unwrap().instruction,
        Aarch64Instruction::VectorStore {
            source: 0,
            bytes: 16,
            address: MemoryAddress::Base {
                register: 5,
                displacement: -16,
                writeback: Writeback::None,
            },
        }
    );

    let mut memory = Memory::default();
    let mut cpu = Aarch64CpuState {
        pc: 0x7400,
        sp: 0xa010,
        ..Default::default()
    };
    cpu.set_vector(0, 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x3c9f_0fe0),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.sp, 0xa000);
    assert_eq!(memory.get(0xa000, 8), 0x99aa_bbcc_ddee_ff00);
    cpu.set_vector(0, 0);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x3cc1_07e0),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.sp, 0xa010);
    assert_eq!(cpu.vector(0), 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
}

#[test]
fn vector_writeback_fault() {
    let mut memory = Memory {
        read_fault: Some(0xa008),
        ..Default::default()
    };
    let mut cpu = Aarch64CpuState {
        pc: 0x7500,
        sp: 0xa000,
        ..Default::default()
    };
    cpu.set_vector(0, u128::MAX);
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x3cc1_07e0),
        Aarch64ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x7500,
                    address: 0xa008,
                    access: AccessKind::Read,
                },
                8
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!(cpu, before);
}

#[test]
fn structure_loads() {
    for word in [
        0x0c40_7020,
        0x0c40_a020,
        0x0c40_7420,
        0x0c40_a420,
        0x0c40_7820,
        0x0c40_a820,
        0x0c40_7c20,
        0x0c40_ac20,
        0x4c40_7020,
        0x4c40_a020,
        0x4c40_7420,
        0x4c40_a420,
        0x4c40_7820,
        0x4c40_a820,
        0x4c40_7c20,
        0x4c40_ac20,
    ] {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
    }
    assert_eq!(
        Aarch64Decoder::decode(0x4c40_a021).unwrap().instruction,
        Aarch64Instruction::VectorLoadPair {
            first: 1,
            second: 2,
            bytes: 16,
            address: MemoryAddress::Base {
                register: 1,
                displacement: 0,
                writeback: Writeback::None,
            },
        }
    );

    let mut memory = Memory::default();
    for (offset, value) in [
        0x99aa_bbcc_ddee_ff00,
        0x1122_3344_5566_7788,
        0x0123_4567_89ab_cdef,
        0xfedc_ba98_7654_3210,
    ]
    .into_iter()
    .enumerate()
    {
        memory.put(0x8003 + offset as u64 * 8, 8, value);
    }
    let mut cpu = Aarch64CpuState {
        pc: 0x7600,
        ..Default::default()
    };
    cpu.set_register(1, 0x8003);
    cpu.set_vector(0, u128::MAX);
    cpu.set_vector(1, u128::MAX);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x4c40_a020),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
    assert_eq!(cpu.vector(1), 0xfedc_ba98_7654_3210_0123_4567_89ab_cdef);

    cpu.sp = 0x8003;
    cpu.set_vector(31, u128::MAX);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x0c40_73ff),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(31), 0x99aa_bbcc_ddee_ff00);
}

#[test]
fn structure_faults() {
    let mut memory = Memory {
        read_fault: Some(0x9018),
        ..Default::default()
    };
    let mut cpu = Aarch64CpuState {
        pc: 0x7700,
        ..Default::default()
    };
    cpu.set_register(1, 0x9000);
    cpu.set_vector(31, u128::MAX);
    cpu.set_vector(0, u128::MAX);
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x4c40_a03f),
        Aarch64ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x7700,
                    address: 0x9018,
                    access: AccessKind::Read,
                },
                8
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!(cpu, before);

    memory.read_fault = Some(0x9008);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x4c40_7020),
        Aarch64ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x7700,
                    address: 0x9008,
                    access: AccessKind::Read,
                },
                8
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!(cpu, before);

    assert_eq!(
        Aarch64Decoder::decode(0x4c41_7020),
        Err(Aarch64DecodeError::Unsupported)
    );
}

#[test]
fn structure_post_index() {
    for word in [
        0x0cdf_7020,
        0x0cdf_a020,
        0x4cdf_7420,
        0x4cdf_a420,
        0x4cc2_7820,
        0x4cc2_a820,
        0x0cc3_7c20,
        0x0cc3_ac20,
    ] {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
    }
    assert_eq!(
        Aarch64Decoder::decode(0x4cdf_7041).unwrap().instruction,
        Aarch64Instruction::VectorLoad {
            destination: 1,
            bytes: 16,
            address: MemoryAddress::Base {
                register: 2,
                displacement: 16,
                writeback: Writeback::PostIndex,
            },
        }
    );
    assert_eq!(
        Aarch64Decoder::decode(0x4cdf_2041).unwrap().instruction,
        Aarch64Instruction::VectorLoadGroup {
            first: 1,
            count: 4,
            bytes: 16,
            address: MemoryAddress::Base {
                register: 2,
                displacement: 64,
                writeback: Writeback::PostIndex,
            },
        }
    );

    let mut memory = Memory::default();
    memory.put(0x8000, 8, 0x99aa_bbcc_ddee_ff00);
    memory.put(0x8008, 8, 0x1122_3344_5566_7788);
    let mut cpu = Aarch64CpuState {
        pc: 0x7800,
        ..Default::default()
    };
    cpu.set_register(2, 0x8000);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x4cdf_7041),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(1), 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
    assert_eq!(cpu.register(2), 0x8010);

    cpu.sp = 0x8000;
    cpu.set_register(3, 0x28);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x4cc3_73e2),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(2), 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
    assert_eq!(cpu.sp, 0x8028);

    for register in 0..4 {
        cpu.set_vector(
            register,
            u128::from(register + 1) * 0x1111_1111_1111_1111_1111_1111_1111_1111,
        );
    }
    cpu.set_register(1, 0x8100);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x4c9f_2020),
        Aarch64ExecutionExit::Continue
    );
    for chunk in 0..8 {
        assert_eq!(
            memory.get(0x8100 + chunk * 8, 8),
            (chunk / 2 + 1) * 0x1111_1111_1111_1111
        );
    }
    assert_eq!(cpu.register(1), 0x8140);
}

#[test]
fn prefetch_hints_are_non_faulting_nops() {
    for word in [0xf980_0000, 0xf880_0000, 0xf8a1_6800, 0xd800_0020] {
        assert_eq!(
            Aarch64Decoder::decode(word).unwrap().instruction,
            Aarch64Instruction::Nop
        );
    }
}

#[test]
fn structure_post_fault() {
    let mut memory = Memory {
        read_fault: Some(0x9018),
        ..Default::default()
    };
    let mut cpu = Aarch64CpuState {
        pc: 0x7900,
        ..Default::default()
    };
    cpu.set_register(1, 0x9000);
    cpu.set_vector(31, u128::MAX);
    cpu.set_vector(0, u128::MAX);
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x4cdf_203f),
        Aarch64ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x7900,
                    address: 0x9018,
                    access: AccessKind::Read,
                },
                8
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!(cpu, before);

    for (word, count) in [(0x4cdf_0020, 4), (0x4cdf_4020, 3), (0x4cdf_8020, 2)] {
        assert!(matches!(
            Aarch64Decoder::decode(word).unwrap().instruction,
            Aarch64Instruction::VectorStructureGroup { count: actual, .. } if actual == count
        ));
    }

    memory.read_fault = None;
    memory.write_fault = Some(0x9038);
    cpu.set_register(1, 0x9000);
    let before = cpu.clone();
    assert!(matches!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x4c9f_2020),
        Aarch64ExecutionExit::OperandFault(_)
    ));
    assert_eq!(cpu, before);
}

#[test]
fn structure_interleaving() {
    for (opcode, count) in [(0_u32, 4_u8), (4, 3), (8, 2)] {
        for (wide, sizes) in [(false, 0_u32..3), (true, 0_u32..4)] {
            for size in sizes {
                let prefix = if wide { 0x4c40_0000 } else { 0x0c40_0000 };
                let load_word = prefix | opcode << 12 | size << 10 | 1 << 5 | 30;
                let decoded = Aarch64Decoder::decode(load_word).unwrap().instruction;
                assert!(matches!(
                    decoded,
                    Aarch64Instruction::VectorStructureGroup {
                        first: 30,
                        count: actual,
                        lane_bits,
                        load: true,
                        wide: actual_wide,
                        ..
                    } if actual == count && lane_bits == 8 << size && actual_wide == wide
                ));

                let lane_bits = 8_u8 << size;
                let bytes = lane_bits / 8;
                let lanes = (if wide { 128 } else { 64 }) / lane_bits;
                let mut memory = Memory::default();
                for lane in 0..lanes {
                    for register in 0..count {
                        let slot = u64::from(lane * count + register);
                        memory.put(0x8000 + slot * u64::from(bytes), bytes, slot + 1);
                    }
                }
                let mut cpu = Aarch64CpuState {
                    pc: 0x7000,
                    ..Default::default()
                };
                cpu.set_register(1, 0x8000);
                for register in 0..count {
                    cpu.set_vector(30_u8.wrapping_add(register) & 31, u128::MAX);
                }
                assert_eq!(
                    cpu.execute_memory(&mut memory, &IDENTITY, load_word),
                    Aarch64ExecutionExit::Continue
                );
                for register in 0..count {
                    let expected = (0..lanes).fold(0_u128, |value, lane| {
                        let slot = u128::from(lane * count + register + 1);
                        value | slot << (u32::from(lane) * u32::from(lane_bits))
                    });
                    assert_eq!(cpu.vector(30_u8.wrapping_add(register) & 31), expected);
                }

                let store_word = load_word & !(1 << 22);
                cpu.set_register(1, 0x9000);
                assert_eq!(
                    cpu.execute_memory(&mut memory, &IDENTITY, store_word),
                    Aarch64ExecutionExit::Continue
                );
                for slot in 0..u64::from(lanes * count) {
                    assert_eq!(memory.get(0x9000 + slot * u64::from(bytes), bytes), slot + 1);
                }
            }
        }
    }

    assert_eq!(Aarch64Decoder::decode(0x0c40_0c20), Err(Aarch64DecodeError::Reserved));

    let mut memory = Memory {
        read_fault: Some(0xa006),
        ..Default::default()
    };
    let mut cpu = Aarch64CpuState {
        pc: 0x7100,
        ..Default::default()
    };
    cpu.set_register(1, 0xa000);
    cpu.set_vector(0, u128::MAX);
    let before = cpu.clone();
    assert!(matches!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x4cdf_4420),
        Aarch64ExecutionExit::OperandFault(fault)
            if fault.fault().address == 0xa006 && fault.fault().access == AccessKind::Read
    ));
    assert_eq!(cpu, before);

    memory.read_fault = None;
    memory.write_fault = Some(0xa006);
    let before_memory = memory.bytes.clone();
    assert!(matches!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x4c9f_4420),
        Aarch64ExecutionExit::OperandFault(_)
    ));
    assert_eq!(cpu, before);
    assert_eq!(memory.bytes, before_memory);
}

#[test]
fn structure_lanes() {
    for (word, count, bits, lane, load, replicate, wide) in [
        (0x0d40_02bf, 1, 8, 0, true, false, false),
        (0x4d40_1ebf, 1, 8, 15, true, false, true),
        (0x4d40_5abf, 1, 16, 7, true, false, true),
        (0x4d60_82bf, 2, 32, 2, true, false, true),
        (0x4d40_6abe, 3, 16, 5, true, false, true),
        (0x4d20_36bd, 4, 8, 13, false, false, true),
        (0x4d40_c2bf, 1, 8, 0, true, true, true),
        (0x0d60_cabf, 2, 32, 0, true, true, false),
        (0x4d60_eebd, 4, 64, 0, true, true, true),
    ] {
        let instruction = Aarch64Decoder::decode(word).unwrap().instruction;
        assert!(matches!(instruction, Aarch64Instruction::VectorStructureLane {
            count: got_count, lane_bits, lane: got_lane, load: got_load,
            replicate: got_replicate, wide: got_wide, .. } if got_count == count
                && lane_bits == bits && got_lane == lane && got_load == load
                && got_replicate == replicate && got_wide == wide));
    }

    let mut memory = Memory::default();
    memory.put(0x8003, 8, 0x8877_6655_4433_2211);
    let mut cpu = Aarch64CpuState {
        pc: 0x7a00,
        ..Default::default()
    };
    cpu.set_register(21, 0x8003);
    cpu.set_vector(31, u128::MAX);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x0d40_06bf),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector_lane(31, 16, 0), 0x11ff);
    assert_eq!(cpu.vector(31) >> 16, u128::MAX >> 16);
    cpu.pc = 0;
    cpu.set_vector(31, 0x0008_0007_0006_0005_0004_0003_0002_0001);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x0d00_5abf),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(memory.get(0x8003, 2), 4);
    cpu.pc = 0;
    cpu.set_register(21, 0x8003);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x4d40_c2bf),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vector(31),
        u128::from(4_u8) * 0x0101_0101_0101_0101_0101_0101_0101_0101
    );
}

#[test]
fn structure_lane_faults() {
    let mut memory = Memory {
        read_fault: Some(0x9001),
        write_fault: Some(0xa001),
        ..Default::default()
    };
    let mut cpu = Aarch64CpuState {
        pc: 0x7b00,
        ..Default::default()
    };
    cpu.set_register(21, 0x9000);
    cpu.set_vector(31, u128::MAX);
    cpu.set_vector(0, u128::MAX);
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x4d60_82bf),
        Aarch64ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x7b00,
                    address: 0x9000,
                    access: AccessKind::Read
                },
                4
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!(cpu, before);
    cpu.set_register(21, 0xa000);
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x4d20_36bf),
        Aarch64ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x7b00,
                    address: 0xa001,
                    access: AccessKind::Write
                },
                1
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!(cpu, before);
    assert!(memory.writes.is_empty());
}

#[test]
fn structure_lane_post() {
    let mut memory = Memory::default();
    memory.put(0x8101, 2, 0xbeef);
    let mut cpu = Aarch64CpuState {
        pc: 0x7c00,
        ..Default::default()
    };
    cpu.set_register(21, 0x8101);
    cpu.set_register(7, 0x20);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x0dc7_5abf),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector_lane(31, 16, 3), 0xbeef);
    assert_eq!(cpu.register(21), 0x8121);
    cpu.pc = 0;
    cpu.set_register(21, 0x8101);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x0ddf_02bf),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(21), 0x8102);
}

#[test]
fn vector_reserved_faults() {
    // opc<1> selects the 128-bit form only when size is zero.
    assert_eq!(Aarch64Decoder::decode(0x7d80_0000), Err(Aarch64DecodeError::Reserved));

    let mut memory = Memory {
        write_fault: Some(0xa008),
        ..Default::default()
    };
    let mut cpu = Aarch64CpuState {
        pc: 0x8000,
        ..Default::default()
    };
    cpu.set_register(0, 0xa000);
    cpu.set_vector(0, u128::MAX);
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x3d80_0000),
        Aarch64ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x8000,
                    address: 0xa008,
                    access: AccessKind::Write,
                },
                8
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!(cpu, before);
    assert!(memory.writes.is_empty());

    memory.write_fault = None;
    memory.read_fault = Some(0xa008);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x3dc0_0001),
        Aarch64ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x8000,
                    address: 0xa008,
                    access: AccessKind::Read,
                },
                8
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!(cpu, before);
}

#[test]
fn vector_pair_family() {
    for word in [
        0x2d3e_0440,
        0x2cc1_13e3,
        0x6d81_18e5,
        0x6d42_2548,
        0xadbf_2d8a,
        0xacc2_37ec,
    ] {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
    }
    assert_eq!(Aarch64Decoder::decode(0xed00_0000), Err(Aarch64DecodeError::Reserved));

    let mut memory = Memory::default();
    let mut cpu = Aarch64CpuState {
        pc: 0x9000,
        sp: 0xb000,
        ..Default::default()
    };
    cpu.set_register(12, 0xa020);
    cpu.set_vector(10, 0x1111_2222_3333_4444_5555_6666_7777_8888);
    cpu.set_vector(11, 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0000);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xadbf_2d8a),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(12), 0xa000);
    assert_eq!(memory.get(0xa000, 8), 0x5555_6666_7777_8888);
    assert_eq!(memory.get(0xa018, 8), 0x9999_aaaa_bbbb_cccc);

    cpu.set_vector(12, 0);
    cpu.set_vector(13, 0);
    cpu.sp = 0xa000;
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xacc2_37ec),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(12), 0x1111_2222_3333_4444_5555_6666_7777_8888);
    assert_eq!(cpu.vector(13), 0x9999_aaaa_bbbb_cccc_dddd_eeee_ffff_0000);
    assert_eq!(cpu.sp, 0xa040);
    assert_eq!(cpu.pc, 0x9008);
}

#[test]
fn vector_pair_fault() {
    let mut memory = Memory {
        write_fault: Some(0xc038),
        ..Default::default()
    };
    let mut cpu = Aarch64CpuState {
        pc: 0xa000,
        ..Default::default()
    };
    cpu.set_register(3, 0xc000);
    cpu.set_vector(0, u128::MAX);
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xad01_0060),
        Aarch64ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0xa000,
                    address: 0xc038,
                    access: AccessKind::Write,
                },
                8
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!(cpu, before);
    assert!(memory.writes.is_empty());
}

#[test]
fn cache_zero_block() {
    let mut memory = Memory::default();
    for offset in (0..64).step_by(8) {
        memory.put(0xd000 + offset, 8, u64::MAX);
    }
    let mut cpu = Aarch64CpuState {
        pc: 0xb000,
        ..Default::default()
    };
    cpu.set_register(3, 0xd03f);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xd50b_7423),
        Aarch64ExecutionExit::Continue
    );
    for offset in (0..64).step_by(8) {
        assert_eq!(memory.get(0xd000 + offset, 8), 0);
    }
    assert_eq!(cpu.register(3), 0xd03f);
    assert_eq!(cpu.pc, 0xb004);
}

#[test]
fn cache_zero_fault() {
    let mut memory = Memory {
        write_fault: Some(0xe027),
        ..Default::default()
    };
    for offset in (0..64).step_by(8) {
        memory.put(0xe000 + offset, 8, u64::MAX);
    }
    let mut cpu = Aarch64CpuState {
        pc: 0xc000,
        ..Default::default()
    };
    cpu.set_register(4, 0xe021);
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xd50b_7424),
        Aarch64ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0xc000,
                    address: 0xe020,
                    access: AccessKind::Write,
                },
                8
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!(cpu, before);
    assert!(memory.writes.is_empty());
    assert_eq!(memory.get(0xe000, 8), u64::MAX);

    assert_eq!(
        Aarch64Decoder::decode(0xd50b_7400),
        Err(Aarch64DecodeError::Unsupported)
    );
}

#[test]
fn unscaled_pre_post() {
    let mut memory = Memory::default();
    let mut cpu = Aarch64CpuState {
        pc: 0x2000,
        sp: 0x3000,
        ..Default::default()
    };
    cpu.set_register(1, 0x3010);
    cpu.set_register(0, 0xaabb_ccdd_eeff_0011);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xf81f_8020),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(memory.get(0x3008, 8), 0xaabb_ccdd_eeff_0011);

    cpu.set_register(5, 0x4000);
    cpu.set_register(4, 0x55);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xf801_0ca4),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(memory.get(0x4010, 8), 0x55);
    assert_eq!(cpu.register(5), 0x4010);

    memory.put(0x5000, 8, 0x1234);
    cpu.set_register(7, 0x5000);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xf85f_04e6),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(6), 0x1234);
    assert_eq!(cpu.register(7), 0x4ff0);

    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xf900_03ff),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(memory.get(cpu.sp, 8), 0);
    memory.put(cpu.sp, 8, u64::MAX);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xf940_03ff),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(31), 0);
}

#[test]
fn register_offsets_cover() {
    let mut memory = Memory::default();
    let mut cpu = Aarch64CpuState {
        pc: 0x3000,
        ..Default::default()
    };
    cpu.set_register(9, 0x6000);
    cpu.set_register(10, 2);
    memory.put(0x6010, 8, 0xfeed_face_cafe_beef);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xf86a_5928),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(8), 0xfeed_face_cafe_beef);

    cpu.set_register(12, 4);
    cpu.set_register(13, u64::from(u32::MAX));
    cpu.set_register(11, 0x1122_3344);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xb82d_d98b),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(memory.get(0, 4), 0x1122_3344);
}

#[test]
fn literal_loads_use() {
    let coordinates = Coordinates {
        low: 0x400000,
        high: 0x7000_0000_0000,
        size: 0x10000,
    };
    let mut memory = Memory::default();
    let mut cpu = Aarch64CpuState {
        pc: coordinates.high + 0x54,
        ..Default::default()
    };
    memory.put(coordinates.low, 8, 0x8877_6655_4433_2211);
    assert_eq!(
        cpu.execute_memory(&mut memory, &coordinates, 0x58ff_fd74),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(20), 0x8877_6655_4433_2211);

    cpu.pc = coordinates.high + 0x58;
    memory.put(coordinates.low, 4, 0x8000_0001);
    assert_eq!(
        cpu.execute_memory(&mut memory, &coordinates, 0x98ff_fd55),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(21), 0xffff_ffff_8000_0001);

    cpu.pc = coordinates.high + 0x5c;
    memory.put(coordinates.low, 4, 0x7fc0_1234);
    assert_eq!(
        cpu.execute_memory(&mut memory, &coordinates, 0x1cff_fd20),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), 0x7fc0_1234);

    cpu.pc = coordinates.high + 0x7710;
    memory.put(coordinates.low + 0x76e0, 4, 0x3f80_0000);
    assert_eq!(
        cpu.execute_memory(&mut memory, &coordinates, 0x1cff_fe80),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.vector(0), 0x3f80_0000);
}

#[test]
fn ldapr_widths_and_acquire_order() {
    for (word, width) in [
        (0x38bf_c020, crate::MemoryWidth::Byte),
        (0x78bf_c020, crate::MemoryWidth::Half),
        (0xb8bf_c020, crate::MemoryWidth::Word),
        (0xf8bf_c020, crate::MemoryWidth::Double),
    ] {
        assert_eq!(
            Aarch64Decoder::decode(word).map(|ir| ir.instruction),
            Ok(Aarch64Instruction::OrderedAccess {
                load: true,
                base: 1,
                transfer: 0,
                width,
                order: crate::MemoryOrder::Acquire,
            })
        );
    }
}

#[test]
fn pair_stack_forms() {
    let mut memory = Memory::default();
    let mut cpu = Aarch64CpuState {
        pc: 0x4000,
        sp: 0x8000,
        ..Default::default()
    };
    cpu.set_register(0, 0x1111);
    cpu.set_register(1, 0x2222);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xa9bf_07e0),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(memory.get(0x7ff0, 8), 0x1111);
    assert_eq!(memory.get(0x7ff8, 8), 0x2222);
    assert_eq!(cpu.sp, 0x7ff0);

    memory.put(0x7ff0, 8, 0xaaaa);
    memory.put(0x7ff8, 8, 0xbbbb);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xa8c1_0fe2),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!((cpu.register(2), cpu.register(3), cpu.sp), (0xaaaa, 0xbbbb, 0x8000));

    memory.put(0x9000, 8, 0x1234);
    cpu.set_register(5, 0x9000);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xf840_84a5),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(5), 0x9008);

    memory.put(0xa000, 8, 0x1111);
    memory.put(0xa008, 8, 0x2222);
    cpu.set_register(1, 0xa000);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xa8c1_0821),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(1), 0xa010);
    assert_eq!(cpu.register(2), 0x2222);

    memory.put(0xb008, 4, 0x8000_0001);
    memory.put(0xb00c, 4, 0x7fff_ffff);
    cpu.set_register(12, 0xb000);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0x6941_2d8a),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(10), 0xffff_ffff_8000_0001);
    assert_eq!(cpu.register(11), 0x7fff_ffff);
}

#[test]
fn unaligned_scalar_accesses() {
    let mut memory = Memory::default();
    let mut cpu = Aarch64CpuState {
        pc: 0x5000,
        ..Default::default()
    };
    cpu.set_register(19, 0x9001);
    memory.put(0x9019, 4, 0xdead_beef);
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xb940_1a72),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(18), 0xdead_beef);

    cpu.pc = 0x5100;
    cpu.set_register(19, 0x9001);
    memory.read_fault = Some(0x901a);
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xb940_1a72),
        Aarch64ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x5100,
                    address: 0x9019,
                    access: AccessKind::Read,
                },
                4
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!(cpu, before);
}

#[test]
fn pair_faults_leave() {
    let mut memory = Memory::default();
    let mut cpu = Aarch64CpuState {
        pc: 0x6000,
        sp: 0xa000,
        ..Default::default()
    };
    memory.put(0xa000, 8, 1);
    memory.put(0xa008, 8, 2);
    memory.read_fault = Some(0xa008);
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xa8c1_0fe2),
        Aarch64ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x6000,
                    address: 0xa008,
                    access: AccessKind::Read,
                },
                8
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!(cpu, before);

    memory.read_fault = None;
    memory.write_fault = Some(0x9ff8);
    cpu.set_register(0, 0x11);
    cpu.set_register(1, 0x22);
    let before_store = cpu.clone();
    assert_eq!(
        cpu.execute_memory(&mut memory, &IDENTITY, 0xa9bf_07e0),
        Aarch64ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x6000,
                    address: 0x9ff8,
                    access: AccessKind::Write,
                },
                8
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!(memory.get(0x9ff0, 8), 0);
    assert_eq!(cpu, before_store);
}

#[test]
fn reserved_memory_encodings() {
    let reserved = [0xc800_0000, 0x6800_0000, 0x38a0_0800, 0x38a0_2800, 0xb8c0_0000];
    let mut memory = Memory::default();
    let mut cpu = Aarch64CpuState {
        pc: 0x7000,
        ..Default::default()
    };
    for word in reserved {
        let decoded = Aarch64Decoder::decode(word);
        if decoded == Err(Aarch64DecodeError::Reserved) {
            let before = cpu.clone();
            assert_eq!(
                cpu.execute_memory(&mut memory, &IDENTITY, word),
                Aarch64ExecutionExit::UndefinedInstruction {
                    instruction: 0x7000,
                    word,
                }
            );
            assert_eq!(cpu, before);
        }
    }

    for option in 0_u32..8 {
        let word = (0xf860_6800 & !(7 << 13)) | option << 13;
        let valid = matches!(option, 2 | 3 | 6 | 7);
        assert_eq!(Aarch64Decoder::decode(word).is_ok(), valid, "{word:#010x}");
    }
    for operation in 0_u32..4 {
        let store = 0x2800_0000 | operation << 30;
        let load = store | 1 << 22;
        assert_eq!(
            Aarch64Decoder::decode(store).is_ok(),
            matches!(operation, 0 | 2),
            "{store:#010x}"
        );
        assert_eq!(Aarch64Decoder::decode(load).is_ok(), operation != 3, "{load:#010x}");
    }
}
