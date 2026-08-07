#![allow(clippy::field_reassign_with_default, clippy::comparison_chain)]

use crate::x86::{VexImmediateShift, VexOperation};
use crate::*;

#[test]
fn vex_f16c_decode_and_roundtrip() {
    let narrow = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x79, 0x1d, 0xc0, 0x00], 0x1000).unwrap();
    assert!(matches!(
        narrow.instruction,
        ScalarInstruction::VexHalfNarrow {
            wide: false,
            control: 0,
            ..
        }
    ));
    let widen = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x79, 0x13, 0xc0], 0x1006).unwrap();
    assert!(matches!(
        widen.instruction,
        ScalarInstruction::VexHalfWiden { wide: false, .. }
    ));

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = u128::from(1.5_f32.to_bits())
        | (u128::from(2.5_f32.to_bits()) << 32)
        | (u128::from(3.5_f32.to_bits()) << 64)
        | (u128::from(4.5_f32.to_bits()) << 96);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, narrow),
        ExecutionExit::Continue
    );
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, widen),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[0],
        u128::from(1.5_f32.to_bits())
            | (u128::from(2.5_f32.to_bits()) << 32)
            | (u128::from(3.5_f32.to_bits()) << 64)
            | (u128::from(4.5_f32.to_bits()) << 96)
    );
}

struct ModelMemory {
    base: u64,
    bytes: Vec<u8>,
    fail_read: bool,
    fail_write: bool,
    commits: usize,
}

#[derive(Clone)]
struct LockedMemory {
    base: u64,
    bytes: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    atomics: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    writes: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    fail: bool,
}

impl LockedMemory {
    fn new(base: u64, length: usize) -> Self {
        Self {
            base,
            bytes: std::sync::Arc::new(std::sync::Mutex::new(vec![0; length])),
            atomics: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            writes: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
            fail: false,
        }
    }

    fn offset(&self, address: u64, bytes: u8) -> Result<usize, ()> {
        let offset = usize::try_from(address.checked_sub(self.base).ok_or(())?).map_err(|_| ())?;
        (offset
            .checked_add(usize::from(bytes))
            .is_some_and(|end| end <= self.bytes.lock().unwrap().len()))
        .then_some(offset)
        .ok_or(())
    }

    fn value(&self, address: u64, bytes: u8) -> Result<u64, ()> {
        let offset = self.offset(address, bytes)?;
        let storage = self.bytes.lock().unwrap();
        Ok(storage[offset..offset + usize::from(bytes)]
            .iter()
            .enumerate()
            .fold(0, |value, (shift, byte)| value | (u64::from(*byte) << (shift * 8))))
    }

    fn replace(&self, address: u64, bytes: u8, value: u64) -> Result<(), ()> {
        let offset = self.offset(address, bytes)?;
        let mut storage = self.bytes.lock().unwrap();
        for index in 0..usize::from(bytes) {
            storage[offset + index] = (value >> (index * 8)) as u8;
        }
        Ok(())
    }
}

impl GuestOperandMemory for LockedMemory {
    type Reservation = (u64, u8);
    type BatchReservation = Vec<(u64, u8)>;

    fn read(&self, address: u64, bytes: u8) -> Result<u64, ()> {
        self.value(address, bytes)
    }

    fn reserve_write(&self, address: u64, bytes: u8) -> Result<Self::Reservation, ()> {
        self.offset(address, bytes)?;
        Ok((address, bytes))
    }

    fn commit_write(&mut self, reservation: Self::Reservation, value: u64) -> Result<(), ()> {
        self.writes.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.replace(reservation.0, reservation.1, value)
    }

    fn reserve_write_batch(&self, writes: &[(u64, u8)]) -> Result<Self::BatchReservation, u64> {
        writes
            .iter()
            .map(|(address, bytes)| self.reserve_write(*address, *bytes).map_err(|()| *address))
            .collect()
    }

    fn commit_write_batch(&mut self, reservations: Self::BatchReservation, values: &[u64]) -> Result<(), ()> {
        for (reservation, value) in reservations.into_iter().zip(values) {
            self.commit_write(reservation, *value)?;
        }
        Ok(())
    }
}

impl ExclusiveMemory for LockedMemory {
    fn load_ordered(&mut self, address: u64, bytes: u8, _order: MemoryOrder) -> Result<u64, ()> {
        self.value(address, bytes)
    }

    fn store_ordered(&mut self, address: u64, bytes: u8, value: u64, _order: MemoryOrder) -> Result<(), ()> {
        self.replace(address, bytes, value)
    }

    fn load_exclusive(
        &mut self,
        address: u64,
        element_bytes: u8,
        pair: bool,
        _order: MemoryOrder,
    ) -> Result<ExclusiveLoad, ()> {
        let value = AtomicValue {
            low: self.value(address, element_bytes)?,
            high: if pair {
                self.value(address + u64::from(element_bytes), element_bytes)?
            } else {
                0
            },
        };
        Ok(ExclusiveLoad {
            value,
            reservation: ExclusiveReservation::new(address, element_bytes, pair, MappingGeneration::new(0)),
        })
    }

    fn store_exclusive(
        &mut self,
        reservation: ExclusiveReservation,
        replacement: AtomicValue,
        order: MemoryOrder,
    ) -> Result<bool, ()> {
        self.store_ordered(
            reservation.address(),
            reservation.element_bytes(),
            replacement.low,
            order,
        )?;
        if reservation.pair() {
            self.store_ordered(
                reservation.address() + u64::from(reservation.element_bytes()),
                reservation.element_bytes(),
                replacement.high,
                order,
            )?;
        }
        Ok(true)
    }

    fn compare_exchange(
        &mut self,
        address: u64,
        element_bytes: u8,
        pair: bool,
        expected: AtomicValue,
        replacement: AtomicValue,
        order: MemoryOrder,
    ) -> Result<AtomicValue, ()> {
        let loaded = self.load_exclusive(address, element_bytes, pair, order)?;
        if loaded.value == expected {
            self.store_exclusive(loaded.reservation, replacement, order)?;
        }
        Ok(loaded.value)
    }

    fn fetch_update(
        &mut self,
        address: u64,
        bytes: u8,
        operation: AtomicOperation,
        operand: u64,
        _order: MemoryOrder,
    ) -> Result<u64, ()> {
        self.atomics.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if self.fail {
            return Err(());
        }
        let offset = self.offset(address, bytes)?;
        let mut storage = self.bytes.lock().unwrap();
        let observed = storage[offset..offset + usize::from(bytes)]
            .iter()
            .enumerate()
            .fold(0_u64, |value, (shift, byte)| value | (u64::from(*byte) << (shift * 8)));
        let replacement = match operation {
            AtomicOperation::Swap => operand,
            AtomicOperation::Add => observed.wrapping_add(operand),
            AtomicOperation::Clear => observed & !operand,
            AtomicOperation::ExclusiveOr => observed ^ operand,
            AtomicOperation::Set => observed | operand,
            AtomicOperation::SignedMaximum => observed.max(operand),
            AtomicOperation::SignedMinimum => observed.min(operand),
            AtomicOperation::UnsignedMaximum => observed.max(operand),
            AtomicOperation::UnsignedMinimum => observed.min(operand),
        };
        for index in 0..usize::from(bytes) {
            storage[offset + index] = (replacement >> (index * 8)) as u8;
        }
        Ok(observed)
    }
}

#[test]
fn endbr64_is_noop() {
    let ir = X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x1e, 0xfa], 0x401000).unwrap();
    assert_eq!(ir.length, 4);
    assert_eq!(ir.instruction, ScalarInstruction::Nop);

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x401000,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.rip, 0x401004);
}

#[test]
fn retained_hints() {
    for bytes in [
        &[0xf3, 0x48, 0x0f, 0x1e, 0xc8][..],
        &[0x0f, 0x18, 0x03][..],
        &[0xf2, 0x0f, 0x1c, 0xc0][..],
        &[0x0f, 0x0d, 0x00][..],
        &[0x0f, 0x1f, 0x40, 0x00][..],
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x800,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[3] = u64::MAX;
        let original = cpu.clone();
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: true,
            fail_write: true,
            commits: 0,
        };
        let instruction = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers, original.registers);
        assert_eq!(cpu.flags, original.flags);
        assert_eq!(cpu.rip, original.rip + u64::from(instruction.length));
        assert_eq!(memory.commits, 0);
    }
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x0f, 0x1e, 0xc8], 0).is_err());
}

#[test]
fn byte_swap_registers() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: true,
        fail_write: true,
        commits: 0,
    };
    let mut dword = CpuState {
        scalar: ScalarState {
            rip: 0x401757,
            flags: FlagState::from_bits(0x8d5),
            ..Default::default()
        },
        ..Default::default()
    };
    dword.registers[1] = 0xffff_ffff_89ab_cdef;
    let dword_flags = dword.flags;
    let decoded = X86ScalarDecoder::decode(&[0x0f, 0xc9], dword.rip).unwrap();
    assert_eq!(decoded.width, ScalarWidth::Dword);
    assert_eq!(
        decoded.instruction,
        ScalarInstruction::ByteSwap {
            register: ScalarRegister::General(1)
        }
    );
    assert_eq!(
        ScalarInterpreter::execute(&mut dword, &mut memory, decoded),
        ExecutionExit::Continue
    );
    assert_eq!(dword.registers[1], 0xefcd_ab89);
    assert_eq!(dword.flags, dword_flags);

    let mut qword = CpuState {
        scalar: ScalarState {
            rip: 0x500,
            ..Default::default()
        },
        ..Default::default()
    };
    qword.registers[6] = 0x0011_2233_4455_6677;
    let decoded = X86ScalarDecoder::decode(&[0x48, 0x0f, 0xce], qword.rip).unwrap();
    assert_eq!(decoded.width, ScalarWidth::Qword);
    assert_eq!(
        ScalarInterpreter::execute(&mut qword, &mut memory, decoded),
        ExecutionExit::Continue
    );
    assert_eq!(qword.registers[6], 0x7766_5544_3322_1100);

    let mut extended = CpuState {
        scalar: ScalarState {
            rip: 0x600,
            ..Default::default()
        },
        ..Default::default()
    };
    extended.registers[9] = 0x0123_4567_89ab_cdef;
    let decoded = X86ScalarDecoder::decode(&[0x49, 0x0f, 0xc9], extended.rip).unwrap();
    assert_eq!(
        decoded.instruction,
        ScalarInstruction::ByteSwap {
            register: ScalarRegister::General(9)
        }
    );
    assert_eq!(
        ScalarInterpreter::execute(&mut extended, &mut memory, decoded),
        ExecutionExit::Continue
    );
    assert_eq!(extended.registers[9], 0xefcd_ab89_6745_2301);
    assert_eq!(memory.commits, 0);
}

#[test]
fn byte_swap_prefixes() {
    for bytes in [
        &[0xf0, 0x0f, 0xc8][..],
        &[0xf2, 0x0f, 0xc8][..],
        &[0xf3, 0x0f, 0xc8][..],
        &[0x64, 0x0f, 0xc8][..],
    ] {
        assert_eq!(X86ScalarDecoder::decode(bytes, 0), Err(ScalarIrError::Invalid));
    }
    assert_eq!(
        X86ScalarDecoder::decode(&[0x66, 0x0f, 0xc8], 0).unwrap().width,
        ScalarWidth::Dword
    );
}

#[test]
fn setne_rex_byte() {
    let ir = X86ScalarDecoder::decode(&[0x40, 0x0f, 0x95, 0xc7], 0x401025).unwrap();
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x401025,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[7] = 0x1122;
    cpu.flags = FlagState::from_bits(0);
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[7], 0x1101);
}

#[test]
fn movsxd_extends_operands() {
    let register = X86ScalarDecoder::decode(&[0x48, 0x63, 0xc6], 0x404265).unwrap();
    assert_eq!(register.width, ScalarWidth::Qword);
    assert_eq!(
        register.instruction,
        ScalarInstruction::MoveSignExtend {
            destination: ScalarRegister::General(0),
            source: ScalarOperand::Register(ScalarRegister::General(6)),
            source_width: ScalarWidth::Dword,
        }
    );
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x404265,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[6] = 0xfeed_face_8000_0001;
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: (-2_i32).to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, register),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 0xffff_ffff_8000_0001);

    cpu.rip = 0x500;
    cpu.registers[3] = 0x1000;
    let from_memory = X86ScalarDecoder::decode(&[0x4c, 0x63, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, from_memory),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[8], u64::MAX - 1);
}

#[test]
fn movzx_extends_operands() {
    let word = X86ScalarDecoder::decode(&[0x0f, 0xb7, 0x95, 0xf0, 0xfe, 0xff, 0xff], 0x41c029).unwrap();
    assert_eq!(word.length, 7);
    assert_eq!(word.width, ScalarWidth::Dword);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x41c029,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[5] = 0x1110;
    cpu.registers[2] = u64::MAX;
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0x34, 0xf2],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, word),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.registers[2], cpu.rip), (0xf234, 0x41c030));

    cpu.rip = 0x500;
    cpu.registers[0] = 0x1122_3344_5566_abcd;
    let high = X86ScalarDecoder::decode(&[0x0f, 0xb6, 0xc4], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, high),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 0xab);

    cpu.rip = 0x600;
    cpu.registers[4] = 0x88;
    cpu.registers[8] = u64::MAX;
    let rex = X86ScalarDecoder::decode(&[0x44, 0x0f, 0xb6, 0xc4], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, rex),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.registers[8], cpu.rip), (0x88, 0x604));

    cpu.rip = 0x700;
    cpu.registers[0] = u64::MAX;
    cpu.registers[6] = 0x1234_5678_9abc_def0;
    let qword = X86ScalarDecoder::decode(&[0x48, 0x0f, 0xb7, 0xc6], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, qword),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 0xdef0);

    cpu.rip = 0x800;
    cpu.registers[0] = 0x1122_3344_5566_7788;
    let word_destination = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xb6, 0xc4], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, word_destination),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 0x1122_3344_5566_0077);
}

#[test]
fn movzx_fault_atomic() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x700,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.registers[0] = 0xfeed;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 2],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let ir = X86ScalarDecoder::decode(&[0x48, 0x0f, 0xb7, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
        ExecutionExit::OperandFault(access) if matches!(access.fault(), MemoryFault {
                instruction: 0x700,
                address: 0x1000,
                access: AccessKind::Read,
            }) && access.length() == 2
    ));
    assert_eq!(cpu, original);
}

#[test]
fn movsx_byte_values() {
    let instruction = X86ScalarDecoder::decode(&[0x0f, 0xbe, 0xc3], 0x900).unwrap();
    for byte in 0_u64..=255 {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x900,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[0] = u64::MAX;
        cpu.registers[3] = byte;
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[0], (byte as u8 as i8 as i32) as u32 as u64);
    }
}

#[test]
fn movsx_byte_registers() {
    for (bytes, source, destination, expected) in [
        (&[0x0f, 0xbe, 0xc4][..], 0x80_u64 << 8, 0_usize, 0xffff_ff80_u64),
        (&[0x40, 0x0f, 0xbe, 0xc4][..], 0x81, 0, 0xffff_ff81),
        (&[0x41, 0x0f, 0xbe, 0xc4][..], 0x82, 0, 0xffff_ff82),
        (&[0x44, 0x0f, 0xbe, 0xc4][..], 0x83, 8, 0xffff_ff83),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0xa00,
                ..Default::default()
            },
            ..Default::default()
        };
        if bytes[0] == 0x41 {
            cpu.registers[12] = source;
        } else {
            cpu.registers[4] = source;
        }
        if bytes[0] == 0x0f {
            cpu.registers[0] = source;
        }
        let instruction = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[destination], expected);
    }

    for raw in 4_u8..=7 {
        let byte = 0x80 + raw;
        let mut legacy = CpuState {
            scalar: ScalarState {
                rip: 0xa80,
                ..Default::default()
            },
            ..Default::default()
        };
        legacy.registers[usize::from(raw - 4)] = u64::from(byte) << 8;
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let high = X86ScalarDecoder::decode(&[0x0f, 0xbe, 0xc0 | raw], legacy.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut legacy, &mut memory, high),
            ExecutionExit::Continue
        );
        assert_eq!(legacy.registers[0], (byte as i8 as i32) as u32 as u64);

        for (rex, source) in [(0x40_u8, raw), (0x41, raw + 8)] {
            let byte = byte + 8;
            let mut rex_cpu = CpuState {
                scalar: ScalarState {
                    rip: 0xa90,
                    ..Default::default()
                },
                ..Default::default()
            };
            rex_cpu.registers[usize::from(source)] = u64::from(byte);
            let low = X86ScalarDecoder::decode(&[rex, 0x0f, 0xbe, 0xc0 | raw], rex_cpu.rip).unwrap();
            assert_eq!(
                ScalarInterpreter::execute(&mut rex_cpu, &mut memory, low),
                ExecutionExit::Continue
            );
            assert_eq!(rex_cpu.registers[0], (byte as i8 as i32) as u32 as u64);
        }
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0xb00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[0] = 0x80_55;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let alias = X86ScalarDecoder::decode(&[0x0f, 0xbe, 0xc4], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, alias),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 0xffff_ff80);
}

#[test]
fn movsx_byte_memory() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0xc00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[11] = 0x1000;
    cpu.registers[8] = 0xaaaa_aaaa_aaaa_aaaa;
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: vec![0x80],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let qword = X86ScalarDecoder::decode(&[0x4d, 0x0f, 0xbe, 0x43, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, qword),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[8], 0xffff_ffff_ffff_ff80);

    cpu.rip = 0xd00;
    cpu.registers[8] = 0x1122_3344_5566_7788;
    let word = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0xbe, 0x43, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, word),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[8], 0x1122_3344_5566_ff80);

    cpu.rip = 0xe00;
    let original = cpu.clone();
    memory.fail_read = true;
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, qword),
        ExecutionExit::OperandFault(access) if access.length() == 1
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x0f, 0xbe, 0x00], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x0f, 0xbe, 0xc0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0xbe, 0xc0], 0).is_err());
}

#[test]
fn movsx_word_forms() {
    for (bytes, source, initial, expected) in [
        (
            &[0x66, 0x0f, 0xbf, 0xc1][..],
            0x8001_u64,
            0x1122_3344_5566_7788,
            0x1122_3344_5566_8001,
        ),
        (&[0x0f, 0xbf, 0xc1][..], 0x8001, u64::MAX, 0xffff_8001),
        (&[0x48, 0x0f, 0xbf, 0xc1][..], 0x8001, 0, 0xffff_ffff_ffff_8001),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0xe80,
                flags: FlagState::from_bits(u16::MAX),
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[0] = initial;
        cpu.registers[1] = source;
        cpu.registers[9] = source;
        let flags = cpu.flags;
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let instruction = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[0], expected);
        assert_eq!(cpu.flags, flags);
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0xf00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.registers[8] = 7;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: 0x8001_u16.to_le_bytes().to_vec(),
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x4c, 0x0f, 0xbf, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 2
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x0f, 0xbf, 0xc0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x0f, 0xbf, 0xc0], 0).is_err());
}

#[test]
fn xlat_address_and_fault() {
    let flags = FlagState::from_bits(u16::MAX);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0xf80,
            flags,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[0] = 0x1122_3344_5566_7782;
    cpu.registers[3] = 0x1000;
    let mut memory = ModelMemory {
        base: 0x1082,
        bytes: vec![0xa5],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let instruction = X86ScalarDecoder::decode(&[0xd7], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 0x1122_3344_5566_77a5);
    assert_eq!(cpu.flags, flags);

    cpu.rip = 0xfa0;
    cpu.registers[0] = 2;
    cpu.registers[3] = 0xffff_ffff;
    cpu.fs_base = 0x2000;
    memory.base = 0x2001;
    memory.bytes[0] = 0x5a;
    let wrapped = X86ScalarDecoder::decode(&[0x64, 0x67, 0xd7], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, wrapped),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0] & 0xff, 0x5a);

    cpu.rip = 0xfc0;
    let original = cpu.clone();
    memory.fail_read = true;
    let fault = X86ScalarDecoder::decode(&[0x64, 0x67, 0xd7], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 1
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf0, 0xd7], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf2, 0xd7], 0).is_err());
}

#[test]
fn count_controlled_branches() {
    for (opcode, counter, zero, taken, expected) in [
        (0xe2_u8, 2_u64, false, true, 1_u64),
        (0xe2, 1, false, false, 0),
        (0xe1, 2, true, true, 1),
        (0xe1, 2, false, false, 1),
        (0xe0, 2, false, true, 1),
        (0xe0, 2, true, false, 1),
        (0xe3, 0, false, true, 0),
        (0xe3, 1, false, false, 1),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x1000,
                flags: FlagState::default().with(Flag::Zero, zero),
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[1] = counter;
        let flags = cpu.flags;
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let instruction = X86ScalarDecoder::decode(&[opcode, 2], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.rip, if taken { 0x1004 } else { 0x1002 });
        assert_eq!(cpu.registers[1], expected);
        assert_eq!(cpu.flags, flags);
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1100,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[1] = 0x1_0000_0001;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let loop32 = X86ScalarDecoder::decode(&[0x67, 0xe2, 0xfe], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, loop32),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[1], 0);
    assert_eq!(cpu.rip, 0x1103);
    assert!(X86ScalarDecoder::decode(&[0xf0, 0xe2, 0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf3, 0xe3, 0], 0).is_err());
}

#[test]
fn rorx_forms_and_fault() {
    let flags = FlagState::from_bits(u16::MAX);
    for (bytes, source, expected) in [
        (
            &[0xc4, 0xe3, 0xfb, 0xf0, 0xd1, 23][..],
            0x0123_4567_89ab_cdef_u64,
            0x579b_de02_468a_cf13,
        ),
        (
            &[0xc4, 0xe3, 0x7b, 0xf0, 0xd1, 4][..],
            0xffff_ffff_1234_5678,
            0x8123_4567,
        ),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x1200,
                flags,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[1] = source;
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let instruction = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[2], expected);
        assert_eq!(cpu.flags, flags);
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1240,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x2000;
    cpu.registers[2] = 7;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x2000,
        bytes: vec![0; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0xfb, 0xf0, 0x13, 0], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe3, 0xff, 0xf0, 0xc0, 1], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe3, 0xf3, 0xf0, 0xc0, 1], 0).is_err());
}

#[test]
fn rorx_segments() {
    let flags = FlagState::from_bits(u16::MAX);
    for (segment, segment_base) in [(0x64, 0x3000), (0x65, 0x4000)] {
        for (vex, width, source, expected) in [
            ([0xe3, 0xfb], 8_usize, 0x0123_4567_89ab_cdef_u64, 0xef01_2345_6789_abcd),
            ([0xe3, 0x7b], 4, 0xffff_ffff_1234_5678, 0x7812_3456),
        ] {
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x1280,
                    fs_base: 0x3000,
                    gs_base: 0x4000,
                    flags,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.registers[3] = 0x20;
            let mut memory = ModelMemory {
                base: segment_base + 0x20,
                bytes: source.to_le_bytes()[..width].to_vec(),
                fail_read: false,
                fail_write: false,
                commits: 0,
            };
            let bytes = [segment, 0xc4, vex[0], vex[1], 0xf0, 0x13, 8];
            let load = X86ScalarDecoder::decode(&bytes, cpu.rip).unwrap();
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, load),
                ExecutionExit::Continue
            );
            assert_eq!((cpu.registers[2], cpu.flags), (expected, flags));

            cpu.rip = 0x12c0;
            cpu.registers[1] = source;
            let bytes = [segment, 0xc4, vex[0], vex[1], 0xf0, 0xd1, 8];
            let register = X86ScalarDecoder::decode(&bytes, cpu.rip).unwrap();
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, register),
                ExecutionExit::Continue
            );
            assert_eq!((cpu.registers[2], cpu.flags), (expected, flags));
        }
    }
}

#[test]
fn bit_isolation_forms_and_fault() {
    for (modrm, value, expected, carry) in [
        (0xc9, 0x58_u64, 0x50_u64, false),
        (0xd1, 0x58, 0x0f, false),
        (0xd9, 0x58, 0x08, true),
        (0xc9, 0, 0, true),
        (0xd1, 0, u64::MAX, true),
        (0xd9, 0, 0, false),
    ] {
        let original_flags = FlagState::from_bits(u16::MAX);
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x1280,
                flags: original_flags,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[1] = value;
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0xa8, 0xf3, modrm], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[10], expected);
        assert_eq!(cpu.flags.contains(Flag::Carry), carry);
        assert_eq!(cpu.flags.contains(Flag::Zero), expected == 0);
        assert_eq!(cpu.flags.contains(Flag::Sign), expected >> 63 != 0);
        assert!(!cpu.flags.contains(Flag::Overflow));
        assert!(cpu.flags.contains(Flag::Parity));
        assert!(cpu.flags.contains(Flag::Auxiliary));
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x12c0,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x3000;
    cpu.registers[10] = 9;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x3000,
        bytes: vec![0; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0xa8, 0xf3, 0x0b], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);

    let dword = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x28, 0xf3, 0xd1], 0).unwrap();
    assert_eq!(dword.width, ScalarWidth::Dword);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0xac, 0xf3, 0xc9], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0x62, 0xa8, 0xf3, 0xc9], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0xa8, 0xf3, 0xc1], 0).is_err());
}

#[test]
fn bmi_segments() {
    for (segment, base) in [(0x64, 0x3000), (0x65, 0x4000)] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x12e0,
                fs_base: 0x3000,
                gs_base: 0x4000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[3] = 0x20;
        let mut memory = ModelMemory {
            base: base + 0x20,
            bytes: 0x58_u64.to_le_bytes().to_vec(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let isolate = X86ScalarDecoder::decode(&[segment, 0xc4, 0xe2, 0xa8, 0xf3, 0x0b], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, isolate),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[10], 0x50);

        cpu.rip = 0x12f0;
        cpu.registers[9] = 0xf0;
        let andn = X86ScalarDecoder::decode(&[segment, 0xc4, 0x62, 0xb0, 0xf2, 0x13], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, andn),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[10], 0x08);

        cpu.rip = 0x12f8;
        cpu.registers[9] = 4;
        let bzhi = X86ScalarDecoder::decode(&[segment, 0xc4, 0x62, 0xb0, 0xf5, 0x13], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, bzhi),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[10], 0x08);

        cpu.rip = 0x12fc;
        cpu.registers[9] = 1;
        let shift = X86ScalarDecoder::decode(&[segment, 0xc4, 0x62, 0xb1, 0xf7, 0x13], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, shift),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[10], 0xb0);

        cpu.rip = 0x12fd;
        cpu.registers[2] = 3;
        let mulx = X86ScalarDecoder::decode(&[segment, 0xc4, 0x62, 0xb3, 0xf6, 0x13], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, mulx),
            ExecutionExit::Continue
        );
        assert_eq!((cpu.registers[9], cpu.registers[10]), (0x108, 0));

        cpu.rip = 0x12fe;
        cpu.registers[9] = 0x35;
        let pext = X86ScalarDecoder::decode(&[segment, 0xc4, 0x62, 0xb2, 0xf5, 0x13], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, pext),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[10], 0x2);

        cpu.rip = 0x1300;
        cpu.registers[1] = 0x58;
        let register = X86ScalarDecoder::decode(&[segment, 0xc4, 0xe2, 0xa8, 0xf3, 0xc9], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, register),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[10], 0x50);

        cpu.rip = 0x1308;
        cpu.registers[9] = 4;
        let register = X86ScalarDecoder::decode(&[segment, 0xc4, 0x62, 0xb0, 0xf5, 0xd1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, register),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[10], 0x08);

        cpu.rip = 0x1310;
        cpu.registers[2] = 3;
        cpu.registers[1] = 0x58;
        let register = X86ScalarDecoder::decode(&[segment, 0xc4, 0x62, 0xb3, 0xf6, 0xd1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, register),
            ExecutionExit::Continue
        );
        assert_eq!((cpu.registers[9], cpu.registers[10]), (0x108, 0));

        cpu.rip = 0x1318;
        cpu.registers[9] = 5;
        cpu.registers[1] = 0x2a;
        let register = X86ScalarDecoder::decode(&[segment, 0xc4, 0x62, 0xb3, 0xf5, 0xd1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, register),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[10], 0x22);
    }
}

#[test]
fn andn_forms_and_fault() {
    let original_flags = FlagState::from_bits(u16::MAX);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1300,
            flags: original_flags,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[9] = 0xff00_ff00_ff00_ff00;
    cpu.registers[1] = 0xaaaa_5555_ffff_0000;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let qword = X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb0, 0xf2, 0xd1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, qword),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[10], 0x00aa_0055_00ff_0000);
    assert!(!cpu.flags.contains(Flag::Carry));
    assert!(!cpu.flags.contains(Flag::Overflow));
    assert!(!cpu.flags.contains(Flag::Zero));
    assert!(!cpu.flags.contains(Flag::Sign));
    assert!(cpu.flags.contains(Flag::Parity));
    assert!(cpu.flags.contains(Flag::Auxiliary));

    cpu.rip = 0x1320;
    cpu.registers[9] = 0;
    cpu.registers[1] = u64::MAX;
    let dword = X86ScalarDecoder::decode(&[0xc4, 0x62, 0x30, 0xf2, 0xd1], cpu.rip).unwrap();
    assert_eq!(dword.width, ScalarWidth::Dword);
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, dword),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[10], u64::from(u32::MAX));
    assert!(cpu.flags.contains(Flag::Sign));

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1340,
            flags: original_flags,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x3400;
    cpu.registers[9] = 0x1234;
    cpu.registers[10] = 0x5678;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x3400,
        bytes: vec![0; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb0, 0xf2, 0x13], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb4, 0xf2, 0xd1], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb1, 0xf2, 0xd1], 0).is_err());
}

#[test]
fn bzhi_forms_and_fault() {
    for (wide, index, expected, carry) in [
        (true, 12_u64, 0xdef_u64, false),
        (true, 0, 0, false),
        (true, 64, 0x0123_4567_89ab_cdef, true),
        (false, 8, 0xef, false),
        (false, 32, 0x89ab_cdef, true),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x1380,
                flags: FlagState::from_bits(u16::MAX),
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[1] = 0x0123_4567_89ab_cdef;
        cpu.registers[9] = index;
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let vex = if wide { 0xb0 } else { 0x30 };
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0x62, vex, 0xf5, 0xd1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[10], expected);
        assert_eq!(cpu.flags.contains(Flag::Carry), carry);
        assert_eq!(cpu.flags.contains(Flag::Zero), expected == 0);
        assert!(!cpu.flags.contains(Flag::Overflow));
        assert!(cpu.flags.contains(Flag::Parity));
        assert!(cpu.flags.contains(Flag::Auxiliary));
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x13c0,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x3800;
    cpu.registers[9] = 7;
    cpu.registers[10] = 3;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x3800,
        bytes: vec![0; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb0, 0xf5, 0x13], cpu.rip).unwrap();
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut memory, load), ExecutionExit::OperandFault(access) if access.length() == 8)
    );
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb4, 0xf5, 0xd1], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb1, 0xf5, 0xd1], 0).is_err());
}

#[test]
fn variable_shifts_and_fault() {
    for (pp, value, count, expected) in [
        (1_u8, 0x8000_0000_0000_0001_u64, 65_u64, 2_u64),
        (2, 0x8000_0000_0000_0001, 65, 0xc000_0000_0000_0000),
        (3, 0x8000_0000_0000_0001, 65, 0x4000_0000_0000_0000),
    ] {
        let flags = FlagState::from_bits(u16::MAX);
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x1400,
                flags,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[1] = value;
        cpu.registers[9] = count;
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb0 | pp, 0xf7, 0xd1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[10], expected);
        assert_eq!(cpu.flags, flags);
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1420,
            flags: FlagState::from_bits(0x845),
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[1] = 0x8000_0001;
    cpu.registers[9] = 36;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let dword = X86ScalarDecoder::decode(&[0xc4, 0x62, 0x32, 0xf7, 0xd1], cpu.rip).unwrap();
    assert_eq!(dword.width, ScalarWidth::Dword);
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, dword),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[10], 0xf800_0000);
    assert_eq!(cpu.flags, FlagState::from_bits(0x845));

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1440,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x4000;
    cpu.registers[9] = 7;
    cpu.registers[10] = 3;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x4000,
        bytes: vec![0; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb3, 0xf7, 0x13], cpu.rip).unwrap();
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut memory, load), ExecutionExit::OperandFault(access) if access.length() == 8)
    );
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb7, 0xf7, 0xd1], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb0, 0xf7, 0xd1], 0).is_err());
}

#[test]
fn mulx_forms_aliases_and_fault() {
    let flags = FlagState::from_bits(u16::MAX);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1480,
            flags,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[2] = u64::MAX;
    cpu.registers[1] = 2;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let qword = X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb3, 0xf6, 0xd1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, qword),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[9], u64::MAX - 1);
    assert_eq!(cpu.registers[10], 1);
    assert_eq!(cpu.flags, flags);

    cpu.rip = 0x14a0;
    cpu.registers[2] = 0xffff_ffff;
    let dword = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x73, 0xf6, 0xca], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, dword),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[1], 0xffff_fffe);

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x14c0,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x4400;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x4400,
        bytes: vec![0; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb3, 0xf6, 0x13], cpu.rip).unwrap();
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut memory, load), ExecutionExit::OperandFault(access) if access.length() == 8)
    );
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb7, 0xf6, 0xd1], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb2, 0xf6, 0xd1], 0).is_err());
}

#[test]
fn pext_pdep_forms_and_fault() {
    let flags = FlagState::from_bits(u16::MAX);
    for (pp, source, mask, expected) in [(2_u8, 0x35_u64, 0x2a_u64, 4_u64), (3, 5, 0x2a, 0x22)] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x1500,
                flags,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[9] = source;
        cpu.registers[1] = mask;
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb0 | pp, 0xf5, 0xd1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[10], expected);
        assert_eq!(cpu.flags, flags);
    }
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1520,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x4800;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x4800,
        bytes: vec![0; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb2, 0xf5, 0x13], cpu.rip).unwrap();
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut memory, load), ExecutionExit::OperandFault(a) if a.length() == 8)
    );
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0x62, 0xb6, 0xf5, 0xd1], 0).is_err());
}

#[test]
fn cmov_conditions_execute() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    for condition in 0_u8..16 {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x900,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[0] = 0xffff_ffff_aaaa_aaaa;
        cpu.registers[2] = 0x1234_5678;
        let ir = X86ScalarDecoder::decode(&[0x0f, 0x40 | condition, 0xc2], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        let expected = if condition & 1 != 0 { 0x1234_5678 } else { 0xaaaa_aaaa };
        assert_eq!((cpu.registers[0], cpu.rip), (expected, 0x903));
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0xa00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[2] = 0xfeed_face_cafe_beef;
    cpu.flags = FlagState::from_bits(1 << Flag::Carry as u8);
    let qword = X86ScalarDecoder::decode(&[0x48, 0x0f, 0x42, 0xc2], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, qword),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.registers[0], cpu.rip), (0xfeed_face_cafe_beef, 0xa04));

    cpu.rip = 0xb00;
    cpu.registers[0] = 0x1122_3344_5566_7788;
    cpu.registers[2] = 0xffff;
    cpu.flags = FlagState::from_bits(0);
    let word = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x42, 0xc2], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, word),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 0x1122_3344_5566_7788);
}

#[test]
fn cmov_memory_faults() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0xc00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[13] = 0x1010;
    cpu.registers[8] = 0xfeed;
    cpu.flags = FlagState::from_bits(0);
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let ir = X86ScalarDecoder::decode(&[0x4d, 0x0f, 0x42, 0x45, 0x08], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
        ExecutionExit::OperandFault(access) if matches!(access.fault(), MemoryFault {
                instruction: 0xc00,
                address: 0x1018,
                access: AccessKind::Read,
            }) && access.length() == 8
    ));
    assert_eq!(cpu, original);
}

#[test]
fn leave_modes_execute() {
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    memory.bytes[..8].copy_from_slice(&0xfeed_face_cafe_beef_u64.to_le_bytes());
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0xd00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[4] = 0x2222;
    cpu.registers[5] = 0x1000;
    let leave = X86ScalarDecoder::decode(&[0xc9], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, leave),
        ExecutionExit::Continue
    );
    assert_eq!(
        (cpu.registers[4], cpu.registers[5], cpu.rip),
        (0x1008, 0xfeed_face_cafe_beef, 0xd01)
    );

    memory.bytes[..2].copy_from_slice(&0xabcd_u16.to_le_bytes());
    cpu.rip = 0xe00;
    cpu.registers[5] = 0x1122_3344_0000_1000;
    let narrow = X86ScalarDecoder::decode(&[0x67, 0x66, 0xc9], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, narrow),
        ExecutionExit::Continue
    );
    assert_eq!(
        (cpu.registers[4], cpu.registers[5], cpu.rip),
        (0x1002, 0x1122_3344_0000_abcd, 0xe03)
    );

    memory.bytes[..8].copy_from_slice(&0x1234_u64.to_le_bytes());
    cpu.rip = 0xf00;
    cpu.registers[5] = 0x1000;
    let rex_wins = X86ScalarDecoder::decode(&[0x66, 0x48, 0xc9], cpu.rip).unwrap();
    assert_eq!(rex_wins.width, ScalarWidth::Qword);
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, rex_wins),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.registers[4], cpu.registers[5]), (0x1008, 0x1234));
}

#[test]
fn leave_fault_state() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1100,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[4] = 0x2222;
    cpu.registers[5] = 0x1000;
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let leave = X86ScalarDecoder::decode(&[0xc9], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, leave),
        ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x1100,
                    address: 0x1000,
                    access: AccessKind::Read,
                },
                8
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!((cpu.registers[4], cpu.registers[5], cpu.rip), (0x1000, 0x1000, 0x1100));
}

#[test]
fn leave_rejects_prefixes() {
    for bytes in [&[0xf3, 0xc9][..], &[0xf0, 0xc9][..], &[0x64, 0xc9][..]] {
        assert_eq!(X86ScalarDecoder::decode(bytes, 0), Err(ScalarIrError::Invalid));
    }
}

#[test]
fn cpuid_executes_policy() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1200,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[0] = 0;
    cpu.registers[1] = u64::MAX;
    cpu.registers[2] = u64::MAX;
    cpu.registers[3] = u64::MAX;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let cpuid = X86ScalarDecoder::decode(&[0x0f, 0xa2], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, cpuid),
        ExecutionExit::Continue
    );
    assert_eq!(
        (
            cpu.registers[0],
            cpu.registers[3],
            cpu.registers[1],
            cpu.registers[2],
            cpu.rip
        ),
        (7, 0x756e_6547, 0x6c65_746e, 0x4965_6e69, 0x1202)
    );

    cpu.rip = 0x1300;
    cpu.registers[0] = 0x8000_0008;
    cpu.registers[1] = 0;
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, cpuid),
        ExecutionExit::Continue
    );
    assert_eq!(
        (cpu.registers[0], cpu.registers[1], cpu.registers[2], cpu.registers[3]),
        (0x3027, 0, 0, 0)
    );
}

#[test]
fn timestamp_counter_reads() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            registers: [u64::MAX; 16],
            flags: FlagState::from_bits(0x8d5),
            rip: 0x1280,
            ..Default::default()
        },
        ..Default::default()
    };
    let original_flags = cpu.flags;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let instruction = X86ScalarDecoder::decode(&[0x0f, 0x31], cpu.rip).unwrap();

    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::TimestampCounter {
            instruction: 0x1280,
            next: 0x1282,
            auxiliary: false,
        }
    );
    assert_eq!((cpu.rip, cpu.flags), (0x1280, original_flags));
    assert_eq!(cpu.registers[1], u64::MAX);
}

#[test]
fn timestamp_counter_auxiliary() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            registers: [u64::MAX; 16],
            flags: FlagState::from_bits(0x8d5),
            rip: 0x1400,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: true,
        fail_write: true,
        commits: 0,
    };
    let flags = cpu.flags;
    let instruction = X86ScalarDecoder::decode(&[0x0f, 0x01, 0xf9], cpu.rip).unwrap();
    assert_eq!(
        instruction.instruction,
        ScalarInstruction::TimestampCounter { auxiliary: true }
    );
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::TimestampCounter {
            instruction: 0x1400,
            next: 0x1403,
            auxiliary: true,
        }
    );
    assert_eq!((cpu.flags, cpu.rip), (flags, 0x1400));
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x01, 0xf9], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x0f, 0x01, 0xf9], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0x0f, 0x01, 0xf8], 0).is_err());
}

#[test]
fn timestamp_counter_prefixes() {
    for bytes in [
        &[0xf3, 0x0f, 0x31][..],
        &[0xf0, 0x0f, 0x31][..],
        &[0x64, 0x0f, 0x31][..],
    ] {
        assert_eq!(X86ScalarDecoder::decode(bytes, 0), Err(ScalarIrError::Invalid));
    }
}

#[test]
fn mmx_scalar_aliases() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1800,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[1] = 0x1122_3344_aabb_ccdd;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    for bytes in [&[0x0f, 0x6e, 0xc1][..], &[0x44, 0x0f, 0x6e, 0xc1][..]] {
        cpu.rip = 0x1800;
        let instruction = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.read_mmx(0), 0xaabb_ccdd);
        assert_eq!(cpu.x87_values[0].bits() >> 64, 0xffff);
        assert_eq!(cpu.x87_classes[0], ExtendedClass::Normal);
    }
    cpu.rip = 0x1900;
    let wide = X86ScalarDecoder::decode(&[0x48, 0x0f, 0x6e, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, wide),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.read_mmx(0), 0x1122_3344_aabb_ccdd);
    cpu.rip = 0x1a00;
    let store = X86ScalarDecoder::decode(&[0x48, 0x0f, 0x7e, 0xc2], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[2], 0x1122_3344_aabb_ccdd);
}

#[test]
fn mmx_transport_tags() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1b00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.write_mmx(1, 0x8877_6655_4433_2211);
    let mut memory = ModelMemory {
        base: 0x2000,
        bytes: vec![0; 16],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let copy = X86ScalarDecoder::decode(&[0x0f, 0x6f, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, copy),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.read_mmx(0), 0x8877_6655_4433_2211);
    cpu.rip = 0x1c00;
    cpu.registers[0] = 0x2000;
    let store = X86ScalarDecoder::decode(&[0x0f, 0x7f, 0x08], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(&memory.bytes[..8], &0x8877_6655_4433_2211_u64.to_le_bytes());
    let values = cpu.x87_values;
    cpu.rip = 0x1d00;
    let empty = X86ScalarDecoder::decode(&[0x0f, 0x77], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, empty),
        ExecutionExit::Continue
    );
    assert!(cpu.x87_classes.iter().all(|class| *class == ExtendedClass::Empty));
    assert_eq!(cpu.x87_values, values);
}

#[test]
fn mmx_fault_rollback() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            registers: [0x2000; 16],
            rip: 0x1e00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.write_mmx(0, 7);
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x2000,
        bytes: vec![0; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x0f, 0x6f, 0x00], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
}

#[test]
fn mmx_float_conversion_family() {
    let mut memory = ModelMemory {
        base: 0x2000,
        bytes: [7_i32.to_le_bytes(), (-9_i32).to_le_bytes()].concat(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1f00,
            flags: FlagState::from_bits(0x8d5),
            ..Default::default()
        },
        mxcsr: 0x1f80,
        ..Default::default()
    };
    let flags = cpu.flags;
    cpu.registers[3] = 0x2000;
    cpu.vectors[0] = 0xaabb_ccdd_eeff_0011_0123_4567_89ab_cdef;
    let singles = X86ScalarDecoder::decode(&[0x0f, 0x2a, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, singles),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] >> 64, 0xaabb_ccdd_eeff_0011);
    assert_eq!(cpu.vectors[0] as u32, 7.0_f32.to_bits());
    assert_eq!((cpu.vectors[0] >> 32) as u32, (-9.0_f32).to_bits());

    cpu.rip = 0x1f10;
    cpu.write_mmx(1, u64::from(11_u32) | (u64::from((-13_i32) as u32) << 32));
    let doubles = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x2a, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, doubles),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u64, 11.0_f64.to_bits());
    assert_eq!((cpu.vectors[0] >> 64) as u64, (-13.0_f64).to_bits());

    cpu.rip = 0x1f20;
    cpu.vectors[1] = u128::from(1.75_f32.to_bits()) | (u128::from((-1.75_f32).to_bits()) << 32);
    cpu.mxcsr = 0x1f80 | (2 << 13);
    let rounded = X86ScalarDecoder::decode(&[0x0f, 0x2d, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, rounded),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.read_mmx(0), u64::from(2_u32) | (u64::from((-1_i32) as u32) << 32));
    assert_ne!(cpu.mxcsr & (1 << 5), 0);

    cpu.rip = 0x1f30;
    cpu.mxcsr = 0x1f80;
    let truncated = X86ScalarDecoder::decode(&[0x0f, 0x2c, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, truncated),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.read_mmx(0), u64::from(1_u32) | (u64::from((-1_i32) as u32) << 32));

    cpu.rip = 0x1f40;
    cpu.mxcsr = 0x1f80;
    cpu.vectors[1] = u128::from(f64::NAN.to_bits()) | (u128::from(f64::INFINITY.to_bits()) << 64);
    let invalid = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x2d, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, invalid),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.read_mmx(0), 0x8000_0000_8000_0000);
    assert_ne!(cpu.mxcsr & 1, 0);
    assert_eq!(cpu.flags, flags);

    cpu.rip = 0x1f50;
    cpu.registers[3] = 0x2000;
    let original = cpu.clone();
    memory.fail_read = true;
    let fault = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x2d, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);
}

#[test]
fn cpuid_rejects_prefixes() {
    for bytes in [&[0xf3, 0x0f, 0xa2][..], &[0xf0, 0x0f, 0xa2][..]] {
        assert_eq!(X86ScalarDecoder::decode(bytes, 0), Err(ScalarIrError::Invalid));
    }
    assert_eq!(X86ScalarDecoder::decode(&[0x66, 0x0f, 0xa2], 0).unwrap().length, 3);
}

#[test]
fn group_two_executes() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    for (raw, operation) in [
        (0, ShiftOperation::RotateLeft),
        (1, ShiftOperation::RotateRight),
        (2, ShiftOperation::CarryLeft),
        (3, ShiftOperation::CarryRight),
        (4, ShiftOperation::Left),
        (5, ShiftOperation::Right),
        (6, ShiftOperation::Left),
        (7, ShiftOperation::ArithmeticRight),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x1400,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[2] = 0x8000_0001;
        cpu.flags = FlagState::from_bits(1);
        let ir = X86ScalarDecoder::decode(&[0xc1, 0xc0 | (raw << 3) | 2, 1], cpu.rip).unwrap();
        assert!(matches!(
            ir.instruction,
            ScalarInstruction::Shift {
                operation: actual,
                count: ShiftCount::Immediate(1),
                ..
            } if actual == operation
        ));
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.rip, 0x1403);
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1500,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[2] = 0x8000_0000;
    let by_one = X86ScalarDecoder::decode(&[0xd1, 0xea], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, by_one),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[2], 0x4000_0000);
    cpu.rip = 0x1600;
    cpu.registers[1] = 8;
    let by_cl = X86ScalarDecoder::decode(&[0xd3, 0xea], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, by_cl),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[2], 0x0040_0000);

    cpu.rip = 0x1700;
    cpu.registers[0] = 0x1122_3344_5566_aa80;
    let high = X86ScalarDecoder::decode(&[0xc0, 0xec, 1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, high),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 0x1122_3344_5566_55_80);
    cpu.rip = 0x1800;
    cpu.registers[4] = 0x80;
    let rex = X86ScalarDecoder::decode(&[0x40, 0xc0, 0xec, 1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, rex),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[4] & 0xff, 0x40);
}

#[test]
fn group_two_faults() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1900,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.flags = FlagState::from_bits(0x8c5);
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0x81; 8],
        fail_read: false,
        fail_write: true,
        commits: 0,
    };
    let shift = X86ScalarDecoder::decode(&[0xc0, 0x23, 1], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, shift),
        ExecutionExit::OperandFault(access) if matches!(access.fault(), MemoryFault {
                instruction: 0x1900,
                address: 0x1000,
                access: AccessKind::Write,
            }) && access.length() == 1
    ));
    assert_eq!(cpu, original);

    memory.fail_write = false;
    cpu.rip = 0x1a00;
    cpu.registers[0] = u64::MAX;
    cpu.flags = FlagState::from_bits(0x8c5);
    let unchanged = X86ScalarDecoder::decode(&[0xc1, 0xe8, 0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, unchanged),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.registers[0], cpu.flags.bits()), (0xffff_ffff, 0x8c5));
}

#[test]
fn group_three_executes() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1b00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[12] = 0x1800;
    let test = X86ScalarDecoder::decode(&[0x41, 0xf7, 0xc4, 0x00, 0x08, 0x00, 0x00], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, test),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.registers[12], cpu.rip), (0x1800, 0x1b07));
    assert!(!cpu.flags.contains(Flag::Zero));

    cpu.rip = 0x1c00;
    cpu.registers[0] = 0x1122_3344_5566_aa80;
    let not_high = X86ScalarDecoder::decode(&[0xf6, 0xd4], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, not_high),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 0x1122_3344_5566_5580);

    cpu.rip = 0x1d00;
    cpu.registers[0] = 0x0310;
    let multiply = X86ScalarDecoder::decode(&[0xf6, 0xe4], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, multiply),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0] & 0xffff, 0x30);

    cpu.rip = 0x1e00;
    cpu.registers[0] = (-3_i64) as u64;
    cpu.registers[3] = 7;
    let signed = X86ScalarDecoder::decode(&[0x48, 0xf7, 0xeb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, signed),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.registers[0], cpu.registers[2]), ((-21_i64) as u64, u64::MAX));

    cpu.rip = 0x1f00;
    cpu.registers[0] = 0x123;
    cpu.registers[3] = 0x10;
    let divide = X86ScalarDecoder::decode(&[0xf6, 0xf3], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, divide),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0] & 0xffff, 0x0312);
}

#[test]
fn group_three_faults() {
    assert_eq!(
        X86ScalarDecoder::decode(&[0xf7, 0xc8, 1, 0, 0, 0], 0),
        Err(ScalarIrError::Invalid)
    );
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x2000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[0] = 0x1234;
    cpu.registers[2] = 0;
    cpu.registers[3] = 0;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let divide = X86ScalarDecoder::decode(&[0xf7, 0xf3], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, divide),
        ExecutionExit::DivideError {
            instruction: 0x2000,
            error: DivisionError::Zero,
        }
    );
    assert_eq!(cpu, original);

    cpu.rip = 0x2100;
    cpu.registers[0] = 0;
    cpu.registers[2] = 1;
    cpu.registers[3] = 1;
    let original = cpu.clone();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, divide),
        ExecutionExit::DivideError {
            instruction: 0x2100,
            error: DivisionError::QuotientOverflow,
        }
    );
    assert_eq!(cpu, original);

    cpu.rip = 0x2200;
    cpu.registers[3] = 0x1000;
    let original = cpu.clone();
    memory.fail_read = true;
    let source = X86ScalarDecoder::decode(&[0xf7, 0x23], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, source),
        ExecutionExit::OperandFault(access) if access.length() == 4
    ));
    assert_eq!(cpu, original);
}

impl GuestOperandMemory for ModelMemory {
    type Reservation = (usize, u8);
    type BatchReservation = Vec<(usize, u8)>;

    fn read(&self, address: u64, bytes: u8) -> Result<u64, ()> {
        if self.fail_read {
            return Err(());
        }
        let offset = usize::try_from(address.checked_sub(self.base).ok_or(())?).map_err(|_| ())?;
        let slice = self.bytes.get(offset..offset + usize::from(bytes)).ok_or(())?;
        let mut value = 0_u64;
        for (shift, byte) in slice.iter().enumerate() {
            value |= u64::from(*byte) << (shift * 8);
        }
        Ok(value)
    }

    fn reserve_write(&self, address: u64, bytes: u8) -> Result<Self::Reservation, ()> {
        if self.fail_write {
            return Err(());
        }
        let offset = usize::try_from(address.checked_sub(self.base).ok_or(())?).map_err(|_| ())?;
        self.bytes.get(offset..offset + usize::from(bytes)).ok_or(())?;
        Ok((offset, bytes))
    }

    fn commit_write(&mut self, (offset, bytes): Self::Reservation, value: u64) -> Result<(), ()> {
        self.commits += 1;
        for index in 0..usize::from(bytes) {
            self.bytes[offset + index] = (value >> (index * 8)) as u8;
        }
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

impl ExclusiveMemory for ModelMemory {
    fn load_ordered(&mut self, address: u64, bytes: u8, _order: MemoryOrder) -> Result<u64, ()> {
        self.read(address, bytes)
    }

    fn store_ordered(&mut self, address: u64, bytes: u8, value: u64, _order: MemoryOrder) -> Result<(), ()> {
        let reservation = self.reserve_write(address, bytes)?;
        self.commit_write(reservation, value)
    }

    fn load_exclusive(
        &mut self,
        address: u64,
        element_bytes: u8,
        pair: bool,
        _order: MemoryOrder,
    ) -> Result<ExclusiveLoad, ()> {
        let low = self.read(address, element_bytes)?;
        let high = if pair {
            self.read(address + u64::from(element_bytes), element_bytes)?
        } else {
            0
        };
        Ok(ExclusiveLoad {
            value: AtomicValue { low, high },
            reservation: ExclusiveReservation::new(address, element_bytes, pair, MappingGeneration::new(0)),
        })
    }

    fn store_exclusive(
        &mut self,
        reservation: ExclusiveReservation,
        replacement: AtomicValue,
        order: MemoryOrder,
    ) -> Result<bool, ()> {
        self.store_ordered(
            reservation.address(),
            reservation.element_bytes(),
            replacement.low,
            order,
        )?;
        if reservation.pair() {
            self.store_ordered(
                reservation.address() + u64::from(reservation.element_bytes()),
                reservation.element_bytes(),
                replacement.high,
                order,
            )?;
        }
        Ok(true)
    }

    fn compare_exchange(
        &mut self,
        address: u64,
        element_bytes: u8,
        pair: bool,
        expected: AtomicValue,
        replacement: AtomicValue,
        order: MemoryOrder,
    ) -> Result<AtomicValue, ()> {
        let loaded = self.load_exclusive(address, element_bytes, pair, order)?;
        if loaded.value == expected {
            self.store_exclusive(loaded.reservation, replacement, order)?;
        }
        Ok(loaded.value)
    }

    fn fetch_update(
        &mut self,
        address: u64,
        bytes: u8,
        operation: AtomicOperation,
        operand: u64,
        order: MemoryOrder,
    ) -> Result<u64, ()> {
        let observed = self.read(address, bytes)?;
        let replacement = match operation {
            AtomicOperation::Swap => operand,
            AtomicOperation::Add => observed.wrapping_add(operand),
            AtomicOperation::Clear => observed & !operand,
            AtomicOperation::ExclusiveOr => observed ^ operand,
            AtomicOperation::Set => observed | operand,
            AtomicOperation::SignedMaximum => observed.max(operand),
            AtomicOperation::SignedMinimum => observed.min(operand),
            AtomicOperation::UnsignedMaximum => observed.max(operand),
            AtomicOperation::UnsignedMinimum => observed.min(operand),
        };
        self.store_ordered(address, bytes, replacement, order)?;
        Ok(observed)
    }
}

#[test]
fn lock_decode() {
    let locked = X86ScalarDecoder::decode(&[0xf0, 0x48, 0x83, 0x03, 1], 0x100).unwrap();
    assert!(matches!(
        locked.instruction,
        ScalarInstruction::Alu {
            operation: AluOperation::Add,
            destination: ScalarOperand::Memory(_),
            source: ScalarOperand::Immediate(1),
            locked: true,
        }
    ));
    let ordinary = X86ScalarDecoder::decode(&[0x48, 0x83, 0x03, 1], 0x100).unwrap();
    assert!(matches!(
        ordinary.instruction,
        ScalarInstruction::Alu { locked: false, .. }
    ));
    let register = X86ScalarDecoder::decode(&[0xf0, 0x48, 0x01, 0x0b], 0x100).unwrap();
    assert!(matches!(
        register.instruction,
        ScalarInstruction::Alu {
            source: ScalarOperand::Register(ScalarRegister::General(1)),
            locked: true,
            ..
        }
    ));
    for invalid in [
        &[0xf0, 0x48, 0x83, 0xc0, 1][..],
        &[0xf0, 0x48, 0x85, 0x03],
        &[0xf0, 0x48, 0x83, 0x3b, 1],
    ] {
        assert_eq!(X86ScalarDecoder::decode(invalid, 0x100), Err(ScalarIrError::Invalid));
    }
}

#[test]
fn lock_updates() {
    let cases = [
        (0x03, 5, 6, false),
        (0x0b, 4, 5, false),
        (0x13, 5, 7, true),
        (0x1b, 5, 3, true),
        (0x23, 7, 1, false),
        (0x2b, 5, 4, false),
        (0x33, 5, 4, false),
    ];
    for (modrm, initial, expected, carry) in cases {
        let mut memory = LockedMemory::new(0x1000, 16);
        memory.replace(0x1001, 8, initial).unwrap();
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x200,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[3] = 0x1001;
        cpu.flags = cpu.flags.with(Flag::Carry, carry);
        let instruction = X86ScalarDecoder::decode(&[0xf0, 0x48, 0x83, modrm, 1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(memory.value(0x1001, 8).unwrap(), expected);
        assert_eq!(memory.atomics.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert_eq!(memory.writes.load(std::sync::atomic::Ordering::Relaxed), 0);
        assert_eq!(cpu.rip, 0x205);
    }

    let mut memory = LockedMemory::new(0x1000, 80);
    memory.replace(0x103f, 8, 9).unwrap();
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x280,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x103f;
    let instruction = X86ScalarDecoder::decode(&[0xf0, 0x48, 0x83, 0x03, 1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!(memory.value(0x103f, 8).unwrap(), 10);
}

#[test]
fn lock_contends() {
    let memory = LockedMemory::new(0x1000, 16);
    let instruction = X86ScalarDecoder::decode(&[0xf0, 0x48, 0x83, 0x03, 1], 0x300).unwrap();
    let workers = (0..4)
        .map(|_| {
            let mut memory = memory.clone();
            std::thread::spawn(move || {
                let mut cpu = CpuState {
                    scalar: ScalarState {
                        rip: 0x300,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                cpu.registers[3] = 0x1001;
                for _ in 0..2_000 {
                    assert_eq!(
                        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
                        ExecutionExit::Continue
                    );
                }
            })
        })
        .collect::<Vec<_>>();
    for worker in workers {
        worker.join().unwrap();
    }
    assert_eq!(memory.value(0x1001, 8).unwrap(), 8_000);
    assert_eq!(memory.atomics.load(std::sync::atomic::Ordering::Relaxed), 8_000);
    assert_eq!(memory.writes.load(std::sync::atomic::Ordering::Relaxed), 0);
}

#[test]
fn lock_fault_order() {
    let mut memory = LockedMemory::new(0x1000, 16);
    memory.fail = true;
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x400,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1001;
    let original = cpu.clone();
    let instruction = X86ScalarDecoder::decode(&[0xf0, 0x48, 0x83, 0x03, 1], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::OperandFault(access)
            if access.length() == 8 && matches!(access.fault(), MemoryFault { access: AccessKind::Write, .. })
    ));
    assert_eq!(cpu, original);
    assert_eq!(memory.atomics.load(std::sync::atomic::Ordering::Relaxed), 1);
    assert_eq!(memory.writes.load(std::sync::atomic::Ordering::Relaxed), 0);

    memory.fail = false;
    memory.atomics.store(0, std::sync::atomic::Ordering::Relaxed);
    cpu.registers[3] = u64::MAX;
    let original = cpu.clone();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::NonCanonical {
            access: AccessKind::Write,
            ..
        }
    ));
    assert_eq!(cpu, original);
    assert_eq!(memory.atomics.load(std::sync::atomic::Ordering::Relaxed), 0);
}

#[test]
fn cmpxchg_decode_rules() {
    let word = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xb1, 0xcb], 0).unwrap();
    assert_eq!(word.width, ScalarWidth::Word);
    assert_eq!(
        word.instruction,
        ScalarInstruction::CompareExchange {
            destination: ScalarOperand::Register(ScalarRegister::General(3)),
            source: ScalarRegister::General(1),
            locked: false,
        }
    );
    let qword = X86ScalarDecoder::decode(&[0x4d, 0x0f, 0xb1, 0xc8], 0).unwrap();
    assert_eq!(qword.width, ScalarWidth::Qword);
    assert!(matches!(
        qword.instruction,
        ScalarInstruction::CompareExchange {
            destination: ScalarOperand::Register(ScalarRegister::General(8)),
            source: ScalarRegister::General(9),
            locked: false,
        }
    ));
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x0f, 0xb1, 0xc8], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x0f, 0xb1, 0x08], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0xb1, 0x08], 0).is_err());
}

#[test]
fn wide_cmpxchg_decode() {
    let eight = X86ScalarDecoder::decode(&[0x0f, 0xc7, 0x0b], 0).unwrap();
    assert!(matches!(
        eight.instruction,
        ScalarInstruction::WideCompareExchange {
            wide: false,
            locked: false,
            ..
        }
    ));
    let sixteen = X86ScalarDecoder::decode(&[0xf0, 0x48, 0x0f, 0xc7, 0x0b], 0).unwrap();
    assert!(matches!(
        sixteen.instruction,
        ScalarInstruction::WideCompareExchange {
            wide: true,
            locked: true,
            ..
        }
    ));
    assert!(X86ScalarDecoder::decode(&[0x48, 0x0f, 0xc7, 0xc9], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0x0f, 0xc7, 0x03], 0).is_err());
}

#[test]
fn wide_cmpxchg_semantics() {
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 16],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    memory.bytes[..8].copy_from_slice(&0x1122_3344_5566_7788_u64.to_le_bytes());
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x200,
            flags: FlagState::from_bits(0x895),
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.registers[0] = 0x5566_7788;
    cpu.registers[2] = 0x1122_3344;
    cpu.registers[1] = 0xaabb_ccdd;
    let instruction = X86ScalarDecoder::decode(&[0x0f, 0xc7, 0x0b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!(memory.read(0x1000, 8).unwrap(), 0xaabb_ccdd_0000_1000);
    assert!(cpu.flags.contains(Flag::Zero));
    assert_eq!(
        cpu.flags.bits() & !(1 << Flag::Zero as u8),
        0x895 & !(1 << Flag::Zero as u8)
    );

    memory.bytes[..8].copy_from_slice(&0xdead_beef_cafe_babe_u64.to_le_bytes());
    cpu.registers[0] = 1;
    cpu.registers[2] = 2;
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.registers[2], cpu.registers[0]), (0xdead_beef, 0xcafe_babe));
    assert!(!cpu.flags.contains(Flag::Zero));

    memory.fail_write = true;
    cpu.registers[0] = 1;
    cpu.registers[2] = 2;
    let original = cpu.clone();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::OperandFault(access)
            if matches!(access.fault(), MemoryFault { access: AccessKind::Write, .. })
    ));
    assert_eq!(cpu, original);

    let mut pair = LockedMemory::new(0x2000, 16);
    pair.replace(0x2000, 8, 7).unwrap();
    pair.replace(0x2008, 8, 9).unwrap();
    let mut wide = CpuState {
        scalar: ScalarState {
            rip: 0x300,
            flags: FlagState::from_bits(1 << Flag::Carry as u8),
            ..Default::default()
        },
        ..Default::default()
    };
    wide.registers[3] = 0x2000;
    wide.registers[0] = 7;
    wide.registers[2] = 9;
    wide.registers[1] = 11;
    wide.registers[3] = 0x2000;
    let decoded = X86ScalarDecoder::decode(&[0xf0, 0x48, 0x0f, 0xc7, 0x0b], wide.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut wide, &mut pair, decoded),
        ExecutionExit::Continue
    );
    assert_eq!(
        (pair.value(0x2008, 8).unwrap(), pair.value(0x2000, 8).unwrap()),
        (11, 0x2000)
    );
    assert_eq!(wide.flags.bits(), (1 << Flag::Carry as u8) | (1 << Flag::Zero as u8));

    wide.registers[3] = 0x3000;
    let original = wide.clone();
    assert!(matches!(
        ScalarInterpreter::execute(&mut wide, &mut pair, decoded),
        ExecutionExit::OperandFault(_)
    ));
    assert_eq!(wide, original);
}

#[test]
fn movbe_decode_matrix() {
    for (prefix, width) in [
        (&[0x66][..], ScalarWidth::Word),
        (&[][..], ScalarWidth::Dword),
        (&[0x48][..], ScalarWidth::Qword),
    ] {
        for (opcode, store) in [(0xf0, false), (0xf1, true)] {
            let mut bytes = prefix.to_vec();
            bytes.extend_from_slice(&[0x0f, 0x38, opcode, 0x0e]);
            let decoded = X86ScalarDecoder::decode(&bytes, 0).unwrap();
            assert_eq!(decoded.width, width);
            assert!(matches!(
                decoded.instruction,
                ScalarInstruction::EndianMove {
                    register: ScalarRegister::General(1),
                    store: actual,
                    ..
                } if actual == store
            ));
        }
    }
    let extended = X86ScalarDecoder::decode(&[0x44, 0x0f, 0x38, 0xf0, 0x06], 0).unwrap();
    assert!(matches!(
        extended.instruction,
        ScalarInstruction::EndianMove {
            register: ScalarRegister::General(8),
            ..
        }
    ));
    assert_eq!(
        X86ScalarDecoder::decode(&[0x66, 0x48, 0x0f, 0x38, 0xf0, 0x06], 0)
            .unwrap()
            .width,
        ScalarWidth::Qword
    );
    assert!(X86ScalarDecoder::decode(&[0x0f, 0x38, 0xf0, 0xce], 0).is_err());
    assert!(matches!(
        X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x38, 0xf0, 0x06], 0)
            .unwrap()
            .instruction,
        ScalarInstruction::Crc32c { .. }
    ));
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x0f, 0x38, 0xf0, 0x06], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x38, 0xf0, 0x06], 0).is_ok());
}

#[test]
fn movbe_semantics() {
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    for (prefix, width, expected) in [
        (&[0x66][..], ScalarWidth::Word, 0x1122),
        (&[][..], ScalarWidth::Dword, 0x1122_3344),
        (&[0x48][..], ScalarWidth::Qword, 0x1122_3344_5566_7788),
    ] {
        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(&[0x0f, 0x38, 0xf0, 0x0e]);
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x200,
                flags: FlagState::from_bits(0x8d5),
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[1] = u64::MAX;
        cpu.registers[6] = 0x1000;
        let flags = cpu.flags;
        let decoded = X86ScalarDecoder::decode(&bytes, cpu.rip).unwrap();
        assert_eq!(decoded.width, width);
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::Continue
        );
        let register = match width {
            ScalarWidth::Word => !0xffff | expected,
            ScalarWidth::Dword | ScalarWidth::Qword => expected,
            ScalarWidth::Byte => unreachable!(),
        };
        assert_eq!(cpu.registers[1], register);
        assert_eq!(cpu.flags, flags);
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x300,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[1] = 0x0102_0304_0506_0708;
    cpu.registers[6] = 0x1000;
    let store = X86ScalarDecoder::decode(&[0x48, 0x0f, 0x38, 0xf1, 0x0e], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(memory.bytes, [1, 2, 3, 4, 5, 6, 7, 8]);

    memory.fail_write = true;
    let original = cpu.clone();
    let bytes = memory.bytes.clone();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::OperandFault(_)
    ));
    assert_eq!(cpu, original);
    assert_eq!(memory.bytes, bytes);

    memory.fail_write = false;
    memory.fail_read = true;
    let load = X86ScalarDecoder::decode(&[0x48, 0x0f, 0x38, 0xf0, 0x0e], cpu.rip).unwrap();
    let original = cpu.clone();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(_)
    ));
    assert_eq!(cpu, original);
}

#[test]
fn cmpxchg_register_aliases() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut success = CpuState {
        scalar: ScalarState {
            rip: 0x100,
            ..Default::default()
        },
        ..Default::default()
    };
    success.registers[0] = 7;
    success.registers[1] = 9;
    let instruction = X86ScalarDecoder::decode(&[0x0f, 0xb1, 0xc8], success.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut success, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!(success.registers[0], 9);
    assert!(success.flags.contains(Flag::Zero));

    let mut failure = CpuState {
        scalar: ScalarState {
            rip: 0x200,
            ..Default::default()
        },
        ..Default::default()
    };
    failure.registers[0] = 3;
    failure.registers[1] = 11;
    failure.registers[2] = 13;
    let failure_instruction = X86ScalarDecoder::decode(&[0x0f, 0xb1, 0xd1], failure.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut failure, &mut memory, failure_instruction),
        ExecutionExit::Continue
    );
    assert_eq!(failure.registers[0], 11);
    assert_eq!(failure.registers[1], 11);
    assert!(!failure.flags.contains(Flag::Zero));

    let mut byte = CpuState {
        scalar: ScalarState {
            rip: 0x240,
            ..Default::default()
        },
        ..Default::default()
    };
    byte.registers[0] = 0xaaaa_aaaa_aaaa_aa11;
    byte.registers[2] = 0xbbbb_bbbb_bbbb_bb11;
    byte.registers[3] = 0xcccc_cccc_cccc_cc44;
    let byte_instruction = X86ScalarDecoder::decode(&[0x0f, 0xb0, 0xda], byte.rip).unwrap();
    assert_eq!(byte_instruction.width, ScalarWidth::Byte);
    assert_eq!(
        ScalarInterpreter::execute(&mut byte, &mut memory, byte_instruction),
        ExecutionExit::Continue
    );
    assert_eq!(byte.registers[2], 0xbbbb_bbbb_bbbb_bb44);
    assert_eq!(byte.registers[0], 0xaaaa_aaaa_aaaa_aa11);
    assert!(byte.flags.contains(Flag::Zero));

    assert!(matches!(
        X86ScalarDecoder::decode(&[0x0f, 0xb0, 0xe0], 0).unwrap().instruction,
        ScalarInstruction::CompareExchange {
            source: ScalarRegister::Byte(ByteRegister::High(0)),
            ..
        }
    ));
    assert!(matches!(
        X86ScalarDecoder::decode(&[0x40, 0x0f, 0xb0, 0xe0], 0)
            .unwrap()
            .instruction,
        ScalarInstruction::CompareExchange {
            source: ScalarRegister::Byte(ByteRegister::Low(4)),
            ..
        }
    ));
}

#[test]
fn locked_cmpxchg_transaction() {
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: 5_u32.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let instruction = X86ScalarDecoder::decode(&[0xf0, 0x0f, 0xb1, 0x0b], 0x400).unwrap();
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x400,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[0] = 5;
    cpu.registers[1] = 8;
    cpu.registers[3] = 0x1000;
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!(memory.read(0x1000, 4).unwrap(), 8);
    assert_eq!(memory.commits, 1);
    assert!(cpu.flags.contains(Flag::Zero));

    cpu.rip = 0x400;
    cpu.registers[0] = 2;
    cpu.registers[1] = 13;
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Yield {
            instruction: 0x400,
            completed: 1,
        }
    );
    assert_eq!(cpu.registers[0], 8);
    assert_eq!(cpu.rip, 0x404);
    assert!(!cpu.flags.contains(Flag::Zero));
    assert_eq!(memory.read(0x1000, 4).unwrap(), 8);
    assert_eq!(memory.commits, 1);

    cpu.rip = 0x400;
    cpu.registers[0] = 8;
    let original = cpu.clone();
    memory.fail_write = true;
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::OperandFault(access) if access.length() == 4
    ));
    assert_eq!(cpu, original);
    assert_eq!(memory.read(0x1000, 4).unwrap(), 8);
}

#[test]
fn xchg_forms() {
    let high = X86ScalarDecoder::decode(&[0x86, 0xe0], 0).unwrap();
    assert_eq!(high.width, ScalarWidth::Byte);
    assert!(matches!(
        high.instruction,
        ScalarInstruction::Exchange {
            destination: ScalarOperand::Register(ScalarRegister::Byte(ByteRegister::Low(0))),
            source: ScalarRegister::Byte(ByteRegister::High(0)),
        }
    ));
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x87, 0xc8], 0).is_err());
    let byte_memory = X86ScalarDecoder::decode(&[0x40, 0x86, 0x33], 0x480).unwrap();
    assert_eq!(byte_memory.width, ScalarWidth::Byte);
    let mut byte_store = ModelMemory {
        base: 0x1000,
        bytes: 0x1122_3344_5566_77aa_u64.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut byte_cpu = CpuState {
        scalar: ScalarState {
            rip: 0x480,
            ..Default::default()
        },
        ..Default::default()
    };
    byte_cpu.registers[3] = 0x1000;
    byte_cpu.registers[6] = 0xbb;
    assert_eq!(
        ScalarInterpreter::execute(&mut byte_cpu, &mut byte_store, byte_memory),
        ExecutionExit::Continue
    );
    assert_eq!(byte_store.read(0x1000, 8).unwrap(), 0x1122_3344_5566_77bb);
    assert_eq!(byte_cpu.registers[6], 0xaa);
    let locked = X86ScalarDecoder::decode(&[0xf0, 0x48, 0x87, 0x0b], 0x500).unwrap();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: 7_u64.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x500,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[1] = 11;
    cpu.registers[3] = 0x1000;
    let flags = cpu.flags;
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, locked),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[1], 7);
    assert_eq!(memory.read(0x1000, 8).unwrap(), 11);
    assert_eq!(cpu.flags, flags);

    let dword = X86ScalarDecoder::decode(&[0x87, 0xc8], cpu.rip).unwrap();
    cpu.registers[0] = u64::MAX;
    cpu.registers[1] = 3;
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, dword),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 3);
    assert_eq!(cpu.registers[1], 0xffff_ffff);
}

#[test]
fn accumulator_xchg_matrix() {
    let encodings: &[(&[u8], ScalarWidth, u8, u64)] = &[
        (&[], ScalarWidth::Dword, 0, 0xffff_ffff),
        (&[0x66], ScalarWidth::Word, 0, 0xffff),
        (&[0x48], ScalarWidth::Qword, 0, u64::MAX),
        (&[0x41], ScalarWidth::Dword, 8, 0xffff_ffff),
        (&[0x49], ScalarWidth::Qword, 8, u64::MAX),
    ];
    for opcode in 0x91_u8..=0x97 {
        for (prefixes, width, extension, mask) in encodings {
            let mut bytes = prefixes.to_vec();
            bytes.push(opcode);
            let source = (opcode & 7) | extension;
            let instruction = X86ScalarDecoder::decode(&bytes, 0x100).unwrap();
            assert_eq!(instruction.width, *width);
            assert_eq!(
                instruction.instruction,
                ScalarInstruction::AccumulatorExchange {
                    source: ScalarRegister::General(source),
                }
            );
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x100,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.registers[0] = 0xaaaa_bbbb_cccc_1111;
            cpu.registers[usize::from(source)] = 0xdddd_eeee_ffff_2222;
            cpu.flags = FlagState::from_bits(u16::MAX);
            let flags = cpu.flags;
            let mut memory = ModelMemory {
                base: 0,
                bytes: Vec::new(),
                fail_read: true,
                fail_write: true,
                commits: 0,
            };
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
                ExecutionExit::Continue
            );
            assert_eq!(cpu.registers[0] & mask, 0xdddd_eeee_ffff_2222 & mask);
            assert_eq!(cpu.registers[usize::from(source)] & mask, 0xaaaa_bbbb_cccc_1111 & mask);
            assert_eq!(cpu.flags, flags);
        }
    }
}

#[test]
fn ah_flag_transfer() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x100,
            flags: FlagState::from_bits((1 << Flag::Overflow as u8) | (1 << Flag::Carry as u8)),
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[0] = 0x1122_3344_5566_8000;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: true,
        fail_write: true,
        commits: 0,
    };

    let sahf = X86ScalarDecoder::decode(&[0x9e], cpu.rip).unwrap();
    assert_eq!(sahf.instruction, ScalarInstruction::FlagsFromAh);
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, sahf),
        ExecutionExit::Continue
    );
    assert!(cpu.flags.contains(Flag::Sign));
    assert!(!cpu.flags.contains(Flag::Zero));
    assert!(!cpu.flags.contains(Flag::Auxiliary));
    assert!(!cpu.flags.contains(Flag::Parity));
    assert!(!cpu.flags.contains(Flag::Carry));
    assert!(cpu.flags.contains(Flag::Overflow));

    cpu.flags = FlagState::from_bits(
        (1 << Flag::Sign as u8)
            | (1 << Flag::Auxiliary as u8)
            | (1 << Flag::Parity as u8)
            | (1 << Flag::Overflow as u8),
    );
    let flags = cpu.flags;
    let rax = cpu.registers[0];
    let lahf = X86ScalarDecoder::decode(&[0x9f], cpu.rip).unwrap();
    assert_eq!(lahf.instruction, ScalarInstruction::AhFromFlags);
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, lahf),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], (rax & !0xff00) | 0x9600);
    assert_eq!(cpu.flags, flags);
    assert_eq!(memory.commits, 0);
}

#[test]
fn flag_transfer_prefixes() {
    for opcode in [0x9e, 0x9f] {
        for prefix in [0xf0, 0xf2, 0xf3, 0x64, 0x65] {
            assert!(X86ScalarDecoder::decode(&[prefix, opcode], 0).is_err());
        }
        for prefix in [0x66, 0x67, 0x40, 0x48] {
            assert!(X86ScalarDecoder::decode(&[prefix, opcode], 0).is_ok());
        }
    }
}

#[test]
fn stack_flag_transfer() {
    for (bytes, instruction, width) in [
        (&[0x9c][..], ScalarInstruction::PushFlags, ScalarWidth::Qword),
        (&[0x66, 0x9c], ScalarInstruction::PushFlags, ScalarWidth::Word),
        (&[0x66, 0x48, 0x9c], ScalarInstruction::PushFlags, ScalarWidth::Qword),
        (&[0x9d], ScalarInstruction::PopFlags, ScalarWidth::Qword),
        (&[0x66, 0x9d], ScalarInstruction::PopFlags, ScalarWidth::Word),
        (&[0x66, 0x48, 0x9d], ScalarInstruction::PopFlags, ScalarWidth::Qword),
    ] {
        let decoded = X86ScalarDecoder::decode(bytes, 0x100).unwrap();
        assert_eq!((decoded.instruction, decoded.width), (instruction, width));
    }
    for opcode in [0x9c, 0x9d] {
        assert!(X86ScalarDecoder::decode(&[0xf0, opcode], 0).is_err());
        for prefix in [0xf2, 0xf3, 0x64, 0x65, 0x67, 0x40] {
            assert!(X86ScalarDecoder::decode(&[prefix, opcode], 0).is_ok());
        }
    }

    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x100,
            flags: FlagState::from_bits(0x895),
            direction: true,
            alignment_check: true,
            id_flag: true,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[4] = 0x1010;
    let push = X86ScalarDecoder::decode(&[0x9c], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, push),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[4], 0x1008);
    assert_eq!(memory.read(0x1008, 8).unwrap(), 0x240e97);

    memory.bytes[8..16].copy_from_slice(&0xc45_u64.to_le_bytes());
    let pop = X86ScalarDecoder::decode(&[0x9d], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, pop),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[4], 0x1010);
    assert_eq!(cpu.flags.bits(), 0x845);
    assert!(cpu.direction);
    assert!(!cpu.alignment_check);
    assert!(!cpu.id_flag);

    memory.bytes[16..18].copy_from_slice(&0_u16.to_le_bytes());
    cpu.registers[4] = 0x1010;
    cpu.alignment_check = true;
    cpu.id_flag = true;
    let pop_word = X86ScalarDecoder::decode(&[0x66, 0x9d], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, pop_word),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[4], 0x1012);
    assert!(cpu.alignment_check);
    assert!(cpu.id_flag);
}

#[test]
fn stack_flag_faults() {
    for (opcode, read, write) in [(0x9c, false, true), (0x9d, true, false)] {
        let mut memory = ModelMemory {
            base: 0x1000,
            bytes: vec![0; 8],
            fail_read: read,
            fail_write: write,
            commits: 0,
        };
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x200,
                flags: FlagState::from_bits(0x8d5),
                direction: true,
                alignment_check: true,
                id_flag: true,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[4] = if opcode == 0x9c { 0x1008 } else { 0x1000 };
        let original = cpu.clone();
        let decoded = X86ScalarDecoder::decode(&[opcode], cpu.rip).unwrap();
        assert!(matches!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::OperandFault(_)
        ));
        assert_eq!(cpu, original);
        assert_eq!(memory.commits, 0);
    }
}

#[test]
fn flag_control() {
    for (opcode, operation) in [
        (0xf5, ControlFlag::ComplementCarry),
        (0xf8, ControlFlag::ClearCarry),
        (0xf9, ControlFlag::SetCarry),
        (0xfc, ControlFlag::ClearDirection),
        (0xfd, ControlFlag::SetDirection),
    ] {
        let decoded = X86ScalarDecoder::decode(&[opcode], 0x100).unwrap();
        assert_eq!(decoded.instruction, ScalarInstruction::FlagControl(operation));
        assert!(X86ScalarDecoder::decode(&[0xf0, opcode], 0).is_err());
        for prefix in [0xf2, 0xf3, 0x64, 0x65, 0x66, 0x67, 0x40, 0x48] {
            assert!(X86ScalarDecoder::decode(&[prefix, opcode], 0).is_ok());
        }

        for carry in [false, true] {
            for direction in [false, true] {
                let mut cpu = CpuState {
                    scalar: ScalarState {
                        rip: 0x100,
                        flags: FlagState::from_bits(0x8d4).with(Flag::Carry, carry),
                        direction,
                        id_flag: true,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                cpu.registers[0] = u64::MAX;
                let original = cpu.clone();
                let mut memory = ModelMemory {
                    base: 0,
                    bytes: Vec::new(),
                    fail_read: true,
                    fail_write: true,
                    commits: 0,
                };
                assert_eq!(
                    ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
                    ExecutionExit::Continue
                );
                assert_eq!(cpu.rip, 0x101);
                assert_eq!(cpu.registers, original.registers);
                assert_eq!(cpu.id_flag, original.id_flag);
                match operation {
                    ControlFlag::ComplementCarry => {
                        assert_eq!(cpu.flags.contains(Flag::Carry), !carry);
                        assert_eq!(cpu.direction, direction);
                    }
                    ControlFlag::ClearCarry | ControlFlag::SetCarry => {
                        assert_eq!(cpu.flags.contains(Flag::Carry), operation == ControlFlag::SetCarry);
                        assert_eq!(cpu.direction, direction);
                    }
                    ControlFlag::ClearDirection | ControlFlag::SetDirection => {
                        assert_eq!(cpu.flags, original.flags);
                        assert_eq!(cpu.direction, operation == ControlFlag::SetDirection);
                    }
                }
                assert_eq!(
                    cpu.flags.bits() & !(1 << Flag::Carry as u8),
                    original.flags.bits() & !(1 << Flag::Carry as u8)
                );
                assert_eq!(memory.commits, 0);
            }
        }
    }
}

#[test]
fn accumulator_xchg_prefixes() {
    for bytes in [
        &[0x90][..],
        &[0x48, 0x90][..],
        &[0xf3, 0x90][..],
        &[0x66, 0xf3, 0x90][..],
        &[0x64, 0x90][..],
    ] {
        assert_eq!(
            X86ScalarDecoder::decode(bytes, 0).unwrap().instruction,
            ScalarInstruction::Nop
        );
    }
    assert!(matches!(
        X86ScalarDecoder::decode(&[0xf3, 0x91], 0).unwrap().instruction,
        ScalarInstruction::AccumulatorExchange {
            source: ScalarRegister::General(1)
        }
    ));
    assert!(matches!(
        X86ScalarDecoder::decode(&[0x41, 0x90], 0).unwrap().instruction,
        ScalarInstruction::AccumulatorExchange {
            source: ScalarRegister::General(8)
        }
    ));
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x91], 0).is_err());
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x100,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[0] = u64::MAX;
    cpu.registers[1] = 0xaaaa_bbbb_1234_5678;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let instruction = X86ScalarDecoder::decode(&[0x91], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 0x1234_5678);
    assert_eq!(cpu.registers[1], 0xffff_ffff);
}

#[test]
fn xadd_flags() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0xc00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[0] = 1;
    cpu.registers[1] = 0x7f;
    let byte = X86ScalarDecoder::decode(&[0x0f, 0xc0, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, byte),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[1] & 0xff, 0x80);
    assert_eq!(cpu.registers[0] & 0xff, 0x7f);
    assert!(cpu.flags.contains(Flag::Overflow));
    assert!(cpu.flags.contains(Flag::Sign));
    assert!(cpu.flags.contains(Flag::Auxiliary));
    assert!(!cpu.flags.contains(Flag::Carry));
    assert!(!cpu.flags.contains(Flag::Zero));
    assert!(!cpu.flags.contains(Flag::Parity));

    cpu.rip = 0xd00;
    cpu.registers[0] = 1;
    cpu.registers[1] = 0xff;
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, byte),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[1] & 0xff, 0);
    assert!(cpu.flags.contains(Flag::Carry));
    assert!(cpu.flags.contains(Flag::Zero));
    assert!(cpu.flags.contains(Flag::Parity));
    assert!(!cpu.flags.contains(Flag::Overflow));
}

#[test]
fn locked_xadd_transaction() {
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: 5_u64.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0xe00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[1] = 7;
    cpu.registers[3] = 0x1000;
    let instruction = X86ScalarDecoder::decode(&[0xf0, 0x48, 0x0f, 0xc1, 0x0b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!(memory.read(0x1000, 8).unwrap(), 12);
    assert_eq!(cpu.registers[1], 5);
    cpu.rip = 0xe00;
    let original = cpu.clone();
    memory.fail_write = true;
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert_eq!(memory.read(0x1000, 8).unwrap(), 12);
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x0f, 0xc1, 0xc1], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0xc1, 0xc1], 0).is_err());
}

#[test]
fn scalar_interpreter_executes() {
    let mut cpu = CpuState::default();
    cpu.rip = 0x100;
    cpu.registers[0] = 5;
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 64],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let add = X86ScalarDecoder::decode(&[0x48, 0x83, 0xc0, 3], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, add),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.registers[0], cpu.rip), (8, 0x104));
    let branch = ScalarIr {
        length: 2,
        width: ScalarWidth::Dword,
        instruction: ScalarInstruction::JumpConditional {
            condition: BranchCondition(4),
            target: 0x222,
        },
    };
    cpu.flags = FlagState::from_bits(1 << 6);
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, branch),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.rip, 0x222);
}

#[test]
fn scalar_interpreter_memory() {
    let mut cpu = CpuState::default();
    cpu.rip = 0x200;
    cpu.registers[0] = 0x1122_3344;
    cpu.registers[3] = 0x1008;
    cpu.registers[4] = 0x1040;
    let original_cpu = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0xaa; 72],
        fail_read: false,
        fail_write: true,
        commits: 0,
    };
    let store = X86ScalarDecoder::decode(&[0x89, 0x03], cpu.rip).unwrap();
    let before = memory.bytes.clone();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::OperandFault(access) if access.length() == 4
    ));
    assert_eq!(cpu, original_cpu);
    assert_eq!(memory.bytes, before);
    memory.fail_write = false;
    let store = X86ScalarDecoder::decode(&[0x89, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(memory.commits, 1);

    let call = ScalarIr {
        length: 5,
        width: ScalarWidth::Qword,
        instruction: ScalarInstruction::Call { target: 0x300 },
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, call),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.rip, cpu.registers[4], memory.commits), (0x300, 0x1038, 2));
    assert_eq!(memory.read(0x1038, 8).unwrap(), 0x207);
}

#[test]
fn scalar_interpreter_faults() {
    let mut cpu = CpuState::default();
    cpu.rip = 0x400;
    cpu.registers[3] = 0x0000_8000_0000_0000;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x8b, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::NonCanonical {
            access: AccessKind::Read,
            ..
        }
    ));
    assert_eq!(cpu, original);
    let syscall = X86ScalarDecoder::decode(&[0x0f, 0x05], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, syscall),
        ExecutionExit::Syscall {
            instruction: 0x400,
            next: 0x402
        }
    );
    assert_eq!(cpu, original);
    let undefined = X86ScalarDecoder::decode(&[0x0f, 0x0b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, undefined),
        ExecutionExit::UndefinedInstruction { instruction: 0x400 }
    );
    assert_eq!(cpu, original);
}

#[test]
fn scalar_interpreter_handles() {
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    memory.bytes[..8].copy_from_slice(&0xfeed_u64.to_le_bytes());
    let mut cpu = CpuState::default();
    cpu.rip = 0x500;
    cpu.registers[4] = 0x1000;
    let pop_alias = ScalarIr {
        length: 2,
        width: ScalarWidth::Qword,
        instruction: ScalarInstruction::Pop {
            destination: ScalarOperand::Memory(EffectiveAddress {
                base: Some(4),
                ..EffectiveAddress::default()
            }),
        },
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, pop_alias),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[4], 0x1008);
    assert_eq!(memory.read(0x1008, 8).unwrap(), 0xfeed);
    assert_eq!(memory.commits, 1);

    let mut overflow_cpu = CpuState::default();
    overflow_cpu.rip = 0x600;
    overflow_cpu.registers[4] = 0;
    let original = overflow_cpu.clone();
    let push = ScalarIr {
        length: 1,
        width: ScalarWidth::Qword,
        instruction: ScalarInstruction::Push {
            source: ScalarOperand::Immediate(1),
        },
    };
    assert!(matches!(
        ScalarInterpreter::execute(&mut overflow_cpu, &mut memory, push),
        ExecutionExit::OperandFault(access) if matches!(access.fault(), MemoryFault {
                access: AccessKind::Write,
                ..
            }) && access.length() == 8
    ));
    assert_eq!(overflow_cpu, original);
}

#[test]
fn string_decoder_retains() {
    for (byte_opcode, wide_opcode, operation) in [
        (0xa4, 0xa5, StringOperation::Move),
        (0xaa, 0xab, StringOperation::Store),
        (0xac, 0xad, StringOperation::Load),
        (0xa6, 0xa7, StringOperation::Compare),
        (0xae, 0xaf, StringOperation::Scan),
    ] {
        let byte = X86ScalarDecoder::decode(&[byte_opcode], 0x100).unwrap();
        assert_eq!(byte.width, ScalarWidth::Byte);
        assert!(matches!(
            byte.instruction,
            ScalarInstruction::String(StringInstruction {
                operation: actual,
                repeat: RepeatCondition::None,
                ..
            }) if actual == operation
        ));
        for (prefix, width) in [
            (&[0x66][..], ScalarWidth::Word),
            (&[][..], ScalarWidth::Dword),
            (&[0x48][..], ScalarWidth::Qword),
        ] {
            let mut bytes = prefix.to_vec();
            bytes.push(wide_opcode);
            let wide = X86ScalarDecoder::decode(&bytes, 0x100).unwrap();
            assert_eq!(wide.width, width);
            assert!(matches!(
                wide.instruction,
                ScalarInstruction::String(StringInstruction {
                    operation: actual,
                    repeat: RepeatCondition::None,
                    ..
                }) if actual == operation
            ));
        }
    }
    let ir = X86ScalarDecoder::decode(&[0x64, 0x67, 0xf3, 0xa6], 0x100).unwrap();
    assert_eq!(
        ir.instruction,
        ScalarInstruction::String(StringInstruction {
            operation: StringOperation::Compare,
            repeat: RepeatCondition::WhileEqual,
            address_32: true,
            source_segment: Some(Segment::Fs),
        })
    );
    let ir = X86ScalarDecoder::decode(&[0xf2, 0xae], 0x100).unwrap();
    assert!(matches!(
        ir.instruction,
        ScalarInstruction::String(StringInstruction {
            repeat: RepeatCondition::WhileNotEqual,
            ..
        })
    ));
    assert!(matches!(
        X86ScalarDecoder::decode(&[0xf2, 0xf3, 0xa6], 0x100)
            .unwrap()
            .instruction,
        ScalarInstruction::String(StringInstruction {
            repeat: RepeatCondition::WhileEqual,
            ..
        })
    ));
    assert!(matches!(
        X86ScalarDecoder::decode(&[0xf3, 0xf2, 0xa6], 0x100)
            .unwrap()
            .instruction,
        ScalarInstruction::String(StringInstruction {
            repeat: RepeatCondition::WhileNotEqual,
            ..
        })
    ));
}

#[test]
fn rep_movs_retains() {
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 0x1000],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    memory.bytes[0xffc..].copy_from_slice(b"ABCD");
    let mut cpu = CpuState::default();
    cpu.rip = 0x700;
    cpu.registers[1] = 8;
    cpu.registers[6] = 0x1ffc;
    cpu.registers[7] = 0x1000;
    let ir = X86ScalarDecoder::decode(&[0xf3, 0xa4], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute_with_budget(&mut cpu, &mut memory, ir, 32),
        ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x700,
                    address: 0x2000,
                    access: AccessKind::Read,
                },
                1
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!(
        (cpu.rip, cpu.registers[1], cpu.registers[6], cpu.registers[7]),
        (0x700, 4, 0x2000, 0x1004)
    );
    assert_eq!(&memory.bytes[..4], b"ABCD");
    assert_eq!(memory.commits, 4);

    memory.bytes.resize(0x1008, 0);
    memory.bytes[0x1000..0x1004].copy_from_slice(b"EFGH");
    assert_eq!(
        ScalarInterpreter::execute_with_budget(&mut cpu, &mut memory, ir, 32),
        ExecutionExit::Continue
    );
    assert_eq!(
        (cpu.rip, cpu.registers[1], cpu.registers[6], cpu.registers[7]),
        (0x702, 0, 0x2004, 0x1008)
    );
    assert_eq!(&memory.bytes[..8], b"ABCDEFGH");
    assert_eq!(memory.commits, 8);
}

#[test]
fn rep_movs_yields() {
    let ir = X86ScalarDecoder::decode(&[0xf3, 0xa4], 0x800).unwrap();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: b"abcdefgh".to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState::default();
    cpu.rip = 0x800;
    cpu.registers[1] = 6;
    cpu.registers[6] = 0x1000;
    cpu.registers[7] = 0x1002;
    assert_eq!(
        ScalarInterpreter::execute_with_budget(&mut cpu, &mut memory, ir, 3),
        ExecutionExit::Yield {
            instruction: 0x800,
            completed: 3,
        }
    );
    assert_eq!((cpu.rip, cpu.registers[1]), (0x800, 3));
    assert_eq!(
        ScalarInterpreter::execute_with_budget(&mut cpu, &mut memory, ir, 3),
        ExecutionExit::Continue
    );
    assert_eq!(&memory.bytes, b"abababab");
    assert_eq!(memory.commits, 6);

    memory.bytes.copy_from_slice(b"abcdefgh");
    memory.commits = 0;
    cpu.rip = 0x800;
    cpu.registers[1] = 6;
    cpu.registers[6] = 0x1005;
    cpu.registers[7] = 0x1007;
    cpu.direction = true;
    assert_eq!(
        ScalarInterpreter::execute_with_budget(&mut cpu, &mut memory, ir, 6),
        ExecutionExit::Continue
    );
    assert_eq!(&memory.bytes, b"ababcdef");
    assert_eq!(
        (cpu.registers[1], cpu.registers[6], cpu.registers[7]),
        (0, 0xfff, 0x1001)
    );
}

#[test]
fn rep_address_width() {
    let instruction = X86ScalarDecoder::decode(&[0x67, 0xf3, 0xaa], 0xb00).unwrap();
    let mut memory = ModelMemory {
        base: u64::from(u32::MAX),
        bytes: vec![0],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState::default();
    cpu.rip = 0xb00;
    cpu.registers[0] = 0x5a;
    cpu.registers[1] = 0xfeed_0000_0000_0001;
    cpu.registers[7] = 0xabcd_0000_ffff_ffff;

    assert_eq!(
        ScalarInterpreter::execute_with_budget(&mut cpu, &mut memory, instruction, 1),
        ExecutionExit::Continue
    );
    assert_eq!(memory.bytes, [0x5a]);
    assert_eq!(memory.commits, 1);
    assert_eq!(cpu.registers[1], 0);
    assert_eq!(cpu.registers[7], 0);

    memory.fail_write = true;
    cpu.rip = 0xb00;
    cpu.registers[1] = 0xfeed_0000_0000_0000;
    cpu.registers[7] = u64::MAX;
    let before = cpu.clone();

    assert_eq!(
        ScalarInterpreter::execute_with_budget(&mut cpu, &mut memory, instruction, 1),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.rip, before.rip + u64::from(instruction.length));
    assert_eq!(cpu.registers, before.registers);
    assert_eq!(memory.commits, 1);
}

#[test]
fn strings_cover_conditions() {
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    memory.bytes[..4].copy_from_slice(b"aaab");
    memory.bytes[8..12].copy_from_slice(b"aaac");
    let mut cpu = CpuState::default();
    cpu.rip = 0x900;
    cpu.registers[1] = 4;
    cpu.registers[6] = 0;
    cpu.registers[7] = 0x1008;
    cpu.fs_base = 0x1000;
    let compare = X86ScalarDecoder::decode(&[0x64, 0xf3, 0xa6], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute_with_budget(&mut cpu, &mut memory, compare, 8),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.registers[1], cpu.registers[6], cpu.registers[7]), (0, 4, 0x100c));
    assert!(!cpu.flags.contains(Flag::Zero));

    cpu.rip = 0xa00;
    cpu.registers[0] = u64::from(b'a');
    cpu.registers[1] = 4;
    cpu.registers[7] = 0x1000;
    let scan = X86ScalarDecoder::decode(&[0xf2, 0xae], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute_with_budget(&mut cpu, &mut memory, scan, 8),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.registers[1], cpu.registers[7]), (3, 0x1001));
    assert!(cpu.flags.contains(Flag::Zero));

    cpu.rip = 0xb00;
    cpu.registers[1] = 0xfeed_0000_0000_0000;
    let zero = X86ScalarDecoder::decode(&[0x67, 0xf3, 0xab], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute_with_budget(&mut cpu, &mut memory, zero, 0),
        ExecutionExit::Continue
    );
    assert_eq!(
        (cpu.rip, cpu.registers[1], memory.commits),
        (0xb03, 0xfeed_0000_0000_0000, 0)
    );

    let mut wrap_memory = ModelMemory {
        base: 0xffff_ffff,
        bytes: vec![0x7b],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    cpu.rip = 0xc00;
    cpu.registers[0] = 0;
    cpu.registers[1] = 0xaaaa_0000_0000_0001;
    cpu.registers[6] = 0xffff_ffff;
    cpu.direction = false;
    let load = X86ScalarDecoder::decode(&[0x67, 0xf3, 0xac], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute_with_budget(&mut cpu, &mut wrap_memory, load, 1),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.registers[0], cpu.registers[1], cpu.registers[6]), (0x7b, 0, 0));
}

#[test]
fn movd_moves_scalars() {
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    memory.bytes[..4].copy_from_slice(&0x89ab_cdef_u32.to_le_bytes());
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[5] = 0x106c;
    cpu.vectors[0] = u128::MAX;
    let load = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x6e, 0x45, 0x94], cpu.rip).unwrap();
    assert_eq!(load.width, ScalarWidth::Dword);
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.vectors[0], cpu.rip), (0x89ab_cdef, 0x4005));

    cpu.rip = 0x5000;
    cpu.registers[8] = 0xfedc_ba98_7654_3210;
    cpu.vectors[9] = u128::MAX;
    let wide = X86ScalarDecoder::decode(&[0x66, 0x4d, 0x0f, 0x6e, 0xc8], cpu.rip).unwrap();
    assert_eq!(wide.width, ScalarWidth::Qword);
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, wide),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[9], 0xfedc_ba98_7654_3210);

    cpu.rip = 0x6000;
    cpu.registers[8] = u64::MAX;
    cpu.vectors[9] = 0xaaaa_bbbb_cccc_dddd_1122_3344_5566_7788;
    let to_gpr = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x7e, 0xc8], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, to_gpr),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[8], 0x5566_7788);

    cpu.rip = 0x7000;
    cpu.registers[3] = 0x1008;
    cpu.vectors[0] = 0xaaaa_bbbb_cccc_dddd_0123_4567_89ab_cdef;
    let store = X86ScalarDecoder::decode(&[0x66, 0x48, 0x0f, 0x7e, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(memory.read(0x1008, 8).unwrap(), 0x0123_4567_89ab_cdef);
    assert!(matches!(
        X86ScalarDecoder::decode(&[0x0f, 0x6e, 0xc0], 0).unwrap().instruction,
        ScalarInstruction::MmxScalar { .. }
    ));
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x6e, 0xc0], 0).is_err());
}

#[test]
fn movd_fault_atomic() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x8000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[0] = u128::MAX;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0xaa; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x6e, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 4
    ));
    assert_eq!(cpu, original);

    memory.fail_read = false;
    memory.fail_write = true;
    let before = memory.bytes.clone();
    let store = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x7e, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::OperandFault(access) if access.length() == 4
    ));
    assert_eq!(cpu, original);
    assert_eq!(memory.bytes, before);
}

#[test]
fn unpack_orders_lanes() {
    let left = u128::from_le_bytes(std::array::from_fn(|index| index as u8));
    let right = u128::from_le_bytes(std::array::from_fn(|index| 0x80 + index as u8));
    for (opcode, lane, high) in [
        (0x60, 1_usize, false),
        (0x61, 2, false),
        (0x62, 4, false),
        (0x68, 1, true),
        (0x69, 2, true),
        (0x6a, 4, true),
        (0x6c, 8, false),
        (0x6d, 8, true),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x9000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[0] = left;
        cpu.vectors[1] = right;
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let ir = X86ScalarDecoder::decode(&[0x66, 0x0f, opcode, 0xc1], cpu.rip).unwrap();
        let base = if high { 8 } else { 0 };
        let mut expected = [0_u8; 16];
        for index in 0..(8 / lane) {
            let source = base + index * lane;
            let output = index * lane * 2;
            expected[output..output + lane].copy_from_slice(&left.to_le_bytes()[source..source + lane]);
            expected[output + lane..output + lane * 2].copy_from_slice(&right.to_le_bytes()[source..source + lane]);
        }
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[0], u128::from_le_bytes(expected));
        assert_eq!(cpu.rip, 0x9004);
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0xa000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[9] = left;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let alias = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x62, 0xc9], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, alias),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[9], 0x07060504_07060504_03020100_03020100);
}

#[test]
fn unpack_memory_faults() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0xb000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[8] = u128::MAX;
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: (0_u8..16).collect(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let ir = X86ScalarDecoder::decode(&[0x66, 0x44, 0x0f, 0x60, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8], 0x07ff06ff_05ff04ff_03ff02ff_01ff00ff);

    cpu.rip = 0xc000;
    let original = cpu.clone();
    memory.fail_read = true;
    let fault = X86ScalarDecoder::decode(&[0x66, 0x44, 0x0f, 0x60, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x62, 0xc0], 0).is_err());
}

#[test]
fn movq_stores_vectors() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0xd000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[8] = 0xaaaa_bbbb_cccc_dddd_0123_4567_89ab_cdef;
    cpu.vectors[1] = u128::MAX;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let register = X86ScalarDecoder::decode(&[0x66, 0x44, 0x0f, 0xd6, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, register),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.vectors[1], cpu.rip), (0x0123_4567_89ab_cdef, 0xd005));

    cpu.rip = 0xe000;
    cpu.vectors[0] = 0xfeed_face_cafe_beef_1122_3344_5566_7788;
    let alias = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xd6, 0xc0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, alias),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], 0x1122_3344_5566_7788);

    cpu.rip = 0xf000;
    memory.base = 0xf020;
    memory.bytes = vec![0; 8];
    let rip_store = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xd6, 0x0d, 0x18, 0, 0, 0], cpu.rip).unwrap();
    cpu.vectors[1] = 0x9999_aaaa_bbbb_cccc_dead_beef_0123_4567;
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, rip_store),
        ExecutionExit::Continue
    );
    assert_eq!(memory.read(0xf020, 8).unwrap(), 0xdead_beef_0123_4567);
    assert_eq!(cpu.rip, 0xf008);
    assert_eq!(
        X86ScalarDecoder::decode(&[0x0f, 0xd6, 0xc0], 0).unwrap().instruction,
        ScalarInstruction::Undefined
    );
    cpu.rip = 0x10_000;
    cpu.write_mmx(3, 0x0123_4567_89ab_cdef);
    cpu.vectors[7] = u128::MAX;
    let to_vector = X86ScalarDecoder::decode(&[0xf3, 0x0f, 0xd6, 0xfb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, to_vector),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[7], 0x0123_4567_89ab_cdef);

    cpu.rip = 0x10_100;
    cpu.vectors[5] = 0xffff_eeee_dddd_cccc_1716_1514_1312_1110;
    let to_mmx = X86ScalarDecoder::decode(&[0xf2, 0x0f, 0xd6, 0xd5], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, to_mmx),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.read_mmx(2), 0x1716_1514_1312_1110);

    assert_eq!(
        X86ScalarDecoder::decode(&[0xf3, 0x0f, 0xd6, 0x00], 0)
            .unwrap()
            .instruction,
        ScalarInstruction::Undefined
    );
    assert_eq!(
        X86ScalarDecoder::decode(&[0xf2, 0x0f, 0xd6, 0x00], 0)
            .unwrap()
            .instruction,
        ScalarInstruction::Undefined
    );
}

#[test]
fn movq_store_faults() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x11000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[0] = u128::MAX;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0xaa; 8],
        fail_read: false,
        fail_write: true,
        commits: 0,
    };
    let before = memory.bytes.clone();
    let store = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xd6, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert_eq!(memory.bytes, before);
}

#[test]
fn pextrw_selects_vector_register_file() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x11_100,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[1] = 0x7766_5544_3322_1100_ffee_ddcc_bbaa_9988;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let xmm = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xc5, 0xc1, 5], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, xmm),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 0x3322);

    cpu.rip = 0x11_180;
    cpu.registers[3] = 0x1000;
    memory.base = 0x1000;
    memory.bytes = vec![0xaa; 2];
    let memory_word = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x3a, 0x15, 0x0b, 3], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, memory_word),
        ExecutionExit::Continue
    );
    assert_eq!(memory.read(0x1000, 2).unwrap(), 0xffee);

    cpu.rip = 0x11_200;
    cpu.write_mmx(1, 0x7766_5544_3322_1100);
    let mmx = X86ScalarDecoder::decode(&[0x0f, 0xc5, 0xc1, 7], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, mmx),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 0x7766);
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0xc5, 0xc1, 0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0x66, 0x0f, 0xc5, 0x01, 0], 0).is_err());
}

#[test]
fn mmx_movemask_uses_eight_bytes() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x11_300,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.write_mmx(1, 0x80_7f_ff_00_81_01_7e_fe);
    cpu.registers[2] = u64::MAX;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let decoded = X86ScalarDecoder::decode(&[0x0f, 0xd7, 0xd1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[2], 0xa9);
    assert!(X86ScalarDecoder::decode(&[0x0f, 0xd7, 0x11], 0).is_err());

    cpu.rip = 0x11_400;
    cpu.registers[0] = 0x1234;
    cpu.write_mmx(0, 0x7766_5544_3322_1100);
    let insert = X86ScalarDecoder::decode(&[0x0f, 0xc4, 0xc0, 4], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, insert),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.read_mmx(0), 0x7766_5544_3322_1234);
}

#[test]
fn imul_truncates_results() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x12000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[1] = (-4_i64) as u64;
    cpu.registers[2] = 3;
    cpu.flags = FlagState::from_bits(1 << Flag::Auxiliary as u8);
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let two = X86ScalarDecoder::decode(&[0x48, 0x0f, 0xaf, 0xd1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, two),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[2], (-12_i64) as u64);
    assert!(cpu.flags.contains(Flag::Sign));
    assert!(cpu.flags.contains(Flag::Auxiliary));
    assert!(!cpu.flags.contains(Flag::Carry));
    assert!(!cpu.flags.contains(Flag::Overflow));

    cpu.rip = 0x13000;
    cpu.registers[1] = i64::MAX as u64;
    let full = X86ScalarDecoder::decode(&[0x48, 0x69, 0xc1, 2, 0, 0, 0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, full),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], u64::MAX - 1);
    assert!(cpu.flags.contains(Flag::Carry));
    assert!(cpu.flags.contains(Flag::Overflow));

    cpu.rip = 0x14000;
    cpu.registers[1] = 2;
    cpu.registers[2] = 0xaaaa_bbbb_cccc_0100;
    let short = X86ScalarDecoder::decode(&[0x66, 0x6b, 0xd1, 0x80], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, short),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[2], 0xaaaa_bbbb_cccc_ff00);
    assert!(!cpu.flags.contains(Flag::Carry));

    cpu.rip = 0x15000;
    cpu.registers[1] = 0x8000_0000;
    cpu.registers[0] = u64::MAX;
    let dword = X86ScalarDecoder::decode(&[0x6b, 0xc1, 1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, dword),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 0x8000_0000);
}

#[test]
fn imul_fault_atomic() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x16000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[0] = 7;
    cpu.registers[3] = 0x1000;
    cpu.flags = FlagState::from_bits(u16::MAX);
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![2; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let ir = X86ScalarDecoder::decode(&[0x48, 0x0f, 0xaf, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x48, 0x0f, 0xaf, 0xc0], 0).is_err());
}

#[test]
fn movq_loads_vectors() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x20000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[8] = 0xaaaa_bbbb_cccc_dddd_0123_4567_89ab_cdef;
    cpu.vectors[9] = u128::MAX;
    let mut memory = ModelMemory {
        base: 0x20020,
        bytes: vec![0; 8],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let register = X86ScalarDecoder::decode(&[0xf3, 0x45, 0x0f, 0x7e, 0xc8], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, register),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.vectors[9], cpu.rip), (0x0123_4567_89ab_cdef, 0x20005));

    cpu.rip = 0x21000;
    cpu.vectors[0] = 0xfeed_face_cafe_beef_1122_3344_5566_7788;
    let alias = X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x7e, 0xc0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, alias),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], 0x1122_3344_5566_7788);

    cpu.rip = 0x20000;
    memory.bytes.copy_from_slice(&0xdead_beef_7654_3210_u64.to_le_bytes());
    let rip_load = X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x7e, 0x05, 0x18, 0, 0, 0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, rip_load),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.vectors[0], cpu.rip), (0xdead_beef_7654_3210, 0x20008));
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x7e, 0xc0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0x66, 0xf3, 0x0f, 0x7e, 0xc0], 0).is_err());
}

#[test]
fn movq_load_faults() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x22000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[0] = u128::MAX;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0xaa; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x7e, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
}

#[test]
fn vector_bitwise_operates() {
    let left = 0xff00_ff00_aaaa_5555_0123_4567_89ab_cdef_u128;
    let right = 0x0ff0_f00f_3333_cccc_fedc_ba98_7654_3210_u128;
    for (opcode, expected) in [
        (0xdb, left & right),
        (0xdf, !left & right),
        (0xeb, left | right),
        (0xef, left ^ right),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x23000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[8] = left;
        cpu.vectors[9] = right;
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let ir = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, opcode, 0xc1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!((cpu.vectors[8], cpu.rip), (expected, 0x23005));
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x24000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = left;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let alias = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xdf, 0xc0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, alias),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], 0);
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0xdb, 0xc0], 0).is_err());
}

#[test]
fn vector_bitwise_faults() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x25000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[0] = u128::MAX;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: (0_u8..16).collect(),
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let ir = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xdb, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);

    memory.fail_read = false;
    let ir = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xef, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[0],
        !u128::from_le_bytes(std::array::from_fn(|index| index as u8))
    );
}

#[test]
fn andpd_forms() {
    let left = 0xfedc_ba98_7654_3210_ffff_0000_aaaa_5555_u128;
    let right = 0x0ff0_00ff_0f0f_f0f0_1234_5678_ffff_0000_u128;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    for destination in 0..16_u8 {
        for source in 0..16_u8 {
            let rex = 0x40 | ((destination >> 3) << 2) | (source >> 3);
            let bytes = [0x66, rex, 0x0f, 0x54, 0xc0 | ((destination & 7) << 3) | (source & 7)];
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x4d000,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.vectors[usize::from(destination)] = left;
            cpu.vectors[usize::from(source)] = right;
            let expected = cpu.vectors[usize::from(destination)] & cpu.vectors[usize::from(source)];
            let instruction = X86ScalarDecoder::decode(&bytes, cpu.rip).unwrap();
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
                ExecutionExit::Continue
            );
            assert_eq!(
                cpu.vectors[usize::from(destination)],
                expected,
                "destination={destination} source={source}"
            );
        }
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4d100,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[8] = left;
    let address = cpu.rip + 9 + 7;
    memory = ModelMemory {
        base: address,
        bytes: right.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let rip = X86ScalarDecoder::decode(&[0x66, 0x44, 0x0f, 0x54, 0x05, 7, 0, 0, 0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, rip),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8], left & right);

    cpu.rip = 0x4d200;
    cpu.registers[11] = 0x1000;
    cpu.registers[10] = 0;
    cpu.vectors[8] = left;
    memory = ModelMemory {
        base: 0x1003,
        bytes: right.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let sib = X86ScalarDecoder::decode(&[0x66, 0x47, 0x0f, 0x54, 0x44, 0x53, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, sib),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8], left & right);
}

#[test]
fn andpd_faults() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4d300,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1001;
    cpu.vectors[0] = u128::MAX;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: vec![0xaa; 8],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let instruction = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x54, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);
    for bytes in [
        &[0xf2, 0x0f, 0x54, 0xc0][..],
        &[0xf3, 0x0f, 0x54, 0xc0],
        &[0xf0, 0x66, 0x0f, 0x54, 0xc0],
    ] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_err());
    }
}

#[test]
fn float_bitwise_family() {
    let left = 0xfedc_ba98_7654_3210_ffff_0000_aaaa_5555_u128;
    let right = 0x0ff0_00ff_0f0f_f0f0_1234_5678_ffff_0000_u128;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    for prefix in [None, Some(0x66)] {
        for (opcode, expected) in [
            (0x54, left & right),
            (0x55, !left & right),
            (0x56, left | right),
            (0x57, left ^ right),
        ] {
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x4d400,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.vectors[8] = left;
            cpu.vectors[9] = right;
            cpu.flags = FlagState::from_bits(u16::MAX);
            cpu.mxcsr = 0xffff;
            let flags = cpu.flags;
            let mxcsr = cpu.mxcsr;
            let mut bytes: Vec<u8> = prefix.into_iter().collect();
            bytes.extend_from_slice(&[0x45, 0x0f, opcode, 0xc1]);
            let instruction = X86ScalarDecoder::decode(&bytes, cpu.rip).unwrap();
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
                ExecutionExit::Continue
            );
            assert_eq!(cpu.vectors[8], expected, "prefix={prefix:?} opcode={opcode:#x}");
            assert_eq!((cpu.flags, cpu.mxcsr), (flags, mxcsr));
        }
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4d500,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = left;
    let alias = X86ScalarDecoder::decode(&[0x0f, 0x55, 0xc0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, alias),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], 0);
}

#[test]
fn float_bitwise_segments() {
    let left = 0xfedc_ba98_7654_3210_ffff_0000_aaaa_5555_u128;
    let right = 0x0ff0_00ff_0f0f_f0f0_1234_5678_ffff_0000_u128;
    for prefix in [None, Some(0x66)] {
        for (segment, segment_base) in [(0x64, 0x2000), (0x65, 0x3000)] {
            for (opcode, expected) in [
                (0x54, left & right),
                (0x55, !left & right),
                (0x56, left | right),
                (0x57, left ^ right),
            ] {
                let mut cpu = CpuState {
                    scalar: ScalarState {
                        rip: 0x4d580,
                        fs_base: 0x2000,
                        gs_base: 0x3000,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                cpu.vectors[8] = left;
                cpu.vectors[9] = right;
                let mut bytes: Vec<u8> = [segment].into_iter().chain(prefix).collect();
                bytes.extend_from_slice(&[0x45, 0x0f, opcode, 0xc1]);
                let register = X86ScalarDecoder::decode(&bytes, cpu.rip).unwrap();
                let mut memory = ModelMemory {
                    base: 0,
                    bytes: vec![],
                    fail_read: false,
                    fail_write: false,
                    commits: 0,
                };
                assert_eq!(
                    ScalarInterpreter::execute(&mut cpu, &mut memory, register),
                    ExecutionExit::Continue
                );
                assert_eq!(cpu.vectors[8], expected);

                cpu.rip = 0x4d5c0;
                cpu.registers[3] = 0x40;
                cpu.vectors[8] = left;
                memory.base = segment_base + 0x40;
                memory.bytes = right.to_le_bytes().to_vec();
                bytes.truncate(usize::from(prefix.is_some()) + 1);
                bytes.extend_from_slice(&[0x44, 0x0f, opcode, 0x03]);
                let load = X86ScalarDecoder::decode(&bytes, cpu.rip).unwrap();
                assert_eq!(
                    ScalarInterpreter::execute(&mut cpu, &mut memory, load),
                    ExecutionExit::Continue
                );
                assert_eq!(cpu.vectors[8], expected);
            }
        }
    }
}

#[test]
fn aligned_moves_vectors() {
    let value = 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff_u128;
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x26000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[8] = value;
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let register = X86ScalarDecoder::decode(&[0x45, 0x0f, 0x28, 0xc8], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, register),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.vectors[9], cpu.rip), (value, 0x26004));

    cpu.rip = 0x27000;
    cpu.registers[3] = 0x1000;
    let store = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x29, 0x03], cpu.rip).unwrap();
    cpu.vectors[0] = value;
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(u128::from_le_bytes(memory.bytes[..16].try_into().unwrap()), value);
    assert_eq!(cpu.rip, 0x27004);

    cpu.rip = 0x28000;
    cpu.vectors[1] = 0;
    let load = X86ScalarDecoder::decode(&[0x0f, 0x28, 0x0b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], value);
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x28, 0xc0], 0).is_err());
}

#[test]
fn aligned_move_faults() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x29000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1001;
    cpu.vectors[0] = u128::MAX;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0xaa; 32],
        fail_read: true,
        fail_write: true,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x0f, 0x28, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::AlignmentFault {
            instruction: 0x29000,
            address: 0x1001,
            access: AccessKind::Read,
        }
    );
    assert_eq!(cpu, original);

    cpu.registers[3] = 0x1000;
    let original = cpu.clone();
    let before = memory.bytes.clone();
    let store = X86ScalarDecoder::decode(&[0x0f, 0x29, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);
    assert_eq!(memory.bytes, before);
}

#[test]
fn integer_vector_transports() {
    let value = 0x1021_3243_5465_7687_98a9_bacb_dced_fe0f_u128;
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x2a000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[14] = 0x1000;
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: value.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let dqa = X86ScalarDecoder::decode(&[0x66, 0x41, 0x0f, 0x6f, 0x06], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, dqa),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.vectors[0], cpu.rip), (value, 0x2a005));

    cpu.rip = 0x2b000;
    cpu.registers[14] = 0x1001;
    memory.base = 0x1001;
    let dqu = X86ScalarDecoder::decode(&[0xf3, 0x45, 0x0f, 0x6f, 0x06], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, dqu),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.vectors[8], cpu.rip), (value, 0x2b005));

    cpu.rip = 0x2c000;
    cpu.vectors[9] = !value;
    let store = X86ScalarDecoder::decode(&[0xf3, 0x45, 0x0f, 0x7f, 0x0e], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(u128::from_le_bytes(memory.bytes[..16].try_into().unwrap()), !value);

    cpu.rip = 0x2d000;
    let aligned = X86ScalarDecoder::decode(&[0x66, 0x41, 0x0f, 0x6f, 0x06], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, aligned),
        ExecutionExit::AlignmentFault { .. }
    ));
    assert!(matches!(
        X86ScalarDecoder::decode(&[0x0f, 0x6f, 0xc0], 0).unwrap().instruction,
        ScalarInstruction::MmxTransport { .. }
    ));
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x6f, 0xc0], 0).is_err());
}

#[test]
fn vector_byte_shifts() {
    let value = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100_u128;
    for (extension, count, expected) in [
        (3_u8, 8_u8, 0x0000_0000_0000_0000_0f0e_0d0c_0b0a_0908_u128),
        (7, 8, 0x0706_0504_0302_0100_0000_0000_0000_0000),
        (3, 16, 0),
        (7, 255, 0),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x2e000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[9] = value;
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let modrm = 0xc0 | (extension << 3) | 1;
        let ir = X86ScalarDecoder::decode(&[0x66, 0x41, 0x0f, 0x73, modrm, count], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!((cpu.vectors[9], cpu.rip), (expected, 0x2e006));
    }
    assert!(X86ScalarDecoder::decode(&[0x66, 0x0f, 0x73, 0x19, 8], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0x66, 0x0f, 0x73, 0xc1, 8], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x73, 0xd9, 8], 0).is_err());
}

struct ShiftReference;

impl ShiftReference {
    fn apply(value: u128, lane: u32, count: u8, extension: u8) -> u128 {
        let bits = lane * 8;
        let mask = (1_u128 << bits) - 1;
        let mut expected = 0;
        for index in 0..16 / lane {
            let offset = index * bits;
            expected |= Self::lane(value >> offset & mask, bits, count, extension) << offset;
        }
        expected
    }

    fn lane(source: u128, bits: u32, count: u8, extension: u8) -> u128 {
        let mask = (1_u128 << bits) - 1;
        if extension == 4 {
            let signed = ((source << (128 - bits)) as i128) >> (128 - bits);
            return (signed >> u32::from(count).min(bits - 1)) as u128 & mask;
        }
        if u32::from(count) >= bits {
            return 0;
        }
        if extension == 2 {
            source >> count
        } else {
            source << count & mask
        }
    }
}

#[test]
fn packed_shift_counts() {
    let value = 0x8000_0001_ffff_7fff_0123_4567_89ab_cdef_u128;
    for (opcode, extension) in [
        (0x71_u8, 2_u8),
        (0x71, 4),
        (0x71, 6),
        (0x72, 2),
        (0x72, 4),
        (0x72, 6),
        (0x73, 2),
        (0x73, 6),
    ] {
        let lane = 1_u32 << (opcode - 0x70);
        for count in 0_u8..=255 {
            let expected = ShiftReference::apply(value, lane, count, extension);
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x2f000,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.vectors[9] = value;
            cpu.flags = FlagState::from_bits(u16::MAX);
            let flags = cpu.flags;
            let modrm = 0xc0 | extension << 3 | 1;
            let instruction = X86ScalarDecoder::decode(&[0x66, 0x41, 0x0f, opcode, modrm, count], cpu.rip).unwrap();
            let mut memory = ModelMemory {
                base: 0,
                bytes: Vec::new(),
                fail_read: false,
                fail_write: false,
                commits: 0,
            };
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
                ExecutionExit::Continue
            );
            assert_eq!(cpu.vectors[9], expected);
            assert_eq!(cpu.flags, flags);
        }
    }
}

#[test]
fn packed_shift_forms() {
    for bytes in [
        &[0x66, 0x0f, 0x71, 0x10, 1][..],
        &[0x66, 0x0f, 0x71, 0xc0, 1],
        &[0x66, 0x0f, 0x71, 0xd8, 1],
        &[0x66, 0x0f, 0x73, 0xe0, 1],
        &[0xf2, 0x0f, 0x71, 0xd0, 1],
        &[0xf3, 0x0f, 0x71, 0xd0, 1],
        &[0xf0, 0x66, 0x0f, 0x71, 0xd0, 1],
    ] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_err());
    }
}

struct PackFixture;

impl PackFixture {
    fn words(values: [i16; 8]) -> u128 {
        let mut bytes = [0_u8; 16];
        for (index, value) in values.into_iter().enumerate() {
            bytes[index * 2..index * 2 + 2].copy_from_slice(&value.to_le_bytes());
        }
        u128::from_le_bytes(bytes)
    }

    fn dwords(values: [i32; 4]) -> u128 {
        let mut bytes = [0_u8; 16];
        for (index, value) in values.into_iter().enumerate() {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_le_bytes());
        }
        u128::from_le_bytes(bytes)
    }
}

#[test]
fn saturating_pack_boundaries() {
    let word_left = [-129, -128, -127, -1, 0, 126, 127, 128];
    let word_right = [i16::MIN, i16::MAX, -2, 1, 254, 255, 256, 300];
    let dword_left = [i32::MIN, -32_769, -32_768, -32_767];
    let dword_right = [32_766, 32_767, 32_768, i32::MAX];
    for (opcode, left, right, expected) in [
        (
            0x63_u8,
            PackFixture::words(word_left),
            PackFixture::words(word_right),
            u128::from_le_bytes([
                128, 128, 129, 255, 0, 126, 127, 127, 128, 127, 254, 1, 127, 127, 127, 127,
            ]),
        ),
        (
            0x67,
            PackFixture::words(word_left),
            PackFixture::words(word_right),
            u128::from_le_bytes([0, 0, 0, 0, 0, 126, 127, 128, 0, 255, 0, 1, 254, 255, 255, 255]),
        ),
        (
            0x6b,
            PackFixture::dwords(dword_left),
            PackFixture::dwords(dword_right),
            u128::from_le_bytes([0, 128, 0, 128, 0, 128, 1, 128, 254, 127, 255, 127, 255, 127, 255, 127]),
        ),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x2f800,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[0] = left;
        cpu.vectors[1] = right;
        cpu.flags = FlagState::from_bits(u16::MAX);
        let flags = cpu.flags;
        let instruction = X86ScalarDecoder::decode(&[0x66, 0x0f, opcode, 0xc1], cpu.rip).unwrap();
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[0], expected);
        assert_eq!(cpu.flags, flags);
    }
}

#[test]
fn saturating_pack_memory() {
    let left = PackFixture::words([-1, 0, 1, 2, 253, 254, 255, 256]);
    let right = PackFixture::words([-2, 3, 4, 5, 252, 300, i16::MIN, i16::MAX]);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x2f900,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[11] = 0x1000;
    cpu.vectors[8] = left;
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: right.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let instruction = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x67, 0x43, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[8],
        u128::from_le_bytes([0, 0, 1, 2, 253, 254, 255, 255, 0, 3, 4, 5, 252, 255, 0, 255])
    );

    cpu.rip = 0x2fa00;
    cpu.vectors[8] = left;
    let original = cpu.clone();
    memory.fail_read = true;
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);
}

#[test]
fn saturating_pack_forms() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x2fb00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = PackFixture::words([-1, 0, 1, 2, 3, 4, 255, 256]);
    let original = cpu.vectors[0];
    let alias = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x67, 0xc0], cpu.rip).unwrap();
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, alias),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u64, (cpu.vectors[0] >> 64) as u64);
    assert_ne!(cpu.vectors[0], original);
    for bytes in [
        &[0xf2, 0x0f, 0x67, 0xc1][..],
        &[0xf3, 0x0f, 0x67, 0xc1],
        &[0xf0, 0x66, 0x0f, 0x67, 0xc1],
    ] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_err());
    }
}

struct IncrementReference;

impl IncrementReference {
    fn byte(value: u64, decrement: bool) -> u64 {
        if decrement {
            value.wrapping_sub(1) & 0xff
        } else {
            value.wrapping_add(1) & 0xff
        }
    }
}

#[test]
fn increment_byte_exhaustive() {
    for decrement in [false, true] {
        let modrm = if decrement { 0xc8 } else { 0xc0 };
        let instruction = X86ScalarDecoder::decode(&[0xfe, modrm], 0x2fc00).unwrap();
        for value in 0_u64..=255 {
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x2fc00,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.registers[0] = value;
            cpu.flags = FlagState::from_bits((value as u16 & 1) << Flag::Carry as u8);
            let carry = cpu.flags.contains(Flag::Carry);
            let mut memory = ModelMemory {
                base: 0,
                bytes: Vec::new(),
                fail_read: false,
                fail_write: false,
                commits: 0,
            };
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
                ExecutionExit::Continue
            );
            let expected = IncrementReference::byte(value, decrement);
            assert_eq!(cpu.registers[0], expected);
            assert_eq!(cpu.flags.contains(Flag::Carry), carry);
        }
    }
}

#[test]
fn increment_width_flags() {
    for (bytes, initial, expected) in [
        (&[0xfe, 0xc0][..], 0x7f_u64, 0x80_u64),
        (&[0x66, 0xff, 0xc0][..], 0x7fff, 0x8000),
        (&[0xff, 0xc0][..], 0x7fff_ffff, 0x8000_0000),
        (&[0x48, 0xff, 0xc0][..], 0x7fff_ffff_ffff_ffff, 0x8000_0000_0000_0000),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x2fd00,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[0] = initial;
        cpu.flags = FlagState::from_bits(1 << Flag::Carry as u8);
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let instruction = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[0], expected);
        assert!(cpu.flags.contains(Flag::Carry));
        assert!(cpu.flags.contains(Flag::Overflow));
        assert!(cpu.flags.contains(Flag::Sign));
        assert!(cpu.flags.contains(Flag::Auxiliary));
        assert!(!cpu.flags.contains(Flag::Zero));
    }
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x2fe00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[0] = 0x8000_0000;
    cpu.flags = FlagState::from_bits(0);
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let decrement = X86ScalarDecoder::decode(&[0xff, 0xc8], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, decrement),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 0x7fff_ffff);
    assert!(cpu.flags.contains(Flag::Overflow));
    assert!(!cpu.flags.contains(Flag::Sign));
    assert!(cpu.flags.contains(Flag::Auxiliary));
}

#[test]
fn increment_byte_registers() {
    for (bytes, register, initial, expected) in [
        (&[0xfe, 0xc4][..], 0_usize, 0x7f00_u64, 0x8000_u64),
        (&[0x40, 0xfe, 0xc4][..], 4, 0x7f, 0x80),
        (&[0x41, 0xfe, 0xc4][..], 12, 0xff, 0),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x2ff00,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[register] = initial;
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let instruction = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[register], expected);
    }
}

#[test]
fn increment_memory_contract() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x30000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1001;
    cpu.flags = FlagState::from_bits(1 << Flag::Carry as u8);
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: u64::MAX.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let locked = X86ScalarDecoder::decode(&[0xf0, 0x48, 0xff, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, locked),
        ExecutionExit::Continue
    );
    assert_eq!(u64::from_le_bytes(memory.bytes[..8].try_into().unwrap()), 0);
    assert_eq!(memory.commits, 1);
    assert!(cpu.flags.contains(Flag::Carry));
    assert!(cpu.flags.contains(Flag::Zero));

    cpu.rip = 0x30100;
    let original = cpu.clone();
    let before = memory.bytes.clone();
    memory.fail_write = true;
    let locked_dec = X86ScalarDecoder::decode(&[0xf0, 0x48, 0xff, 0x0b], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, locked_dec),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert_eq!(memory.bytes, before);
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x48, 0xff, 0xc0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf2, 0xff, 0xc0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xfe, 0xd0], 0).is_err());
}

struct DoubleShiftReference;

impl DoubleShiftReference {
    fn opcode(right: bool) -> u8 {
        if right { 0xac } else { 0xa4 }
    }
    fn arithmetic(right: bool, width: IntegerWidth, value: u64, fill: u64, count: u8) -> Arithmetic {
        if right {
            Arithmetic::shift_right_double(width, value, fill, count)
        } else {
            Arithmetic::shift_left_double(width, value, fill, count)
        }
    }
}

#[test]
fn double_shift_counts() {
    for (prefix, width, integer, mask, bits, right) in [
        (
            &[0x66][..],
            ScalarWidth::Word,
            IntegerWidth::Word,
            0xffff_u64,
            16_u8,
            false,
        ),
        (&[0x66][..], ScalarWidth::Word, IntegerWidth::Word, 0xffff, 16, true),
        (&[][..], ScalarWidth::Dword, IntegerWidth::Dword, 0xffff_ffff, 32, false),
        (&[][..], ScalarWidth::Dword, IntegerWidth::Dword, 0xffff_ffff, 32, true),
        (
            &[0x48][..],
            ScalarWidth::Qword,
            IntegerWidth::Qword,
            u64::MAX,
            64,
            false,
        ),
        (&[0x48][..], ScalarWidth::Qword, IntegerWidth::Qword, u64::MAX, 64, true),
    ] {
        for count in [0_u8, 1, bits - 1, bits, bits.saturating_add(1), 31, 32, 63, 64, 255] {
            let mut bytes = prefix.to_vec();
            bytes.extend_from_slice(&[0x0f, DoubleShiftReference::opcode(right), 0xd0, count]);
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x30200,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.registers[0] = 0x8123_4567_89ab_cdef;
            cpu.registers[2] = 0xfedc_ba98_7654_3210;
            cpu.flags = FlagState::from_bits(u16::MAX);
            let flags = cpu.flags;
            let expected = DoubleShiftReference::arithmetic(right, integer, cpu.registers[0], cpu.registers[2], count);
            let instruction = X86ScalarDecoder::decode(&bytes, cpu.rip).unwrap();
            assert_eq!(instruction.width, width);
            let mut memory = ModelMemory {
                base: 0,
                bytes: Vec::new(),
                fail_read: false,
                fail_write: false,
                commits: 0,
            };
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
                ExecutionExit::Continue
            );
            assert_eq!(
                cpu.registers[0] & mask,
                expected.result,
                "width={width:?} right={right} count={count}"
            );
            assert_eq!(cpu.flags, flags.apply(expected.flags));
        }
    }
}

#[test]
fn double_shift_cl() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x30300,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[1] = 1;
    cpu.registers[8] = 0x8000_0000_0000_0000;
    cpu.registers[10] = 1;
    cpu.flags = FlagState::from_bits(0);
    let instruction = X86ScalarDecoder::decode(&[0x4d, 0x0f, 0xa5, 0xd0], cpu.rip).unwrap();
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[8], 0);
    assert!(cpu.flags.contains(Flag::Carry));
    assert!(cpu.flags.contains(Flag::Overflow));
}

#[test]
fn double_shift_memory() {
    let initial = 0x8123_4567_89ab_cdef_u64;
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x30400,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[11] = 0x1000;
    cpu.registers[10] = 0xfedc_ba98_7654_3210;
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: initial.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let instruction = X86ScalarDecoder::decode(&[0x4d, 0x0f, 0xac, 0x53, 0x01, 4], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue
    );
    let expected = Arithmetic::shift_right_double(IntegerWidth::Qword, initial, cpu.registers[10], 4);
    assert_eq!(
        u64::from_le_bytes(memory.bytes[..8].try_into().unwrap()),
        expected.result
    );

    cpu.rip = 0x30500;
    let original = cpu.clone();
    let before = memory.bytes.clone();
    memory.fail_write = true;
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert_eq!(memory.bytes, before);
    memory.fail_write = true;
    memory.fail_read = false;
    cpu.registers[1] = 0;
    let original = cpu.clone();
    let zero = X86ScalarDecoder::decode(&[0x4d, 0x0f, 0xad, 0x53, 0x01], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, zero),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert_eq!(memory.bytes, before);

    memory.fail_write = false;
    let commits = memory.commits;
    let flags = cpu.flags;
    let zero = X86ScalarDecoder::decode(&[0x4d, 0x0f, 0xad, 0x53, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, zero),
        ExecutionExit::Continue
    );
    assert_eq!(memory.bytes, before);
    assert_eq!(memory.commits, commits + 1);
    assert_eq!(cpu.flags, flags);

    for (encoded, cl) in [
        (&[0x66, 0x45, 0x0f, 0xa4, 0x53, 0x01, 17][..], None),
        (&[0x66, 0x45, 0x0f, 0xad, 0x53, 0x01][..], Some(31_u64)),
    ] {
        cpu.rip = cpu.rip.wrapping_add(0x100);
        if let Some(count) = cl {
            cpu.registers[1] = count;
        }
        let original = cpu.clone();
        let before = memory.bytes.clone();
        memory.fail_write = true;
        let preserved = X86ScalarDecoder::decode(encoded, cpu.rip).unwrap();
        assert!(matches!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, preserved),
            ExecutionExit::OperandFault(access) if access.length() == 2
        ));
        assert_eq!(cpu, original);
        assert_eq!(memory.bytes, before);

        memory.fail_write = false;
        let commits = memory.commits;
        let flags = cpu.flags;
        let preserved = X86ScalarDecoder::decode(encoded, cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, preserved),
            ExecutionExit::Continue
        );
        assert_eq!(memory.bytes, before);
        assert_eq!(memory.commits, commits + 1);
        assert_eq!(cpu.flags, flags);
    }
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x48, 0x0f, 0xa4, 0xc8, 1], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x48, 0x0f, 0xa4, 0xc8, 1], 0).is_err());
}

#[test]
fn x87_control_roundtrip() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x30600,
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(cpu.x87_control, 0x037f);
    cpu.registers[0] = 0x1000;
    cpu.flags = FlagState::from_bits(u16::MAX);
    cpu.vectors[0] = u128::MAX;
    let architectural = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: 0xffff_u16.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0xd9, 0x68, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.x87_control, 0x1f7f);
    assert_eq!(cpu.flags, architectural.flags);
    assert_eq!(cpu.vectors, architectural.vectors);

    cpu.rip = 0x30700;
    cpu.x87_control = 0x0b7f;
    memory.bytes.fill(0);
    let store = X86ScalarDecoder::decode(&[0xd9, 0x78, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(u16::from_le_bytes(memory.bytes[..2].try_into().unwrap()), 0x0b7f);
    cpu.rip = 0x30800;
    let wait = X86ScalarDecoder::decode(&[0x9b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, wait),
        ExecutionExit::Continue
    );
}

#[test]
fn x87_control_faults() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x30900,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[0] = 0x1001;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: vec![0; 2],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0xd9, 0x28], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 2
    ));
    assert_eq!(cpu, original);
    memory.fail_read = false;
    memory.fail_write = true;
    let store = X86ScalarDecoder::decode(&[0xd9, 0x38], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::OperandFault(access) if access.length() == 2
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xd9, 0xef], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf0, 0xd9, 0x38], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf2, 0xd9, 0x38], 0).is_err());
}

#[test]
fn x87_extended_roundtrip() {
    let indefinite = ExtendedReal::INDEFINITE.bits();
    let cases: [(u128, u128, ExtendedClass, u16); 12] = [
        (
            0x0000_0000_0000_0000_0000,
            0x0000_0000_0000_0000_0000,
            ExtendedClass::Zero,
            0,
        ),
        (
            0x8000_0000_0000_0000_0000,
            0x8000_0000_0000_0000_0000,
            ExtendedClass::Zero,
            0,
        ),
        (
            0x0000_0000_0000_0000_0001,
            0x0000_0000_0000_0000_0001,
            ExtendedClass::Denormal,
            2,
        ),
        (0x0000_8000_0000_0000_0000, indefinite, ExtendedClass::QuietNan, 1),
        (
            0x3fff_8000_0000_0000_0000,
            0x3fff_8000_0000_0000_0000,
            ExtendedClass::Normal,
            0,
        ),
        (
            0xbfff_ffff_ffff_ffff_ffff,
            0xbfff_ffff_ffff_ffff_ffff,
            ExtendedClass::Normal,
            0,
        ),
        (
            0x7fff_8000_0000_0000_0000,
            0x7fff_8000_0000_0000_0000,
            ExtendedClass::Infinity,
            0,
        ),
        (
            0xffff_8000_0000_0000_0000,
            0xffff_8000_0000_0000_0000,
            ExtendedClass::Infinity,
            0,
        ),
        (
            0x7fff_c000_0000_0000_0001,
            0x7fff_c000_0000_0000_0001,
            ExtendedClass::QuietNan,
            0,
        ),
        (
            0x7fff_8000_0000_0000_0001,
            0x7fff_c000_0000_0000_0001,
            ExtendedClass::QuietNan,
            1,
        ),
        (0x4000_0000_0000_0000_0001, indefinite, ExtendedClass::QuietNan, 1),
        (0x7fff_0000_0000_0000_0000, indefinite, ExtendedClass::QuietNan, 1),
    ];
    for (bits, expected, class, flags) in cases {
        let mut bytes = vec![0xaa; 32];
        bytes[..10].copy_from_slice(&bits.to_le_bytes()[..10]);
        let mut memory = ModelMemory {
            base: 0x1000,
            bytes,
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x410eac,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[5] = 0x1010;
        let load = X86ScalarDecoder::decode(&[0xdb, 0x6d, 0xf0], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, load),
            ExecutionExit::Continue
        );
        assert_eq!((cpu.x87_status >> 11) & 7, 7);
        assert_eq!(cpu.x87_values[7].bits(), expected);
        assert_eq!(cpu.x87_classes[7], class);
        assert_eq!(cpu.x87_status & 3, flags);

        cpu.registers[3] = 0x1000;
        let store = X86ScalarDecoder::decode(&[0xdb, 0x7b, 0x10], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, store),
            ExecutionExit::Continue
        );
        assert_eq!(&memory.bytes[16..26], &expected.to_le_bytes()[..10]);
        assert_eq!(cpu.x87_classes[7], ExtendedClass::Empty);
        assert_eq!((cpu.x87_status >> 11) & 7, 0);
    }
}

#[test]
fn x87_address_forms() {
    for raw_mod in 0..3_u8 {
        for rm in 0..8_u8 {
            let bytes = x87_encoding(raw_mod, rm, 5);
            let decoded = X86ScalarDecoder::decode(&bytes, 0x4000).unwrap();
            assert!(matches!(
                decoded.instruction,
                ScalarInstruction::X87Extended { load: true, .. }
            ));
            let bytes = x87_encoding(raw_mod, rm, 7);
            let decoded = X86ScalarDecoder::decode(&bytes, 0x4000).unwrap();
            assert!(matches!(
                decoded.instruction,
                ScalarInstruction::X87Extended { load: false, .. }
            ));
        }
    }
    for group in 0..8_u8 {
        let decoded = X86ScalarDecoder::decode(&[0xdb, group << 3], 0);
        assert_eq!(decoded.is_ok(), matches!(group, 0..=3 | 5 | 7), "group={group}");
    }
    for group in [5_u8, 7] {
        let register = X86ScalarDecoder::decode(&[0xdb, 0xc0 | group << 3], 0);
        assert_eq!(register.is_ok(), group == 5);
        assert!(X86ScalarDecoder::decode(&[0xf0, 0xdb, group << 3], 0).is_err());
    }
}

fn x87_encoding(raw_mod: u8, rm: u8, group: u8) -> Vec<u8> {
    float_encoding(0xdb, raw_mod, rm, group)
}

fn float_encoding(opcode: u8, raw_mod: u8, rm: u8, group: u8) -> Vec<u8> {
    let mut bytes = vec![opcode, raw_mod << 6 | group << 3 | rm];
    if rm == 4 {
        bytes.push(0x24);
    }
    if raw_mod == 0 && rm == 5 {
        bytes.extend_from_slice(&0_i32.to_le_bytes());
    }
    if raw_mod == 1 {
        bytes.push(0);
    }
    if raw_mod == 2 {
        bytes.extend_from_slice(&0_i32.to_le_bytes());
    }
    bytes
}

#[test]
fn x87_float_addresses() {
    for (opcode, format) in [(0xd9, FloatWidth::Single), (0xdd, FloatWidth::Double)] {
        for group in [0_u8, 2, 3] {
            check_float_group(opcode, format, group);
        }
        for group in 0..8_u8 {
            let decoded = X86ScalarDecoder::decode(&[opcode, group << 3], 0);
            let valid = matches!(group, 0 | 2 | 3)
                || opcode == 0xd9 && matches!(group, 4..=7)
                || opcode == 0xdd && matches!(group, 1 | 4 | 6 | 7);
            assert_eq!(decoded.is_ok(), valid, "opcode={opcode:x} group={group}");
            let register = X86ScalarDecoder::decode(&[opcode, 0xc0 | group << 3], 0);
            let register_valid = matches!(
                (opcode, group),
                (0xd9, 0 | 1 | 2 | 4 | 5 | 6 | 7) | (0xdd, 0 | 2 | 3 | 4 | 5)
            );
            assert_eq!(register.is_ok(), register_valid);
        }
    }
    for rex in [0x40, 0x41, 0x44, 0x45] {
        assert!(matches!(
            X86ScalarDecoder::decode(&[rex, 0xd9, 0x00], 0).unwrap().instruction,
            ScalarInstruction::X87Float {
                format: FloatWidth::Single,
                store: false,
                ..
            }
        ));
    }
}

fn check_float_group(opcode: u8, format: FloatWidth, group: u8) {
    for raw_mod in 0..3_u8 {
        for rm in 0..8_u8 {
            let bytes = float_encoding(opcode, raw_mod, rm, group);
            let decoded = X86ScalarDecoder::decode(&bytes, 0x4000).unwrap();
            assert!(matches!(decoded.instruction, ScalarInstruction::X87Float {
                format: actual, store, pop, ..
            } if actual == format && store == (group != 0) && pop == (group == 3)));
        }
    }
}

#[test]
fn x87_float_loads() {
    let cases = [
        (
            FloatWidth::Single,
            0_u64,
            0x0000_0000_0000_0000_0000_u128,
            ExtendedClass::Zero,
            0,
        ),
        (
            FloatWidth::Single,
            0x8000_0000,
            0x8000_0000_0000_0000_0000,
            ExtendedClass::Zero,
            0,
        ),
        (
            FloatWidth::Single,
            0x3f80_0000,
            0x3fff_8000_0000_0000_0000,
            ExtendedClass::Normal,
            0,
        ),
        (
            FloatWidth::Single,
            0x0000_0001,
            0x3f6a_8000_0000_0000_0000,
            ExtendedClass::Denormal,
            2,
        ),
        (
            FloatWidth::Single,
            0x7f80_0000,
            0x7fff_8000_0000_0000_0000,
            ExtendedClass::Infinity,
            0,
        ),
        (
            FloatWidth::Single,
            0x7fc0_0001,
            0x7fff_c000_0100_0000_0000,
            ExtendedClass::QuietNan,
            0,
        ),
        (
            FloatWidth::Single,
            0x7f80_0001,
            0x7fff_c000_0100_0000_0000,
            ExtendedClass::QuietNan,
            1,
        ),
        (
            FloatWidth::Double,
            0x3ff0_0000_0000_0000,
            0x3fff_8000_0000_0000_0000,
            ExtendedClass::Normal,
            0,
        ),
        (
            FloatWidth::Double,
            0x0000_0000_0000_0001,
            0x3bcd_8000_0000_0000_0000,
            ExtendedClass::Denormal,
            2,
        ),
        (
            FloatWidth::Double,
            0x7ff0_0000_0000_0000,
            0x7fff_8000_0000_0000_0000,
            ExtendedClass::Infinity,
            0,
        ),
        (
            FloatWidth::Double,
            0x7ff8_0000_0000_0001,
            0x7fff_c000_0000_0000_0800,
            ExtendedClass::QuietNan,
            0,
        ),
    ];
    for (format, bits, expected, class, flags) in cases {
        let bytes = match format {
            FloatWidth::Single => bits.to_le_bytes()[..4].to_vec(),
            FloatWidth::Double => bits.to_le_bytes().to_vec(),
        };
        let mut memory = ModelMemory {
            base: 0x1000,
            bytes,
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x46000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[3] = 0x1000;
        let opcode = if format == FloatWidth::Single { 0xd9 } else { 0xdd };
        let load = X86ScalarDecoder::decode(&[opcode, 0x03], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, load),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.x87_values[7].bits(), expected, "format={format:?} bits={bits:x}");
        assert_eq!(cpu.x87_classes[7], class);
        assert_eq!(cpu.x87_status & 3, flags);
    }
}

#[test]
fn x87_float_rounding() {
    let halfway = ExtendedReal::from_bits(0x3fff_8000_0080_0000_0000);
    for (rounding, expected) in [
        (0_u16, 0x3f80_0000),
        (1, 0x3f80_0000),
        (2, 0x3f80_0001),
        (3, 0x3f80_0000),
    ] {
        let (cpu, memory) = float_store(halfway, ExtendedClass::Normal, rounding, FloatWidth::Single);
        assert_eq!(u32::from_le_bytes(memory.bytes[..4].try_into().unwrap()), expected);
        assert_eq!(cpu.x87_status & 0x20, 0x20);
        assert_eq!(cpu.x87_classes[0], ExtendedClass::Normal);
    }
    let negative = ExtendedReal::from_bits(halfway.bits() | 1_u128 << 79);
    for (rounding, expected) in [
        (0_u16, 0xbf80_0000),
        (1, 0xbf80_0001),
        (2, 0xbf80_0000),
        (3, 0xbf80_0000),
    ] {
        let (_, memory) = float_store(negative, ExtendedClass::Normal, rounding, FloatWidth::Single);
        assert_eq!(u32::from_le_bytes(memory.bytes[..4].try_into().unwrap()), expected);
    }

    let tiny = ExtendedReal::from_bits((u128::from((16383 - 150) as u16) << 64) | 1_u128 << 63);
    for (rounding, expected) in [(0_u16, 0), (1, 0), (2, 1), (3, 0)] {
        let (cpu, memory) = float_store(tiny, ExtendedClass::Normal, rounding, FloatWidth::Single);
        assert_eq!(u32::from_le_bytes(memory.bytes[..4].try_into().unwrap()), expected);
        assert_eq!(cpu.x87_status & 0x30, 0x30);
    }
    let huge = ExtendedReal::from_bits((u128::from((16383 + 128) as u16) << 64) | 1_u128 << 63);
    for (rounding, expected) in [
        (0_u16, 0x7f80_0000),
        (1, 0x7f7f_ffff),
        (2, 0x7f80_0000),
        (3, 0x7f7f_ffff),
    ] {
        let (cpu, memory) = float_store(huge, ExtendedClass::Normal, rounding, FloatWidth::Single);
        assert_eq!(u32::from_le_bytes(memory.bytes[..4].try_into().unwrap()), expected);
        assert_eq!(cpu.x87_status & 0x28, 0x28);
    }
}

fn float_store(
    value: ExtendedReal,
    class: ExtendedClass,
    rounding: u16,
    format: FloatWidth,
) -> (CpuState, ModelMemory) {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x47000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.x87_values[0] = value;
    cpu.x87_classes[0] = class;
    cpu.x87_control = (cpu.x87_control & !(3 << 10)) | rounding << 10;
    let bytes = if format == FloatWidth::Single { 4 } else { 8 };
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0xaa; bytes],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let opcode = if format == FloatWidth::Single { 0xd9 } else { 0xdd };
    let store = X86ScalarDecoder::decode(&[opcode, 0x13], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    (cpu, memory)
}

#[test]
fn x87_float_contract() {
    let one = ExtendedReal::from_bits(0x3fff_8000_0000_0000_0000);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x48000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.x87_values[0] = one;
    cpu.x87_classes[0] = ExtendedClass::Normal;
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0xaa; 8],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let store = X86ScalarDecoder::decode(&[0xdd, 0x13], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(
        u64::from_le_bytes(memory.bytes[..8].try_into().unwrap()),
        1_f64.to_bits()
    );
    assert_eq!(cpu.x87_classes[0], ExtendedClass::Normal);
    cpu.rip = 0x48100;
    let pop = X86ScalarDecoder::decode(&[0xdd, 0x1b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, pop),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.x87_classes[0], ExtendedClass::Empty);
    assert_eq!((cpu.x87_status >> 11) & 7, 1);

    let halfway = ExtendedReal::from_bits(0x3fff_8000_0080_0000_0000);
    let mut blocked = CpuState {
        scalar: ScalarState {
            rip: 0x48200,
            ..Default::default()
        },
        ..Default::default()
    };
    blocked.registers[3] = 0x1000;
    blocked.x87_values[0] = halfway;
    blocked.x87_classes[0] = ExtendedClass::Normal;
    blocked.x87_control &= !(1 << 5);
    memory.bytes.fill(0xaa);
    let single_pop = X86ScalarDecoder::decode(&[0xd9, 0x1b], blocked.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut blocked, &mut memory, single_pop),
        ExecutionExit::UndefinedInstruction { instruction: 0x48200 }
    );
    assert_eq!(blocked.x87_classes[0], ExtendedClass::Normal);
    assert_eq!(blocked.x87_status & 0x80a0, 0x80a0);
    assert!(memory.bytes.iter().all(|byte| *byte == 0xaa));

    let mut faulted = CpuState {
        scalar: ScalarState {
            rip: 0x48300,
            ..Default::default()
        },
        ..Default::default()
    };
    faulted.registers[3] = 0x1000;
    faulted.x87_values[0] = halfway;
    faulted.x87_classes[0] = ExtendedClass::Normal;
    let original = faulted.clone();
    memory.fail_write = true;
    assert!(matches!(
        ScalarInterpreter::execute(&mut faulted, &mut memory, single_pop),
        ExecutionExit::OperandFault(access) if access.length() == 4
    ));
    assert_eq!(faulted, original);
    memory.fail_write = false;
    memory.fail_read = true;
    let load = X86ScalarDecoder::decode(&[0xd9, 0x03], faulted.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut faulted, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 4
    ));
    assert_eq!(faulted, original);

    for (class, value, expected, flag) in [
        (
            ExtendedClass::QuietNan,
            ExtendedReal::from_bits(0x7fff_c000_0000_0000_0800),
            0x7fc0_0000_u32,
            0_u16,
        ),
        (
            ExtendedClass::SignalingNan,
            ExtendedReal::from_bits(0x7fff_8000_0000_0000_0800),
            0xffc0_0000,
            1,
        ),
        (
            ExtendedClass::Unsupported,
            ExtendedReal::from_bits(0x4000_0000_0000_0000_0001),
            0xffc0_0000,
            1,
        ),
    ] {
        let (cpu, memory) = float_store(value, class, 0, FloatWidth::Single);
        assert_eq!(u32::from_le_bytes(memory.bytes[..4].try_into().unwrap()), expected);
        assert_eq!(cpu.x87_status & 1, flag);
    }

    let double_half = ExtendedReal::from_bits(0x3fff_8000_0000_0000_0400);
    let (cpu, memory) = float_store(double_half, ExtendedClass::Normal, 0, FloatWidth::Double);
    assert_eq!(
        u64::from_le_bytes(memory.bytes[..8].try_into().unwrap()),
        1_f64.to_bits()
    );
    assert_eq!(cpu.x87_status & 0x20, 0x20);

    let exact_tiny = ExtendedReal::from_bits((u128::from((16383 - 149) as u16) << 64) | 1_u128 << 63);
    let (cpu, memory) = float_store(exact_tiny, ExtendedClass::Normal, 0, FloatWidth::Single);
    assert_eq!(u32::from_le_bytes(memory.bytes[..4].try_into().unwrap()), 1);
    assert_eq!(cpu.x87_status & 0x30, 0);
}

#[test]
fn x87_compare_decode() {
    for (opcode, pop) in [(0xdb, false), (0xdf, true)] {
        for (group, ordered) in [(5_u8, false), (6, true)] {
            check_compare_group(opcode, pop, group, ordered);
        }
    }
    for opcode in [0xdb, 0xdf] {
        for group in 0..8_u8 {
            let decoded = X86ScalarDecoder::decode(&[opcode, 0xc0 | group << 3], 0);
            assert_eq!(
                decoded.is_ok(),
                matches!(group, 5 | 6) || group == 4 && matches!(opcode, 0xdb | 0xdf) || opcode == 0xdb && group <= 3
            );
        }
        for prefix in [0xf0, 0xf2, 0xf3] {
            assert!(X86ScalarDecoder::decode(&[prefix, opcode, 0xe8], 0).is_err());
        }
    }
}

#[test]
fn x87_conditional_move_family() {
    for (opcode, negate) in [(0xda, false), (0xdb, true)] {
        for condition in 0..4_u8 {
            let ir = X86ScalarDecoder::decode(&[opcode, 0xc1 | condition << 3], 0x48f00).unwrap();
            assert_eq!(
                ir.instruction,
                ScalarInstruction::X87ConditionalMove {
                    source: 1,
                    condition,
                    negate
                }
            );
        }
    }

    for (opcode, flag, moves) in [
        (0xda, Flag::Carry, true),
        (0xda, Flag::Zero, true),
        (0xda, Flag::Parity, true),
        (0xdb, Flag::Carry, false),
        (0xdb, Flag::Zero, false),
        (0xdb, Flag::Parity, false),
    ] {
        let group = match flag {
            Flag::Carry => 0,
            Flag::Zero => 1,
            Flag::Parity => 3,
            _ => unreachable!(),
        };
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x48f00,
                flags: FlagState::default().with(flag, true),
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.x87_values[0] = ExtendedReal::from_bits(1);
        cpu.x87_values[1] = ExtendedReal::from_bits(2);
        cpu.x87_classes[0] = ExtendedClass::Normal;
        cpu.x87_classes[1] = ExtendedClass::QuietNan;
        let ir = X86ScalarDecoder::decode(&[opcode, 0xc1 | group << 3], cpu.rip).unwrap();
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.x87_values[0].bits(), if moves { 2 } else { 1 });
        assert_eq!(
            cpu.x87_classes[0],
            if moves {
                ExtendedClass::QuietNan
            } else {
                ExtendedClass::Normal
            }
        );
        assert_eq!(cpu.rip, 0x48f02);
    }
}

fn check_compare_group(opcode: u8, pop: bool, group: u8, ordered: bool) {
    for source in 0..8_u8 {
        for rex in [None, Some(0x40), Some(0x41), Some(0x44), Some(0x45)] {
            let mut bytes: Vec<u8> = rex.into_iter().collect();
            bytes.extend_from_slice(&[opcode, 0xc0 | group << 3 | source]);
            let ir = X86ScalarDecoder::decode(&bytes, 0x49000).unwrap();
            assert_eq!(ir.instruction, ScalarInstruction::X87Compare { source, ordered, pop });
        }
    }
}

#[test]
fn x87_compare_relations() {
    let values = [
        (0xbfff_8000_0000_0000_0000_u128, -1_i8),
        (0x8000_0000_0000_0000_0000, 0),
        (0x0000_0000_0000_0000_0000, 0),
        (0x3fff_8000_0000_0000_0000, 1),
        (0x4000_8000_0000_0000_0000, 2),
    ];
    for (left, left_order) in values {
        for (right, right_order) in values {
            compare_relation(left, left_order, right, right_order);
        }
    }
}

fn compare_relation(left: u128, left_order: i8, right: u128, right_order: i8) {
    let mut cpu = compare_cpu(
        ExtendedReal::from_bits(left),
        ExtendedClass::Normal,
        ExtendedReal::from_bits(right),
        ExtendedClass::Normal,
    );
    if left_order == 0 {
        cpu.x87_classes[5] = ExtendedClass::Zero;
    }
    if right_order == 0 {
        cpu.x87_classes[6] = ExtendedClass::Zero;
    }
    let original_other = cpu.flags.bits() & !0x8d5;
    let compare = X86ScalarDecoder::decode(&[0xdb, 0xe9], cpu.rip).unwrap();
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: true,
        fail_write: true,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, compare),
        ExecutionExit::Continue
    );
    let expected = if left_order < right_order {
        1
    } else if left_order == right_order {
        1 << 6
    } else {
        0
    };
    assert_eq!(cpu.flags.bits() & 0x8d5, expected);
    assert_eq!(cpu.flags.bits() & !0x8d5, original_other);
    assert_eq!((cpu.x87_status >> 11) & 7, 5);
    assert_eq!(
        cpu.x87_classes[5],
        if left_order == 0 {
            ExtendedClass::Zero
        } else {
            ExtendedClass::Normal
        }
    );
}

#[test]
fn x87_compare_nan() {
    let normal = ExtendedReal::from_bits(0x3fff_8000_0000_0000_0000);
    let quiet = ExtendedReal::from_bits(0x7fff_c000_0000_0000_0001);
    let signal = ExtendedReal::from_bits(0x7fff_8000_0000_0000_0001);
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: true,
        fail_write: true,
        commits: 0,
    };

    let mut unordered = compare_cpu(quiet, ExtendedClass::QuietNan, normal, ExtendedClass::Normal);
    let fucomi = X86ScalarDecoder::decode(&[0xdb, 0xe9], unordered.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut unordered, &mut memory, fucomi),
        ExecutionExit::Continue
    );
    assert_eq!(unordered.flags.bits() & 0x8d5, 0x45);
    assert_eq!(unordered.x87_status & 1, 0);

    for (value, class, ordered) in [
        (quiet, ExtendedClass::QuietNan, true),
        (signal, ExtendedClass::SignalingNan, false),
        (
            ExtendedReal::from_bits(0x4000_0000_0000_0000_0001),
            ExtendedClass::Unsupported,
            false,
        ),
    ] {
        let mut cpu = compare_cpu(value, class, normal, ExtendedClass::Normal);
        let opcode = if ordered { 0xf1 } else { 0xe9 };
        let compare = X86ScalarDecoder::decode(&[0xdf, opcode], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, compare),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.flags.bits() & 0x8d5, 0x45);
        assert_eq!(cpu.x87_status & 1, 1);
        assert_eq!((cpu.x87_status >> 11) & 7, 6);
        assert_eq!(cpu.x87_classes[5], ExtendedClass::Empty);
    }

    let mut blocked = compare_cpu(quiet, ExtendedClass::QuietNan, normal, ExtendedClass::Normal);
    blocked.x87_control &= !1;
    let flags = blocked.flags;
    let values = blocked.x87_values;
    let classes = blocked.x87_classes;
    let ordered_pop = X86ScalarDecoder::decode(&[0xdf, 0xf1], blocked.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut blocked, &mut memory, ordered_pop),
        ExecutionExit::UndefinedInstruction { instruction: 0x4a000 }
    );
    assert_eq!(blocked.flags, flags);
    assert_eq!(blocked.x87_values, values);
    assert_eq!(blocked.x87_classes, classes);
    assert_eq!((blocked.x87_status >> 11) & 7, 5);
    assert_eq!(blocked.x87_status & 0x8081, 0x8081);

    let mut empty = CpuState {
        scalar: ScalarState {
            rip: 0x4b000,
            ..Default::default()
        },
        ..Default::default()
    };
    let pop = X86ScalarDecoder::decode(&[0xdf, 0xe9], empty.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut empty, &mut memory, pop),
        ExecutionExit::Continue
    );
    assert_eq!(empty.flags.bits() & 0x8d5, 0x45);
    assert_eq!(empty.x87_status & 0x241, 0x41);
    assert_eq!((empty.x87_status >> 11) & 7, 1);
}

fn compare_cpu(
    left: ExtendedReal,
    left_class: ExtendedClass,
    right: ExtendedReal,
    right_class: ExtendedClass,
) -> CpuState {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4a000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.x87_status = 5 << 11;
    cpu.x87_values[5] = left;
    cpu.x87_classes[5] = left_class;
    cpu.x87_values[6] = right;
    cpu.x87_classes[6] = right_class;
    cpu.flags = FlagState::from_bits(0xffff);
    cpu
}

#[test]
fn x87_stack_decode() {
    let cases = [
        (0xd9, 0_u8, X87StackOperation::Load),
        (0xd9, 1, X87StackOperation::Exchange),
        (0xdd, 2, X87StackOperation::Store),
        (0xdd, 3, X87StackOperation::StorePop),
    ];
    for (opcode, group, operation) in cases {
        check_stack_group(opcode, group, operation);
    }
    for prefix in [0xf0, 0xf2, 0xf3] {
        for bytes in [
            [prefix, 0xd9, 0xc0],
            [prefix, 0xd9, 0xc8],
            [prefix, 0xdd, 0xd0],
            [prefix, 0xdd, 0xd8],
        ] {
            assert!(X86ScalarDecoder::decode(&bytes, 0).is_err());
        }
    }
}

#[test]
fn x87_environment_and_constants() {
    assert_eq!(
        X86ScalarDecoder::decode(&[0xdb, 0xe3], 0x51000).unwrap().instruction,
        ScalarInstruction::X87Initialize
    );
    assert_eq!(
        X86ScalarDecoder::decode(&[0xdf, 0xe0], 0x51000).unwrap().instruction,
        ScalarInstruction::X87Status
    );
    for constant in 0..7_u8 {
        assert_eq!(
            X86ScalarDecoder::decode(&[0xd9, 0xe8 + constant], 0x51000)
                .unwrap()
                .instruction,
            ScalarInstruction::X87Constant { constant }
        );
    }
    assert!(X86ScalarDecoder::decode(&[0xd9, 0xef], 0x51000).is_err());
    assert!(matches!(
        X86ScalarDecoder::decode(&[0xd9, 0x20], 0x51000).unwrap().instruction,
        ScalarInstruction::X87Environment { load: true, .. }
    ));
    assert!(matches!(
        X86ScalarDecoder::decode(&[0xd9, 0x30], 0x51000).unwrap().instruction,
        ScalarInstruction::X87Environment { load: false, .. }
    ));

    let values = std::array::from_fn(|index| ExtendedReal::from_bits(0x3fff_8000_0000_0000_0000 + index as u128));
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x51000,
            registers: [0x1234_5678_9abc_def0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
            ..Default::default()
        },
        x87_control: 0,
        x87_status: 0xffff,
        x87_values: values,
        x87_classes: [ExtendedClass::Normal; 8],
        ..Default::default()
    };
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: true,
        fail_write: true,
        commits: 0,
    };
    let init = X86ScalarDecoder::decode(&[0xdb, 0xe3], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, init),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.rip, 0x51002);
    assert_eq!(cpu.x87_control, 0x037f);
    assert_eq!(cpu.x87_status, 0);
    assert_eq!(cpu.x87_values, values);
    assert_eq!(cpu.x87_classes, [ExtendedClass::Empty; 8]);

    cpu.x87_status = 0x6543;
    let status = X86ScalarDecoder::decode(&[0xdf, 0xe0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, status),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 0x1234_5678_9abc_6543);

    let one = X86ScalarDecoder::decode(&[0xd9, 0xe8], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, one),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.x87_status >> 11) & 7, 3);
    assert_eq!(cpu.x87_values[3], ExtendedReal::from_bits(0x3fff_8000_0000_0000_0000));
    assert_eq!(cpu.x87_classes[3], ExtendedClass::Normal);

    cpu.x87_control = 0x0300;
    cpu.x87_status = 5 << 11 | 1 << 14;
    cpu.x87_classes = [
        ExtendedClass::Normal,
        ExtendedClass::Zero,
        ExtendedClass::Infinity,
        ExtendedClass::Empty,
        ExtendedClass::Empty,
        ExtendedClass::Empty,
        ExtendedClass::Empty,
        ExtendedClass::Empty,
    ];
    cpu.rip = 0x52000;
    memory.base = 0x52006;
    memory.bytes = vec![0xaa; 28];
    memory.fail_read = false;
    memory.fail_write = false;
    let store = X86ScalarDecoder::decode(&[0xd9, 0x35, 0, 0, 0, 0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(&memory.bytes[0..4], &0xffff_0300_u32.to_le_bytes());
    assert_eq!(&memory.bytes[4..8], &0xffff_6800_u32.to_le_bytes());
    assert_eq!(&memory.bytes[8..12], &0xffff_ffe4_u32.to_le_bytes());
    assert_eq!(&memory.bytes[24..28], &0xffff_0000_u32.to_le_bytes());
    assert_eq!(cpu.x87_control, 0x033f);

    cpu.x87_classes.fill(ExtendedClass::Empty);
    cpu.x87_control = 0;
    cpu.x87_status = 0;
    cpu.rip = 0x53000;
    memory.base = 0x53006;
    let load = X86ScalarDecoder::decode(&[0xd9, 0x25, 0, 0, 0, 0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.x87_control, 0x0340);
    assert_eq!(cpu.x87_status, 0x6800);
    assert_eq!(cpu.x87_classes[0], cpu.x87_values[0].class());
    assert_eq!(cpu.x87_classes[1], cpu.x87_values[1].class());
    assert_eq!(cpu.x87_classes[2], cpu.x87_values[2].class());
    assert_eq!(cpu.x87_classes[3..], [ExtendedClass::Empty; 5]);
}

#[test]
fn x87_arithmetic_family() {
    for opcode in [0xd8, 0xdc, 0xde] {
        for operation in [0_u8, 1, 4, 5, 6, 7] {
            assert!(matches!(
                X86ScalarDecoder::decode(&[opcode, 0xc1 | operation << 3], 0x54000)
                    .unwrap()
                    .instruction,
                ScalarInstruction::X87Arithmetic { address: None, operation: decoded, .. } if decoded == operation
            ));
        }
    }
    for (opcode, format) in [(0xd8, FloatWidth::Single), (0xdc, FloatWidth::Double)] {
        assert!(matches!(
            X86ScalarDecoder::decode(&[opcode, 0x00], 0x54000).unwrap().instruction,
            ScalarInstruction::X87Arithmetic { address: Some(_), operation: 0, format: decoded, .. } if decoded == format
        ));
    }

    let (one, normal) = crate::x86::real::Conversion::expand(1_f64.to_bits(), FloatWidth::Double);
    let (two, _) = crate::x86::real::Conversion::expand(2_f64.to_bits(), FloatWidth::Double);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x54000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.x87_status = 6 << 11;
    cpu.x87_values[6] = one;
    cpu.x87_values[7] = two;
    cpu.x87_classes[6] = normal;
    cpu.x87_classes[7] = normal;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: true,
        fail_write: true,
        commits: 0,
    };
    let add = X86ScalarDecoder::decode(&[0xd8, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, add),
        ExecutionExit::Continue
    );
    assert_eq!(
        crate::x86::real::Conversion::narrow(cpu.x87_values[6], cpu.x87_classes[6], FloatWidth::Double, 0).bits,
        3_f64.to_bits()
    );

    cpu.rip = 0x55000;
    let multiply_pop = X86ScalarDecoder::decode(&[0xde, 0xc9], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, multiply_pop),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.x87_status >> 11) & 7, 7);
    assert_eq!(
        crate::x86::real::Conversion::narrow(cpu.x87_values[7], cpu.x87_classes[7], FloatWidth::Double, 0).bits,
        6_f64.to_bits()
    );

    let (ten, _) = crate::x86::real::Conversion::expand(10_f64.to_bits(), FloatWidth::Double);
    cpu.rip = 0x56000;
    cpu.registers[0] = 0x7000;
    cpu.x87_status = 7 << 11;
    cpu.x87_values[7] = ten;
    cpu.x87_classes[7] = ExtendedClass::Normal;
    memory.base = 0x7000;
    memory.bytes = 2_i16.to_le_bytes().to_vec();
    memory.fail_read = false;
    let integer_divide = X86ScalarDecoder::decode(&[0xde, 0x30], cpu.rip).unwrap();
    assert!(matches!(
        integer_divide.instruction,
        ScalarInstruction::X87Arithmetic {
            operation: 6,
            integer_bytes: 2,
            ..
        }
    ));
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, integer_divide),
        ExecutionExit::Continue
    );
    assert_eq!(
        crate::x86::real::Conversion::narrow(cpu.x87_values[7], cpu.x87_classes[7], FloatWidth::Double, 0).bits,
        5_f64.to_bits()
    );
}

#[test]
fn x87_integer_family() {
    for (bytes, encoded, load, pop, truncate) in [
        (4, &[0xdb, 0x00][..], true, false, false),
        (4, &[0xdb, 0x08][..], false, true, true),
        (4, &[0xdb, 0x10][..], false, false, false),
        (4, &[0xdb, 0x18][..], false, true, false),
        (8, &[0xdd, 0x08][..], false, true, true),
        (2, &[0xdf, 0x00][..], true, false, false),
        (8, &[0xdf, 0x28][..], true, false, false),
        (8, &[0xdf, 0x38][..], false, true, false),
    ] {
        assert!(matches!(
            X86ScalarDecoder::decode(encoded, 0x56000).unwrap().instruction,
            ScalarInstruction::X87Integer { bytes: decoded, load: l, pop: p, truncate: t, .. }
                if decoded == bytes && l == load && p == pop && t == truncate
        ));
    }
    let mut memory = ModelMemory {
        base: 0x56002,
        bytes: (-7_i32).to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x56000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[0] = memory.base;
    let load = X86ScalarDecoder::decode(&[0xdb, 0x00], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.x87_status >> 11) & 7, 7);
    memory.base = 0x57002;
    memory.bytes = vec![0; 4];
    cpu.registers[0] = memory.base;
    cpu.rip = 0x57000;
    let store = X86ScalarDecoder::decode(&[0xdb, 0x18], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(i32::from_le_bytes(memory.bytes.try_into().unwrap()), -7);
    assert_eq!((cpu.x87_status >> 11) & 7, 0);
}

fn check_stack_group(opcode: u8, group: u8, operation: X87StackOperation) {
    for source in 0..8_u8 {
        for rex in [None, Some(0x40), Some(0x41), Some(0x44), Some(0x45)] {
            let mut bytes: Vec<u8> = rex.into_iter().collect();
            bytes.extend_from_slice(&[opcode, 0xc0 | group << 3 | source]);
            assert_eq!(
                X86ScalarDecoder::decode(&bytes, 0).unwrap().instruction,
                ScalarInstruction::X87Stack { source, operation }
            );
        }
    }
}

#[test]
fn x87_stack_transfer() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: true,
        fail_write: true,
        commits: 0,
    };
    for top in 0..8_usize {
        for source in 0..7_usize {
            let source_index = (top + source) & 7;
            let destination = top.wrapping_sub(1) & 7;
            let value = ExtendedReal::from_bits(0x4000_8000_0000_0000_0100 + source as u128);
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x4c000,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.x87_status = (top as u16) << 11;
            cpu.x87_values[source_index] = value;
            cpu.x87_classes[source_index] = ExtendedClass::Normal;
            let load = X86ScalarDecoder::decode(&[0xd9, 0xc0 | source as u8], cpu.rip).unwrap();
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, load),
                ExecutionExit::Continue
            );
            assert_eq!(cpu.x87_values[destination], value);
            assert_eq!(cpu.x87_classes[destination], ExtendedClass::Normal);
            assert_eq!((cpu.x87_status >> 11) & 7, destination as u16);
        }

        for source in 0..8_usize {
            check_stack_transfer(top, source, &mut memory);
        }
    }

    let mut self_pop = CpuState {
        scalar: ScalarState {
            rip: 0x4f000,
            ..Default::default()
        },
        ..Default::default()
    };
    self_pop.x87_status = 3 << 11;
    self_pop.x87_values[3] = ExtendedReal::from_bits(0x3fff_8000_0000_0000_0000);
    self_pop.x87_classes[3] = ExtendedClass::Normal;
    let instruction = X86ScalarDecoder::decode(&[0xdd, 0xd8], self_pop.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut self_pop, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!(self_pop.x87_classes[3], ExtendedClass::Empty);
    assert_eq!((self_pop.x87_status >> 11) & 7, 4);
}

fn check_stack_transfer(top: usize, source: usize, memory: &mut ModelMemory) {
    let source_index = (top + source) & 7;
    let left = ExtendedReal::from_bits(0x3fff_8000_0000_0000_0011);
    let right = ExtendedReal::from_bits(0x4000_8000_0000_0000_0022);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4d000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.x87_status = (top as u16) << 11;
    cpu.x87_values[top] = left;
    cpu.x87_classes[top] = ExtendedClass::Normal;
    cpu.x87_values[source_index] = right;
    cpu.x87_classes[source_index] = ExtendedClass::Normal;
    let exchange = X86ScalarDecoder::decode(&[0xd9, 0xc8 | source as u8], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, memory, exchange),
        ExecutionExit::Continue
    );
    let expected = if source == 0 { (right, right) } else { (right, left) };
    assert_eq!((cpu.x87_values[top], cpu.x87_values[source_index]), expected);
    assert_eq!((cpu.x87_status >> 11) & 7, top as u16);

    cpu.rip = 0x4e000;
    cpu.x87_values[top] = left;
    cpu.x87_classes[top] = ExtendedClass::Normal;
    if source != 0 {
        cpu.x87_classes[source_index] = ExtendedClass::Empty;
    }
    let store = X86ScalarDecoder::decode(&[0xdd, 0xd0 | source as u8], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.x87_values[source_index], left);
    assert_eq!(cpu.x87_classes[source_index], ExtendedClass::Normal);
}

#[test]
fn x87_stack_faults() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: true,
        fail_write: true,
        commits: 0,
    };
    let mut overflow = CpuState {
        scalar: ScalarState {
            rip: 0x50000,
            ..Default::default()
        },
        ..Default::default()
    };
    overflow.x87_classes[7] = ExtendedClass::Normal;
    overflow.x87_classes[0] = ExtendedClass::Normal;
    let load = X86ScalarDecoder::decode(&[0xd9, 0xc0], overflow.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut overflow, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(overflow.x87_values[7], ExtendedReal::INDEFINITE);
    assert_eq!(overflow.x87_status & 0x241, 0x241);
    assert_eq!((overflow.x87_status >> 11) & 7, 7);

    let mut underflow = CpuState {
        scalar: ScalarState {
            rip: 0x51000,
            ..Default::default()
        },
        ..Default::default()
    };
    let exchange = X86ScalarDecoder::decode(&[0xd9, 0xc9], underflow.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut underflow, &mut memory, exchange),
        ExecutionExit::Continue
    );
    assert_eq!(underflow.x87_values[0], ExtendedReal::INDEFINITE);
    assert_eq!(underflow.x87_values[1], ExtendedReal::INDEFINITE);
    assert_eq!(underflow.x87_status & 0x241, 0x41);

    let mut blocked = CpuState {
        scalar: ScalarState {
            rip: 0x52000,
            ..Default::default()
        },
        ..Default::default()
    };
    blocked.x87_control &= !1;
    blocked.x87_classes[0] = ExtendedClass::Normal;
    blocked.x87_classes[7] = ExtendedClass::Normal;
    let values = blocked.x87_values;
    let classes = blocked.x87_classes;
    assert_eq!(
        ScalarInterpreter::execute(&mut blocked, &mut memory, load),
        ExecutionExit::UndefinedInstruction { instruction: 0x52000 }
    );
    assert_eq!(blocked.x87_values, values);
    assert_eq!(blocked.x87_classes, classes);
    assert_eq!((blocked.x87_status >> 11) & 7, 0);
    assert_eq!(blocked.x87_status & 0x82c1, 0x82c1);

    let mut empty_pop = CpuState {
        scalar: ScalarState {
            rip: 0x53000,
            ..Default::default()
        },
        ..Default::default()
    };
    let pop = X86ScalarDecoder::decode(&[0xdd, 0xd9], empty_pop.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut empty_pop, &mut memory, pop),
        ExecutionExit::Continue
    );
    assert_eq!(empty_pop.x87_classes[1], ExtendedClass::QuietNan);
    assert_eq!((empty_pop.x87_status >> 11) & 7, 1);
    assert_eq!(empty_pop.x87_status & 0x241, 0x41);
}

#[test]
fn x87_extended_faults() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x42000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 10],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0xdb, 0x2b], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 10
    ));
    assert_eq!(cpu, original);

    memory.fail_read = false;
    memory.bytes.truncate(8);
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if matches!(access.fault(), MemoryFault {
                address: 0x1008,
                access: AccessKind::Read,
                ..
            }) && access.length() == 10
    ));
    assert_eq!(cpu, original);

    memory.bytes.resize(10, 0);
    cpu.x87_classes[7] = ExtendedClass::Normal;
    let occupied = cpu.clone();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.x87_values[7], ExtendedReal::INDEFINITE);
    assert_eq!(cpu.x87_classes[7], ExtendedClass::QuietNan);
    assert_eq!(cpu.x87_status & 0x241, 0x241);
    assert_eq!(occupied.rip + 2, cpu.rip);

    let mut empty = CpuState {
        scalar: ScalarState {
            rip: 0x43000,
            ..Default::default()
        },
        ..Default::default()
    };
    empty.registers[3] = 0x1000;
    memory.bytes.fill(0xaa);
    let store = X86ScalarDecoder::decode(&[0xdb, 0x3b], empty.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut empty, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(
        &memory.bytes[..10],
        &ExtendedReal::INDEFINITE.bits().to_le_bytes()[..10]
    );
    assert_eq!(empty.x87_status & 0x241, 0x41);

    let mut unmasked = CpuState {
        scalar: ScalarState {
            rip: 0x44000,
            ..Default::default()
        },
        ..Default::default()
    };
    unmasked.registers[3] = 0x1000;
    unmasked.x87_control &= !1;
    memory.bytes.fill(0xaa);
    assert_eq!(
        ScalarInterpreter::execute(&mut unmasked, &mut memory, store),
        ExecutionExit::UndefinedInstruction { instruction: 0x44000 }
    );
    assert_eq!(unmasked.rip, 0x44000);
    assert_eq!(unmasked.x87_status & 0x41, 0x41);
    assert!(memory.bytes.iter().all(|byte| *byte == 0xaa));

    for (bits, mask, flag) in [
        (0x0000_0000_0000_0000_0001_u128, 1_u16 << 1, 1_u16 << 1),
        (0x7fff_8000_0000_0000_0001_u128, 1, 1),
        (0x4000_0000_0000_0000_0001_u128, 1, 1),
    ] {
        memory.bytes[..10].copy_from_slice(&bits.to_le_bytes()[..10]);
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x45000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[3] = 0x1000;
        cpu.x87_control &= !mask;
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, load),
            ExecutionExit::UndefinedInstruction { instruction: 0x45000 }
        );
        assert_eq!(cpu.rip, 0x45000);
        assert_eq!(cpu.x87_classes, [ExtendedClass::Empty; 8]);
        assert_eq!(cpu.x87_status & (flag | 0x8080), flag | 0x8080);
    }
}

#[test]
fn vector_integer_wraps() {
    let left = 0xffff_ffff_ffff_ffff_0001_00ff_7fff_ffff_u128;
    let right = 0x0000_0000_0000_0001_0002_0001_0001_0001_u128;
    for (opcode, expected) in [
        (0xd4, 0x0000_0000_0000_0000_0003_0100_8001_0000_u128),
        (0xfb, 0xffff_ffff_ffff_fffe_ffff_00fe_7ffe_fffe_u128),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x2f000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[0] = left;
        cpu.vectors[1] = right;
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let ir = X86ScalarDecoder::decode(&[0x66, 0x0f, opcode, 0xc1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[0], expected);
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x30000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = 0x00ff;
    cpu.vectors[1] = 0x0001;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let byte = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xfc, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, byte),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], 0);
    assert!(matches!(
        X86ScalarDecoder::decode(&[0x0f, 0xd4, 0xc0], 0).unwrap().instruction,
        ScalarInstruction::MmxPacked {
            operation: MmxOperation::Add(8),
            ..
        },
    ));
}

#[test]
fn unsigned_dword_multiply() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x2f100,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = 0xffff_ffff_0000_0007_0000_0003_ffff_fffe;
    cpu.vectors[1] = 0xffff_ffff_0000_000b_0000_0005_ffff_fffd;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let instruction = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xf4, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue,
    );
    assert_eq!(
        cpu.vectors[0],
        u128::from(u64::from(u32::MAX - 1) * u64::from(u32::MAX - 2)) | (u128::from(7_u64 * 11) << 64),
    );
    assert!(matches!(
        X86ScalarDecoder::decode(&[0x0f, 0xf4, 0xc1], 0).unwrap().instruction,
        ScalarInstruction::MmxPacked {
            operation: MmxOperation::UnsignedMultiplyDword,
            ..
        },
    ));
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0xf4, 0xc1], 0).is_err());
}

#[test]
fn word_multiply_family() {
    let left = 0x8000_8000_ffff_0002_8000_8000_ffff_0002_u128;
    let right = 0x8000_8000_0003_0004_8000_8000_0003_0004_u128;
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x2f280,
            flags: FlagState::from_bits(u16::MAX),
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[8] = left;
    cpu.vectors[9] = right;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    for (opcode, expected) in [
        (0xd5, 0x0000_0000_fffd_0008_0000_0000_fffd_0008_u128),
        (0xe4, 0x4000_4000_0002_0000_4000_4000_0002_0000_u128),
        (0xe5, 0x4000_4000_ffff_0000_4000_4000_ffff_0000_u128),
        (0xf5, 0x8000_0000_0000_0005_8000_0000_0000_0005_u128),
    ] {
        let instruction = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, opcode, 0xc1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[8], expected);
        assert_eq!(cpu.flags, FlagState::from_bits(u16::MAX));
        cpu.vectors[8] = left;
        cpu.rip += 5;
    }

    cpu.write_mmx(0, 0xffff_0002_8000_8000);
    cpu.write_mmx(1, 0x0003_0004_8000_8000);
    let mmx = X86ScalarDecoder::decode(&[0x0f, 0xe4, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, mmx),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.read_mmx(0), 0x0002_0000_4000_4000);

    cpu.rip = 0x2f300;
    cpu.registers[3] = 0x1000;
    cpu.vectors[8] = left;
    memory.base = 0x1000;
    memory.bytes = right.to_le_bytes().to_vec();
    let from_memory = X86ScalarDecoder::decode(&[0x66, 0x44, 0x0f, 0xd5, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, from_memory),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8], 0x0000_0000_fffd_0008_0000_0000_fffd_0008);

    cpu.rip = 0x2f320;
    memory.bytes.truncate(8);
    let original = cpu.clone();
    let fault = X86ScalarDecoder::decode(&[0x66, 0x44, 0x0f, 0xf5, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.address() == 0x1008 && access.length() == 16
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0xd5, 0xc1], 0).is_err());
}

#[test]
fn indirect_call_width() {
    let ir = X86ScalarDecoder::decode(&[0x41, 0xff, 0x57, 0x10], 0x30000).unwrap();
    assert_eq!(ir.width, ScalarWidth::Qword);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x30000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[4] = 0x1030;
    cpu.registers[15] = 0x1000;
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 64],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    memory.bytes[16..24].copy_from_slice(&0x4000_u64.to_le_bytes());
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.rip, cpu.registers[4]), (0x4000, 0x1028));
    assert_eq!(memory.read(0x1028, 8).unwrap(), 0x30004);
}

#[test]
fn unaligned_vector_moves() {
    let value = 0x1234_5678_9abc_def0_0fed_cba9_8765_4321_u128;
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x31000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1001;
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: value.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x44, 0x0f, 0x10, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.vectors[8], cpu.rip), (value, 0x31004));

    cpu.rip = 0x32000;
    cpu.vectors[9] = !value;
    let store = X86ScalarDecoder::decode(&[0x66, 0x44, 0x0f, 0x11, 0x0b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(u128::from_le_bytes(memory.bytes[..16].try_into().unwrap()), !value);

    cpu.rip = 0x33000;
    cpu.vectors[0] = value;
    let alias = X86ScalarDecoder::decode(&[0x0f, 0x10, 0xc0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, alias),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], value);
}

#[test]
fn accumulator_extension() {
    let cases = [
        (
            &[0x66, 0x98][..],
            ScalarWidth::Word,
            ScalarWidth::Byte,
            0x1122_3344_5566_7780,
            0x1122_3344_5566_ff80,
        ),
        (
            &[0x98][..],
            ScalarWidth::Dword,
            ScalarWidth::Word,
            0x1122_3344_5566_8001,
            0x0000_0000_ffff_8001,
        ),
        (
            &[0x48, 0x98][..],
            ScalarWidth::Qword,
            ScalarWidth::Dword,
            0x1122_3344_8000_0001,
            0xffff_ffff_8000_0001,
        ),
        (
            &[0x4f, 0x98][..],
            ScalarWidth::Qword,
            ScalarWidth::Dword,
            0xffff_ffff_7fff_ffff,
            0x0000_0000_7fff_ffff,
        ),
        (
            &[0x66, 0x48, 0x98][..],
            ScalarWidth::Qword,
            ScalarWidth::Dword,
            0x1234_5678_ffff_ffff,
            u64::MAX,
        ),
    ];
    for (bytes, width, source_width, initial, expected) in cases {
        let ir = X86ScalarDecoder::decode(bytes, 0x34000).unwrap();
        assert_eq!(ir.width, width);
        assert_eq!(
            ir.instruction,
            ScalarInstruction::AccumulatorSignExtend { source_width }
        );
        let flags = FlagState::from_bits(0x8d5);
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x34000,
                flags,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[0] = initial;
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: true,
            fail_write: true,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(
            (cpu.registers[0], cpu.flags, cpu.rip),
            (expected, flags, 0x34000 + bytes.len() as u64)
        );
        assert_eq!(memory.commits, 0);
    }

    for bytes in [&[0xf3, 0x98][..], &[0xf2, 0x98], &[0x2e, 0x98], &[0x67, 0x98]] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_ok());
    }
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x98], 0).is_err());
}

#[test]
fn accumulator_high_extension() {
    let cases = [
        (
            &[0x66, 0x99][..],
            ScalarWidth::Word,
            0x1122_3344_5566_8001,
            0x8877_6655_4433_2211,
            0x8877_6655_4433_ffff,
        ),
        (
            &[0x66, 0x99][..],
            ScalarWidth::Word,
            0x1122_3344_5566_7fff,
            0x8877_6655_4433_ffff,
            0x8877_6655_4433_0000,
        ),
        (
            &[0x99][..],
            ScalarWidth::Dword,
            0x1122_3344_8000_0001,
            u64::MAX,
            0x0000_0000_ffff_ffff,
        ),
        (&[0x99][..], ScalarWidth::Dword, 0xffff_ffff_7fff_ffff, u64::MAX, 0),
        (
            &[0x48, 0x99][..],
            ScalarWidth::Qword,
            0x8000_0000_0000_0001,
            0,
            u64::MAX,
        ),
        (
            &[0x4f, 0x99][..],
            ScalarWidth::Qword,
            0x7fff_ffff_ffff_ffff,
            u64::MAX,
            0,
        ),
        (&[0x66, 0x48, 0x99][..], ScalarWidth::Qword, u64::MAX, 0, u64::MAX),
    ];
    for (bytes, width, accumulator, initial_high, expected_high) in cases {
        let ir = X86ScalarDecoder::decode(bytes, 0x404ab6).unwrap();
        assert_eq!(ir.width, width);
        assert_eq!(ir.instruction, ScalarInstruction::AccumulatorHighExtend);
        let flags = FlagState::from_bits(0x8d5);
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x404ab6,
                flags,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[0] = accumulator;
        cpu.registers[2] = initial_high;
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: true,
            fail_write: true,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[0], accumulator);
        assert_eq!(cpu.registers[2], expected_high);
        assert_eq!(cpu.flags, flags);
        assert_eq!(cpu.rip, 0x404ab6 + bytes.len() as u64);
    }
    for bytes in [&[0xf3, 0x99][..], &[0xf2, 0x99], &[0x2e, 0x99], &[0x67, 0x99]] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_ok());
    }
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x99], 0).is_err());
}

#[test]
fn dword_shuffle() {
    let value = 0x4444_4444_3333_3333_2222_2222_1111_1111_u128;
    for (selectors, expected) in [
        (0x00, 0x1111_1111_1111_1111_1111_1111_1111_1111),
        (0x55, 0x2222_2222_2222_2222_2222_2222_2222_2222),
        (0xaa, 0x3333_3333_3333_3333_3333_3333_3333_3333),
        (0xff, 0x4444_4444_4444_4444_4444_4444_4444_4444),
        (0x1b, 0x1111_1111_2222_2222_3333_3333_4444_4444),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x35000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[9] = value;
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let ir = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x70, 0xc1, selectors], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[8], expected);
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x36000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = value;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let alias = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x70, 0xc0, 0xe4], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, alias),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], 0x4444_4444_3333_3333_2222_2222_1111_1111);
}

#[test]
fn word_shuffle_selectors() {
    let basis = 0x8888_7777_6666_5555_4444_3333_2222_1111_u128;
    for (prefix, base) in [(0xf2, 0_u32), (0xf3, 4_u32)] {
        for selectors in 0_u8..=u8::MAX {
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x700,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.vectors[1] = basis;
            cpu.vectors[2] = u128::MAX;
            let instruction = X86ScalarDecoder::decode(&[prefix, 0x0f, 0x70, 0xd1, selectors], cpu.rip).unwrap();
            let mut memory = ModelMemory {
                base: 0,
                bytes: Vec::new(),
                fail_read: false,
                fail_write: false,
                commits: 0,
            };
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
                ExecutionExit::Continue
            );
            let preserved = [basis & (!0_u128 << 64), basis & u128::from(u64::MAX)][base as usize / 4];
            let source0 = u32::from(selectors & 3);
            let source1 = u32::from(selectors >> 2 & 3);
            let source2 = u32::from(selectors >> 4 & 3);
            let source3 = u32::from(selectors >> 6 & 3);
            let expected = preserved
                | (basis >> ((base + source0) * 16) & 0xffff) << (base * 16)
                | (basis >> ((base + source1) * 16) & 0xffff) << ((base + 1) * 16)
                | (basis >> ((base + source2) * 16) & 0xffff) << ((base + 2) * 16)
                | (basis >> ((base + source3) * 16) & 0xffff) << ((base + 3) * 16);
            assert_eq!(cpu.vectors[2], expected, "prefix={prefix:x} selectors={selectors:x}");
        }
    }
}

#[test]
fn word_shuffle_memory() {
    let basis = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210_u128;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&basis.to_le_bytes());
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes,
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x800,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[1] = 0x1000;
    let instruction = X86ScalarDecoder::decode(&[0xf2, 0x44, 0x0f, 0x70, 0x01, 0x1b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8] >> 64, basis >> 64);
    cpu.rip = 0x900;
    let original = cpu.clone();
    memory.fail_read = true;
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0x0f, 0x70, 0xc1, 0], 0).is_err());
}

#[test]
fn dword_shuffle_memory() {
    let value = 0x4444_4444_3333_3333_2222_2222_1111_1111_u128;
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x37000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[8] = u128::MAX;
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: value.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x66, 0x44, 0x0f, 0x70, 0x43, 0x01, 0x1b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8], 0x1111_1111_2222_2222_3333_3333_4444_4444);

    cpu.rip = 0x38000;
    cpu.vectors[8] = u128::MAX;
    let original = cpu.clone();
    memory.fail_read = true;
    let fault = X86ScalarDecoder::decode(&[0x66, 0x44, 0x0f, 0x70, 0x43, 0x01, 0xe4], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0x66, 0xf2, 0x0f, 0x70, 0xc0, 0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf0, 0xf3, 0x0f, 0x70, 0xc0, 0], 0).is_err());
}

#[test]
fn packed_shuffle_decode() {
    for case in 0_u32..2 * 16 * 16 * 256 {
        let double = case >= 16 * 16 * 256;
        let selectors = case as u8;
        let source = (case >> 8) as u8 & 15;
        let destination = (case >> 12) as u8 & 15;
        let mut bytes = Vec::with_capacity(6);
        if double {
            bytes.push(0x66);
        }
        bytes.extend_from_slice(&[
            0x40 | ((destination >> 3) << 2) | (source >> 3),
            0x0f,
            0xc6,
            0xc0 | ((destination & 7) << 3) | (source & 7),
            selectors,
        ]);
        let instruction = X86ScalarDecoder::decode(&bytes, 0x4000).unwrap();
        assert_eq!(instruction.length, bytes.len() as u8);
        assert_eq!(
            instruction.instruction,
            ScalarInstruction::VectorShuffle {
                mode: if double {
                    VectorShuffleMode::PackedDouble
                } else {
                    VectorShuffleMode::PackedSingle
                },
                destination,
                source: VectorSource::Register(source),
                selectors,
            }
        );
    }

    for bytes in [
        &[0xf2, 0x0f, 0xc6, 0xc0, 0][..],
        &[0xf3, 0x0f, 0xc6, 0xc0, 0],
        &[0x66, 0xf2, 0x0f, 0xc6, 0xc0, 0],
        &[0xf0, 0x0f, 0xc6, 0xc0, 0],
    ] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_err());
    }
}

#[test]
fn packed_shuffle_values() {
    let left = 0x4444_4444_3333_3333_2222_2222_1111_1111_u128;
    let right = 0xdddd_dddd_cccc_cccc_bbbb_bbbb_aaaa_aaaa_u128;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    for selectors in 0_u8..=u8::MAX {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x55000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[8] = left;
        cpu.vectors[9] = right;
        let single = X86ScalarDecoder::decode(&[0x45, 0x0f, 0xc6, 0xc1, selectors], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, single),
            ExecutionExit::Continue
        );
        let mut expected = 0_u128;
        for lane in 0..4 {
            let source = u32::from(selectors >> (lane * 2) & 3);
            let vector = [left, left, right, right][lane];
            expected |= (vector >> (source * 32) & u128::from(u32::MAX)) << (lane * 32);
        }
        assert_eq!(cpu.vectors[8], expected, "single selectors={selectors:#x}");

        cpu.rip = 0x56000;
        cpu.vectors[8] = left;
        cpu.vectors[9] = right;
        let double = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0xc6, 0xc1, selectors], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, double),
            ExecutionExit::Continue
        );
        let low = left >> (u32::from(selectors & 1) * 64) & u128::from(u64::MAX);
        let high = right >> (u32::from(selectors >> 1 & 1) * 64) & u128::from(u64::MAX);
        assert_eq!(cpu.vectors[8], low | high << 64, "double selectors={selectors:#x}");
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x57000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = left;
    let alias = X86ScalarDecoder::decode(&[0x0f, 0xc6, 0xc0, 0xe4], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, alias),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], left);
}

#[test]
fn packed_shuffle_memory() {
    let left = 0x4444_4444_3333_3333_2222_2222_1111_1111_u128;
    let right = 0xdddd_dddd_cccc_cccc_bbbb_bbbb_aaaa_aaaa_u128;
    for offset in [0_u64, 1] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x58000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[11] = 0x1000;
        cpu.vectors[8] = left;
        let mut memory = ModelMemory {
            base: 0x1000 + offset,
            bytes: right.to_le_bytes().to_vec(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let single = X86ScalarDecoder::decode(&[0x45, 0x0f, 0xc6, 0x43, offset as u8, 0xe4], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, single),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[8], 0xdddd_dddd_cccc_cccc_2222_2222_1111_1111);

        cpu.rip = 0x59000;
        cpu.vectors[8] = left;
        let original = cpu.clone();
        memory.fail_read = true;
        let fault = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0xc6, 0x43, offset as u8, 0x01], cpu.rip).unwrap();
        assert!(matches!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
                ExecutionExit::OperandFault(access) if access.length() == 16
        ));
        assert_eq!(cpu, original);
        assert_eq!(memory.commits, 0);
    }
}

#[test]
fn packed_equal_lanes() {
    let left = 0xffff_0000_8000_7fff_1234_5678_aabb_ccdd_u128;
    let right = 0xffff_1111_8000_0000_1234_9999_aacc_ccdd_u128;
    for (opcode, expected) in [
        (0x74, 0xffff_0000_ffff_0000_ffff_0000_ff00_ffff_u128),
        (0x75, 0xffff_0000_ffff_0000_ffff_0000_0000_ffff_u128),
        (0x76, 0x0000_0000_0000_0000_0000_0000_0000_0000_u128),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x39000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[8] = left;
        cpu.vectors[9] = right;
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let ir = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, opcode, 0xc1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[8], expected);
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x3a000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = left;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let alias = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x76, 0xc0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, alias),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], u128::MAX);
}

#[test]
fn packed_equal_memory() {
    let left = 0xaaaa_bbbb_cccc_dddd_1111_2222_3333_4444_u128;
    let right = 0xaaaa_0000_cccc_0000_1111_0000_3333_0000_u128;
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x3b000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[11] = 0x1000;
    cpu.vectors[8] = left;
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: right.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x75, 0x43, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8], 0xffff_0000_ffff_0000_ffff_0000_ffff_0000);

    cpu.rip = 0x3c000;
    cpu.vectors[8] = left;
    let original = cpu.clone();
    memory.fail_read = true;
    let fault = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x74, 0x43, 0x01], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);
    for prefix in [0xf2, 0xf3] {
        assert!(X86ScalarDecoder::decode(&[prefix, 0x0f, 0x74, 0xc0], 0).is_err());
    }
}

#[test]
fn packed_compare_decode() {
    for pair in 0_u16..256 {
        let destination = (pair / 16) as u8;
        let source = (pair % 16) as u8;
        let rex = 0x40 | ((destination >> 3) << 2) | (source >> 3);
        for (opcode, comparison, lane) in [
            (0x64, VectorComparison::SignedGreater, 1),
            (0x65, VectorComparison::SignedGreater, 2),
            (0x66, VectorComparison::SignedGreater, 4),
            (0x74, VectorComparison::Equal, 1),
            (0x75, VectorComparison::Equal, 2),
            (0x76, VectorComparison::Equal, 4),
        ] {
            let bytes = [0x66, rex, 0x0f, opcode, 0xc0 | ((destination & 7) << 3) | (source & 7)];
            let ir = X86ScalarDecoder::decode(&bytes, 0x422f89).unwrap();
            assert_eq!(
                ir.instruction,
                ScalarInstruction::VectorCompare {
                    comparison,
                    destination,
                    source: VectorSource::Register(source),
                    lane,
                }
            );
        }
    }
    for pair in 0_u16..256 {
        let destination = (pair / 16) as u8;
        let source = (pair % 16) as u8;
        let rex = 0x40 | ((destination >> 3) << 2) | (source >> 3);
        for (opcode, comparison) in [(0x29, VectorComparison::Equal), (0x37, VectorComparison::SignedGreater)] {
            let bytes = [
                0x66,
                rex,
                0x0f,
                0x38,
                opcode,
                0xc0 | ((destination & 7) << 3) | (source & 7),
            ];
            let ir = X86ScalarDecoder::decode(&bytes, 0x422f89).unwrap();
            assert_eq!(
                ir.instruction,
                ScalarInstruction::VectorCompare {
                    comparison,
                    destination,
                    source: VectorSource::Register(source),
                    lane: 8,
                }
            );
        }
    }
}

#[test]
fn packed_compare_values() {
    let left = u128::from_le_bytes([
        0x80, 0x7f, 0xff, 0x00, 0x81, 0x01, 0xfe, 0x7e, 0x80, 0x00, 0x7f, 0xff, 0x01, 0x81, 0x7e, 0xfe,
    ]);
    let right = u128::from_le_bytes([
        0x7f, 0x80, 0xff, 0xff, 0x80, 0x00, 0x01, 0x7e, 0xff, 0x00, 0x80, 0x7f, 0x00, 0x82, 0x7d, 0xff,
    ]);
    for (bytes, lane, comparison) in [
        (&[0x66, 0x44, 0x0f, 0x64, 0xc6][..], 1, VectorComparison::SignedGreater),
        (&[0x66, 0x44, 0x0f, 0x65, 0xc6], 2, VectorComparison::SignedGreater),
        (&[0x66, 0x44, 0x0f, 0x66, 0xc6], 4, VectorComparison::SignedGreater),
        (
            &[0x66, 0x44, 0x0f, 0x38, 0x37, 0xc6],
            8,
            VectorComparison::SignedGreater,
        ),
        (&[0x66, 0x44, 0x0f, 0x74, 0xc6], 1, VectorComparison::Equal),
        (&[0x66, 0x44, 0x0f, 0x75, 0xc6], 2, VectorComparison::Equal),
        (&[0x66, 0x44, 0x0f, 0x76, 0xc6], 4, VectorComparison::Equal),
        (&[0x66, 0x44, 0x0f, 0x38, 0x29, 0xc6], 8, VectorComparison::Equal),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x422f89,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[8] = left;
        cpu.vectors[6] = right;
        cpu.flags = FlagState::from_bits(0x8d5);
        let flags = cpu.flags;
        let expected = VectorLane::compare(left, right, lane, comparison);
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: true,
            fail_write: true,
            commits: 0,
        };
        let ir = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[8], expected);
        assert_eq!(cpu.flags, flags);
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x422f89,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = left;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: true,
        fail_write: true,
        commits: 0,
    };
    let alias = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x64, 0xc0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, alias),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], 0);
}

#[test]
fn packed_compare_memory() {
    for (opcode, lane, comparison) in [
        (0x64, 1, VectorComparison::SignedGreater),
        (0x65, 2, VectorComparison::SignedGreater),
        (0x66, 4, VectorComparison::SignedGreater),
        (0x74, 1, VectorComparison::Equal),
        (0x75, 2, VectorComparison::Equal),
        (0x76, 4, VectorComparison::Equal),
    ] {
        let left = 0x8000_0000_0000_0000_7fff_ffff_ffff_ffff_u128;
        let right = 0xffff_ffff_ffff_ffff_0000_0000_0000_0000_u128;
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x430000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[11] = 0x1000;
        cpu.vectors[8] = left;
        let mut memory = ModelMemory {
            base: 0x1001,
            bytes: right.to_le_bytes().to_vec(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let ir = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, opcode, 0x43, 0x01], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[8], VectorLane::compare(left, right, lane, comparison));

        cpu.rip = 0x440000;
        cpu.vectors[8] = left;
        let original = cpu.clone();
        memory.bytes.truncate(8);
        let fault = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, opcode, 0x43, 0x01], cpu.rip).unwrap();
        assert!(matches!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
                ExecutionExit::OperandFault(access) if access.length() == 16
        ));
        assert_eq!(cpu, original);
        assert_eq!(memory.commits, 0);
    }
    for (opcode, comparison) in [(0x29, VectorComparison::Equal), (0x37, VectorComparison::SignedGreater)] {
        let left = 0x8000_0000_0000_0000_7fff_ffff_ffff_ffff_u128;
        let right = 0xffff_ffff_ffff_ffff_0000_0000_0000_0000_u128;
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x450000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[11] = 0x2000;
        cpu.vectors[8] = left;
        let mut memory = ModelMemory {
            base: 0x2001,
            bytes: right.to_le_bytes().to_vec(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let ir = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x38, opcode, 0x43, 0x01], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[8], VectorLane::compare(left, right, 8, comparison));

        cpu.rip = 0x460000;
        cpu.vectors[8] = left;
        let original = cpu.clone();
        memory.bytes.truncate(8);
        let fault = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x38, opcode, 0x43, 0x01], cpu.rip).unwrap();
        assert!(matches!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
                ExecutionExit::OperandFault(access) if access.length() == 16
        ));
        assert_eq!(cpu, original);
        assert_eq!(memory.commits, 0);
    }
}

#[test]
fn aes_legacy_register_and_fault() {
    let state = u128::from_le_bytes([
        0x00, 0x10, 0x20, 0x30, 0x40, 0x50, 0x60, 0x70, 0x80, 0x90, 0xa0, 0xb0, 0xc0, 0xd0, 0xe0, 0xf0,
    ]);
    let key = u128::from_le_bytes([
        0xd6, 0xaa, 0x74, 0xfd, 0xd2, 0xaf, 0x72, 0xfa, 0xda, 0xa6, 0x78, 0xf1, 0xd6, 0xab, 0x76, 0xfe,
    ]);
    let expected = u128::from_le_bytes([
        0x89, 0xd8, 0x10, 0xe8, 0x85, 0x5a, 0xce, 0x68, 0x2d, 0x18, 0x43, 0xd8, 0xcb, 0x12, 0x8f, 0xe4,
    ]);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x470000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[8] = state;
    cpu.vectors[9] = key;
    cpu.flags = FlagState::default().with(Flag::Carry, true).with(Flag::Zero, true);
    let flags = cpu.flags;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: true,
        fail_write: true,
        commits: 0,
    };
    let instruction = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x38, 0xdc, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8], expected);
    assert_eq!(cpu.flags, flags);

    cpu.rip = 0x480000;
    cpu.registers[11] = 0x1000;
    cpu.vectors[8] = state;
    let original = cpu.clone();
    let fault = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x38, 0xdc, 0x43, 0x01], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);
}

#[test]
fn aes_rejects_wrong_prefixes() {
    for bytes in [
        &[0x0f, 0x38, 0xdc, 0xc0][..],
        &[0xf2, 0x66, 0x0f, 0x38, 0xdc, 0xc0],
        &[0xf3, 0x66, 0x0f, 0x38, 0xdc, 0xc0],
        &[0xf0, 0x66, 0x0f, 0x38, 0xdc, 0xc0],
    ] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_err());
    }
    assert!(X86ScalarDecoder::decode(&[0x66, 0x0f, 0x3a, 0xdf, 0xc0, 0x8b], 0).is_ok());
}

#[test]
fn packed_compare_rejects() {
    for bytes in [
        &[0xf2, 0x0f, 0x64, 0xc0][..],
        &[0xf3, 0x0f, 0x64, 0xc0],
        &[0xf0, 0x66, 0x0f, 0x64, 0xc0],
        &[0x66, 0x0f, 0x38, 0x36, 0xc0],
        &[0x66, 0xf2, 0x0f, 0x38, 0x29, 0xc0],
        &[0xf0, 0x66, 0x0f, 0x38, 0x37, 0xc0],
    ] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_err(), "accepted {bytes:02x?}");
    }
    for opcode in [0x64, 0x65, 0x66, 0x74, 0x75, 0x76] {
        for prefix in [0xf0, 0xf2, 0xf3] {
            assert!(X86ScalarDecoder::decode(&[prefix, 0x66, 0x0f, opcode, 0xc0], 0).is_err());
        }
    }
    for opcode in [0x29, 0x37] {
        for prefix in [0xf0, 0xf2, 0xf3] {
            assert!(X86ScalarDecoder::decode(&[prefix, 0x66, 0x0f, 0x38, opcode, 0xc0], 0).is_err());
        }
        assert!(X86ScalarDecoder::decode(&[0x0f, 0x38, opcode, 0xc0], 0).is_err());
    }
}

#[test]
fn packed_byte_mask() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x3d000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[9] = u128::from_le_bytes([
        0x80, 0x7f, 0xff, 0x00, 0x81, 0x01, 0xfe, 0x7e, 0x80, 0x00, 0x7f, 0xff, 0x01, 0x81, 0x7e, 0xfe,
    ]);
    cpu.registers[8] = u64::MAX;
    let flags = FlagState::from_bits(0x8d5);
    cpu.flags = flags;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: true,
        fail_write: true,
        commits: 0,
    };
    let ir = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0xd7, 0xc1], cpu.rip).unwrap();
    assert_eq!(ir.width, ScalarWidth::Dword);
    assert_eq!(
        ir.instruction,
        ScalarInstruction::VectorMask {
            destination: ScalarRegister::General(8),
            source: 9,
            lane: 1,
        }
    );
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.registers[8], cpu.flags), (0xa955, flags));

    for bit in 0..16 {
        cpu.rip = 0x3e000;
        cpu.vectors[0] = 1_u128 << (bit * 8 + 7);
        cpu.registers[0] = u64::MAX;
        let basis = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xd7, 0xc0], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, basis),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[0], 1_u64 << bit);
    }
}

#[test]
fn floating_mask_decode() {
    for case in 0_u16..1024 {
        let pair = case & 255;
        let destination = (pair / 16) as u8;
        let source = (pair % 16) as u8;
        let wide = case & 256 != 0;
        let double = case & 512 != 0;
        let rex = 0x40 | (u8::from(wide) << 3) | ((destination >> 3) << 2) | (source >> 3);
        let mut bytes = Vec::new();
        if double {
            bytes.push(0x66);
        }
        bytes.extend([rex, 0x0f, 0x50, 0xc0 | ((destination & 7) << 3) | (source & 7)]);
        let ir = X86ScalarDecoder::decode(&bytes, 0x40185f).unwrap();
        assert_eq!(ir.width, ScalarWidth::Dword);
        assert_eq!(
            ir.instruction,
            ScalarInstruction::VectorMask {
                destination: ScalarRegister::General(destination),
                source,
                lane: if double { 8 } else { 4 },
            }
        );
    }
}

#[test]
fn floating_mask_values() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: true,
        fail_write: true,
        commits: 0,
    };
    let flags = FlagState::from_bits(0x8d5);
    for (bytes, value, expected) in [
        (
            &[0x45, 0x0f, 0x50, 0xc0][..],
            0x0000_0000_8000_0000_7fff_ffff_ffff_ffff_u128,
            5_u64,
        ),
        (
            &[0x66, 0x45, 0x0f, 0x50, 0xc0],
            0x8000_0000_0000_0000_7fff_ffff_ffff_ffff_u128,
            2_u64,
        ),
        (
            &[0x66, 0x4d, 0x0f, 0x50, 0xc0],
            0xffff_ffff_ffff_ffff_8000_0000_0000_0000_u128,
            3_u64,
        ),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x40185f,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[8] = u64::MAX;
        cpu.vectors[8] = value;
        cpu.flags = flags;
        let ir = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[8], expected);
        assert_eq!(cpu.vectors[8], value);
        assert_eq!(cpu.flags, flags);
    }
}

#[test]
fn floating_mask_rejects() {
    for bytes in [
        &[0x0f, 0x50, 0x00][..],
        &[0x66, 0x0f, 0x50, 0x00],
        &[0xf2, 0x0f, 0x50, 0xc0],
        &[0xf3, 0x0f, 0x50, 0xc0],
        &[0x66, 0xf2, 0x0f, 0x50, 0xc0],
        &[0x66, 0xf3, 0x0f, 0x50, 0xc0],
        &[0xf0, 0x0f, 0x50, 0xc0],
        &[0xf0, 0x66, 0x0f, 0x50, 0xc0],
        &[0x64, 0x0f, 0x50, 0xc0],
    ] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_err(), "accepted {bytes:02x?}");
    }
}

#[test]
fn insert_word_lanes() {
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: 0xabcd_u16.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x900,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[1] = 0x1234;
    cpu.vectors[2] = u128::MAX;
    let register = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xc4, 0xd1, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, register),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[2] >> 48 & 0xffff, 0x1234);
    cpu.rip = 0xa00;
    cpu.registers[1] = 0x1000;
    let load = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xc4, 0x09, 0x07], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1] >> 112, 0xabcd);
    cpu.rip = 0xb00;
    let original = cpu.clone();
    memory.fail_read = true;
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 2
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0xc4, 0xc1, 0], 0).is_err());
}

#[test]
fn byte_mask_rejects() {
    assert!(X86ScalarDecoder::decode(&[0x66, 0x0f, 0xd7, 0x00], 0).is_err());
    assert!(matches!(
        X86ScalarDecoder::decode(&[0x0f, 0xd7, 0xc0], 0).unwrap().instruction,
        ScalarInstruction::MmxMask { .. }
    ));
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x0f, 0xd7, 0xc0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0xd7, 0xc0], 0).is_err());
}

#[test]
fn bit_scan_basis() {
    for (prefixes, width, bits) in [
        (&[0x66][..], ScalarWidth::Word, 16_u32),
        (&[][..], ScalarWidth::Dword, 32),
        (&[0x48][..], ScalarWidth::Qword, 64),
    ] {
        for bit in 0..bits {
            let mut bytes = prefixes.to_vec();
            bytes.extend_from_slice(&[0x0f, 0xbc, 0xc1]);
            let ir = X86ScalarDecoder::decode(&bytes, 0x3f000).unwrap();
            assert_eq!(ir.width, width);
            let flags = FlagState::from_bits((1 << Flag::Carry as u8) | (1 << Flag::Sign as u8));
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x3f000,
                    flags,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.registers[0] = u64::MAX;
            cpu.registers[1] = 1_u64 << bit;
            let mut memory = ModelMemory {
                base: 0,
                bytes: Vec::new(),
                fail_read: false,
                fail_write: false,
                commits: 0,
            };
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
                ExecutionExit::Continue
            );
            assert_eq!(cpu.read_register(ScalarRegister::General(0), width), u64::from(bit));
            assert!(!cpu.flags.contains(Flag::Zero));
            assert!(cpu.flags.contains(Flag::Carry));
            assert!(cpu.flags.contains(Flag::Sign));
        }
    }
}

#[test]
fn immediate_bit_actions() {
    for (extension, expected) in [(4_u8, 3_u64), (5, 3), (6, 1), (7, 1)] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x1200,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[0] = 3;
        cpu.flags = FlagState::from_bits(u16::MAX).with(Flag::Carry, false);
        let flags = cpu.flags;
        let modrm = 0xc0 | extension << 3;
        let instruction = X86ScalarDecoder::decode(&[0x48, 0x0f, 0xba, modrm, 1], cpu.rip).unwrap();
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[0], expected);
        assert!(cpu.flags.contains(Flag::Carry));
        assert_eq!(
            cpu.flags.bits() & !(1 << Flag::Carry as u8),
            flags.bits() & !(1 << Flag::Carry as u8)
        );
    }
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1240,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = (-12_i64) as u64;
    let test = X86ScalarDecoder::decode(&[0x0f, 0xba, 0xe3, 0], cpu.rip).unwrap();
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, test),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[3], (-12_i64) as u64);
    assert!(X86ScalarDecoder::decode(&[0x0f, 0xba, 0xc0, 0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x0f, 0xba, 0x20, 0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x0f, 0xab, 0xc8], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x0f, 0xab, 0xc8], 0).is_err());
}

#[test]
fn register_bit_widths() {
    for (bytes, index, expected) in [
        (&[0x66, 0x0f, 0xab, 0xc8][..], 15_u64, 1_u64 << 15),
        (&[0x0f, 0xab, 0xc8][..], 31, 1_u64 << 31),
        (&[0x48, 0x0f, 0xab, 0xc8][..], 63, 1_u64 << 63),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x1280,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[1] = index;
        let instruction = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[0], expected);
        assert!(!cpu.flags.contains(Flag::Carry));
    }
}

#[test]
fn memory_bit_addressing() {
    let mut memory = ModelMemory {
        base: 0x0fff,
        bytes: vec![0x80, 0, 0x80],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1300,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[0] = 0x1000;
    cpu.registers[1] = u64::MAX;
    let negative = X86ScalarDecoder::decode(&[0x48, 0x0f, 0xb3, 0x08], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, negative),
        ExecutionExit::Continue
    );
    assert!(cpu.flags.contains(Flag::Carry));
    assert_eq!(memory.read(0x0fff, 1).unwrap(), 0);

    cpu.rip = 0x1400;
    cpu.registers[0] = 0x0ffd;
    let locked = X86ScalarDecoder::decode(&[0xf0, 0x0f, 0xba, 0x28, 0x7f], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, locked),
        ExecutionExit::Continue
    );
    assert_eq!(memory.read(0x1000, 1).unwrap(), 0x80);
    cpu.rip = 0x1400;
    let original = cpu.clone();
    memory.fail_write = true;
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, locked),
        ExecutionExit::OperandFault(access) if access.length() == 4
    ));
    assert_eq!(cpu, original);
}

#[test]
fn bit_scan_zero() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x40000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[8] = 0;
    cpu.registers[9] = 0x1122_3344_5566_7788;
    let flags = FlagState::from_bits((1 << Flag::Carry as u8) | (1 << Flag::Overflow as u8));
    cpu.flags = flags;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let zero = X86ScalarDecoder::decode(&[0x4d, 0x0f, 0xbc, 0xc8], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, zero),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[9], 0x1122_3344_5566_7788);
    assert!(cpu.flags.contains(Flag::Zero));
    assert!(cpu.flags.contains(Flag::Carry));
    assert!(cpu.flags.contains(Flag::Overflow));

    cpu.rip = 0x41000;
    cpu.registers[0] = 1 << 13;
    let alias = X86ScalarDecoder::decode(&[0x0f, 0xbc, 0xc0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, alias),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 13);

    cpu.rip = 0x41100;
    cpu.registers[0] = 0xffff_ffff_8000_0000;
    let narrow = X86ScalarDecoder::decode(&[0x0f, 0xbc, 0xc0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, narrow),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 31);
}

#[test]
fn bit_scan_memory() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x42000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1001;
    cpu.registers[8] = u64::MAX;
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: (1_u64 << 47).to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x4c, 0x0f, 0xbc, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[8], 47);

    cpu.rip = 0x43000;
    cpu.registers[8] = u64::MAX;
    let original = cpu.clone();
    memory.fail_read = true;
    let fault = X86ScalarDecoder::decode(&[0x4c, 0x0f, 0xbc, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x0f, 0xbc, 0xc0], 0).is_err());
}

#[test]
fn leading_zero_count() {
    for (bytes, source, result) in [
        (&[0xf3, 0x66, 0x0f, 0xbd, 0xc1][..], 0x80_u64, 8_u64),
        (&[0xf3, 0x0f, 0xbd, 0xc1][..], 1 << 17, 14),
        (&[0xf3, 0x48, 0x0f, 0xbd, 0xc1][..], 1 << 41, 22),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x43300,
                flags: FlagState::from_bits(u16::MAX),
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[1] = source;
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let instruction = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.read_register(ScalarRegister::General(0), instruction.width), result);
        assert!(!cpu.flags.contains(Flag::Carry));
        assert_eq!(cpu.flags.contains(Flag::Zero), result == 0);
        assert!(cpu.flags.contains(Flag::Parity));
        assert!(cpu.flags.contains(Flag::Auxiliary));
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x43380,
            ..Default::default()
        },
        ..Default::default()
    };
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let instruction = X86ScalarDecoder::decode(&[0xf3, 0x0f, 0xbd, 0xc0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 32);
    assert!(cpu.flags.contains(Flag::Carry));
    assert!(!cpu.flags.contains(Flag::Zero));
}

#[test]
fn population_count() {
    for (bytes, source, result, high) in [
        (
            &[0xf3, 0x66, 0x0f, 0xb8, 0xc1][..],
            0xffff_0000_8001_u64,
            2_u64,
            0xffff_0000_u64,
        ),
        (&[0xf3, 0x0f, 0xb8, 0xc1][..], 0xffff_0000_8000_0001, 2, 0),
        (&[0xf3, 0x48, 0x0f, 0xb8, 0xc1][..], u64::MAX, 64, 0),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x433c0,
                flags: FlagState::from_bits(u16::MAX),
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[0] = high;
        cpu.registers[1] = source;
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let instruction = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.read_register(ScalarRegister::General(0), instruction.width), result);
        assert_eq!(cpu.registers[0] & !0xffff, high);
        for flag in [
            Flag::Carry,
            Flag::Parity,
            Flag::Auxiliary,
            Flag::Zero,
            Flag::Sign,
            Flag::Overflow,
        ] {
            assert!(!cpu.flags.contains(flag));
        }
    }
    assert!(X86ScalarDecoder::decode(&[0x0f, 0xb8, 0xc0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x0f, 0xb8, 0xc0], 0).is_err());
}

#[test]
fn tzcnt_values() {
    for (prefixes, width, bits) in [
        (&[0xf3, 0x66][..], ScalarWidth::Word, 16_u32),
        (&[0xf3][..], ScalarWidth::Dword, 32),
        (&[0xf3, 0x48][..], ScalarWidth::Qword, 64),
    ] {
        for bit in 0..bits {
            let mut bytes = prefixes.to_vec();
            bytes.extend_from_slice(&[0x0f, 0xbc, 0xc1]);
            let instruction = X86ScalarDecoder::decode(&bytes, 0x43400).unwrap();
            assert_eq!(instruction.width, width);
            let flags = FlagState::from_bits(u16::MAX);
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x43400,
                    flags,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.registers[0] = u64::MAX;
            cpu.registers[1] = 1_u64 << bit;
            let mut memory = ModelMemory {
                base: 0,
                bytes: vec![],
                fail_read: false,
                fail_write: false,
                commits: 0,
            };
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
                ExecutionExit::Continue
            );
            assert_eq!(cpu.read_register(ScalarRegister::General(0), width), u64::from(bit));
            assert!(!cpu.flags.contains(Flag::Carry));
            assert_eq!(cpu.flags.contains(Flag::Zero), bit == 0);
            assert!(cpu.flags.contains(Flag::Parity));
            assert!(cpu.flags.contains(Flag::Auxiliary));
            assert!(cpu.flags.contains(Flag::Sign));
            assert!(cpu.flags.contains(Flag::Overflow));
            assert!(width != ScalarWidth::Dword || cpu.registers[0] >> 32 == 0);
        }

        let mut bytes = prefixes.to_vec();
        bytes.extend_from_slice(&[0x0f, 0xbc, 0xc1]);
        let instruction = X86ScalarDecoder::decode(&bytes, 0x43500).unwrap();
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x43500,
                registers: [u64::MAX; 16],
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[1] = 0;
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.read_register(ScalarRegister::General(0), width), u64::from(bits));
        assert!(cpu.flags.contains(Flag::Carry));
        assert!(!cpu.flags.contains(Flag::Zero));
    }
}

#[test]
fn tzcnt_forms() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x43600,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[9] = 1 << 41;
    let high = X86ScalarDecoder::decode(&[0xf3, 0x4d, 0x0f, 0xbc, 0xc1], cpu.rip).unwrap();
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, high),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[8], 41);

    cpu.rip = 0x43700;
    memory = ModelMemory {
        base: 0x43711,
        bytes: (1_u64 << 39).to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let rip = X86ScalarDecoder::decode(&[0xf3, 0x48, 0x0f, 0xbc, 0x05, 0x08, 0x00, 0x00, 0x00], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, rip),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 39);

    cpu.rip = 0x43800;
    cpu.registers[9] = 0x1000;
    cpu.registers[10] = 7;
    memory = ModelMemory {
        base: 0x1011,
        bytes: (1_u64 << 27).to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let sib = X86ScalarDecoder::decode(&[0xf3, 0x47, 0x0f, 0xbc, 0x44, 0x51, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, sib),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[8], 27);

    cpu.rip = 0x43900;
    let original = cpu.clone();
    memory.fail_read = true;
    let fault = X86ScalarDecoder::decode(&[0xf3, 0x47, 0x0f, 0xbc, 0x44, 0x51, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 4
    ));
    assert_eq!(cpu, original);
    for bytes in [&[0xf2, 0x0f, 0xbc, 0xc0][..], &[0xf0, 0xf3, 0x0f, 0xbc, 0xc0]] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_err());
    }
}

#[test]
fn high_half_moves() {
    let low = 0x1122_3344_5566_7788_u64;
    let high = 0x99aa_bbcc_ddee_ff00_u64;
    let loaded = 0x0123_4567_89ab_cdef_u64;
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x44000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[8] = u128::from(low) | (u128::from(high) << 64);
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: loaded.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x44, 0x0f, 0x16, 0x43, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8], u128::from(low) | (u128::from(loaded) << 64));

    cpu.rip = 0x45000;
    cpu.vectors[8] = u128::from(low) | (u128::from(high) << 64);
    let store = X86ScalarDecoder::decode(&[0x66, 0x44, 0x0f, 0x17, 0x43, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(u64::from_le_bytes(memory.bytes[..8].try_into().unwrap()), high);
    assert_eq!(cpu.vectors[8], u128::from(low) | (u128::from(high) << 64));
}

#[test]
fn high_half_faults() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x46000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1001;
    cpu.vectors[0] = 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: vec![0xaa; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x0f, 0x16, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);

    memory.fail_read = false;
    memory.fail_write = true;
    let before = memory.bytes.clone();
    let store = X86ScalarDecoder::decode(&[0x0f, 0x17, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert_eq!(memory.bytes, before);
}

#[test]
fn high_half_forms() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x46f00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = 0xaaaa_bbbb_cccc_dddd_1111_2222_3333_4444;
    cpu.vectors[1] = 0xeeee_ffff_0000_1111_5555_6666_7777_8888;
    let move_low_high = X86ScalarDecoder::decode(&[0x0f, 0x16, 0xc1], cpu.rip).unwrap();
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, move_low_high),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], 0x5555_6666_7777_8888_1111_2222_3333_4444);
    assert!(X86ScalarDecoder::decode(&[0x0f, 0x17, 0xc1], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x16, 0x00], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x16, 0x00], 0).is_ok());
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x17, 0x00], 0).is_err());
}

#[test]
fn low_half_moves() {
    let low = 0x1122_3344_5566_7788_u64;
    let high = 0x99aa_bbcc_ddee_ff00_u64;
    let loaded = 0x0123_4567_89ab_cdef_u64;
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x46800,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[11] = 0x1000;
    cpu.vectors[8] = u128::from(low) | (u128::from(high) << 64);
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: loaded.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x12, 0x43, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8], u128::from(loaded) | (u128::from(high) << 64));

    cpu.rip = 0x46900;
    cpu.vectors[8] = u128::from(low) | (u128::from(high) << 64);
    let store = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x13, 0x43, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(u64::from_le_bytes(memory.bytes[..8].try_into().unwrap()), low);

    cpu.vectors[0] = u128::from(low) | (u128::from(high) << 64);
    cpu.vectors[1] = u128::from(loaded) << 64;
    let movhlps = X86ScalarDecoder::decode(&[0x0f, 0x12, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, movhlps),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], u128::from(loaded) | (u128::from(high) << 64));
}

#[test]
fn low_half_contract() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x46a00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1001;
    cpu.vectors[0] = 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: vec![0xaa; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x12, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    memory.fail_read = false;
    memory.fail_write = true;
    let before = memory.bytes.clone();
    let store = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x13, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert_eq!(memory.bytes, before);

    assert!(X86ScalarDecoder::decode(&[0x66, 0x0f, 0x12, 0xc1], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0x0f, 0x13, 0xc1], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x12, 0x00], 0).is_ok());
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x12, 0x00], 0).is_ok());
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x66, 0x0f, 0x12, 0x00], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0x0f, 0x12, 0x00], 0).is_ok());
}

#[test]
fn sse3_duplicate_moves() {
    let words = 1_u128 | (2_u128 << 32) | (3_u128 << 64) | (4_u128 << 96);
    let original_flags = FlagState::from_bits(0x8d5);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x46b00,
            flags: original_flags,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[9] = words;
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: 0x0123_4567_89ab_cdef_u64.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };

    let ddup = X86ScalarDecoder::decode(&[0xf2, 0x45, 0x0f, 0x12, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, ddup),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[8],
        1_u128 | (2_u128 << 32) | (1_u128 << 64) | (2_u128 << 96)
    );

    cpu.rip = 0x46b10;
    let low = X86ScalarDecoder::decode(&[0xf3, 0x45, 0x0f, 0x12, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, low),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[8],
        1_u128 | (1_u128 << 32) | (3_u128 << 64) | (3_u128 << 96)
    );

    cpu.rip = 0x46b20;
    let high = X86ScalarDecoder::decode(&[0xf3, 0x45, 0x0f, 0x16, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, high),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[8],
        2_u128 | (2_u128 << 32) | (4_u128 << 64) | (4_u128 << 96)
    );

    cpu.rip = 0x46b30;
    let memory_ddup = X86ScalarDecoder::decode(&[0xf2, 0x44, 0x0f, 0x12, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, memory_ddup),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8], 0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
    assert_eq!(cpu.flags, original_flags);

    cpu.rip = 0x4016e8;
    memory.base = 0x4881b0;
    let corpus_ddup = X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x12, 0x25, 0xc0, 0x6a, 0x08, 0x00], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, corpus_ddup),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[4], 0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);

    for bytes in [
        &[0xf2, 0x0f, 0x16, 0xc1][..],
        &[0x66, 0xf2, 0x0f, 0x12, 0xc1],
        &[0xf0, 0xf3, 0x0f, 0x12, 0xc1],
    ] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_err());
    }
}

#[test]
fn sse3_duplicate_faults_transactionally() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x46c00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[0] = 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0xaa; 8],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };

    let full = X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x12, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, full),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);

    memory.fail_read = true;
    let half = X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x12, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, half),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
}

#[test]
fn vex_sse3_duplicate_widths() {
    let low = 1_u128 | (2_u128 << 32) | (3_u128 << 64) | (4_u128 << 96);
    let upper = 5_u128 | (6_u128 << 32) | (7_u128 << 64) | (8_u128 << 96);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x46d00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = low;
    cpu.vector_upper[2] = upper;
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };

    let low_single = X86ScalarDecoder::decode(&[0xc5, 0xfa, 0x12, 0xca], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, low_single),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[1],
        1_u128 | (1_u128 << 32) | (3_u128 << 64) | (3_u128 << 96)
    );
    assert_eq!(cpu.vector_upper[1], 0);

    cpu.rip = 0x46d10;
    let high_single = X86ScalarDecoder::decode(&[0xc5, 0xfa, 0x16, 0xca], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, high_single),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[1],
        2_u128 | (2_u128 << 32) | (4_u128 << 64) | (4_u128 << 96)
    );

    cpu.rip = 0x46d20;
    let wide_double = X86ScalarDecoder::decode(&[0xc5, 0xff, 0x12, 0xca], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, wide_double),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[1],
        1_u128 | (2_u128 << 32) | (1_u128 << 64) | (2_u128 << 96)
    );
    assert_eq!(
        cpu.vector_upper[1],
        5_u128 | (6_u128 << 32) | (5_u128 << 64) | (6_u128 << 96)
    );

    cpu.rip = 0x46d30;
    cpu.registers[3] = 0x1000;
    memory.bytes.truncate(8);
    memory.bytes[..8].copy_from_slice(&0x0123_4567_89ab_cdef_u64.to_le_bytes());
    let memory_double = X86ScalarDecoder::decode(&[0xc5, 0xfb, 0x12, 0x0b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, memory_double),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 0x0123_4567_89ab_cdef_0123_4567_89ab_cdef);
    assert_eq!(cpu.vector_upper[1], 0);

    cpu.rip = 0x46d40;
    memory.bytes = [11_u64, 12, 13, 14].into_iter().flat_map(u64::to_le_bytes).collect();
    let wide_memory = X86ScalarDecoder::decode(&[0xc5, 0xff, 0x12, 0x0b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, wide_memory),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 11_u128 | (11_u128 << 64));
    assert_eq!(cpu.vector_upper[1], 13_u128 | (13_u128 << 64));

    cpu.rip = 0x46d50;
    memory.bytes.truncate(24);
    let original = cpu.clone();
    let wide_fault = X86ScalarDecoder::decode(&[0xc5, 0xff, 0x12, 0x0b], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, wide_fault),
        ExecutionExit::OperandFault(access) if access.length() == 32
    ));
    assert_eq!(cpu, original);
}

#[test]
fn scalar_double_moves() {
    let low = 0x0123_4567_89ab_cdef_u64;
    let high = 0xfedc_ba98_7654_3210_u64;
    let loaded = 0x4009_21fb_5444_2d18_u64;
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x47000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[8] = u128::from(low) | (u128::from(high) << 64);
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: loaded.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0xf2, 0x44, 0x0f, 0x10, 0x43, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8], u128::from(loaded));

    cpu.rip = 0x47100;
    cpu.vectors[8] = u128::from(low) | (u128::from(high) << 64);
    let store = X86ScalarDecoder::decode(&[0xf2, 0x44, 0x0f, 0x11, 0x43, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(u64::from_le_bytes(memory.bytes[..8].try_into().unwrap()), low);

    cpu.vectors[0] = u128::from(low) | (u128::from(high) << 64);
    cpu.vectors[1] = u128::from(loaded);
    let register = X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x10, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, register),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], u128::from(loaded) | (u128::from(high) << 64));
}

#[test]
fn scalar_double_contract() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x47200,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1001;
    cpu.vectors[0] = u128::MAX;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: vec![0xaa; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x10, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    memory.fail_read = false;
    memory.fail_write = true;
    let before = memory.bytes.clone();
    let store = X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x11, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert_eq!(memory.bytes, before);
    assert!(X86ScalarDecoder::decode(&[0xf0, 0xf2, 0x0f, 0x10, 0x03], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0x66, 0xf2, 0x0f, 0x10, 0x03], 0).is_ok());
}

#[test]
fn scalar_single_moves() {
    let upper = 0x1122_3344_5566_7788_99aa_bbcc_u128 << 32;
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x47300,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[8] = upper | u128::from(1.0_f32.to_bits());
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: 2.5_f32.to_bits().to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0xf3, 0x44, 0x0f, 0x10, 0x43, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8], u128::from(2.5_f32.to_bits()));

    cpu.rip = 0x47400;
    cpu.vectors[8] = upper | u128::from(1.0_f32.to_bits());
    cpu.vectors[9] = u128::from(3.5_f32.to_bits());
    let register = X86ScalarDecoder::decode(&[0xf3, 0x45, 0x0f, 0x10, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, register),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8], upper | u128::from(3.5_f32.to_bits()));

    cpu.rip = 0x47500;
    let store = X86ScalarDecoder::decode(&[0xf3, 0x44, 0x0f, 0x11, 0x43, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(memory.bytes, 3.5_f32.to_bits().to_le_bytes());

    cpu.rip = 0x47580;
    memory.fail_read = true;
    let original = cpu.clone();
    let fault = X86ScalarDecoder::decode(&[0xf3, 0x44, 0x0f, 0x10, 0x43, 0x01], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 4
    ));
    assert_eq!(cpu, original);
}

#[test]
fn scalar_single_arithmetic() {
    let cases = [
        (0x58, 1.5_f32, 2.25_f32, 3.75_f32),
        (0x59, 1.5, 2.25, 3.375),
        (0x5c, 1.5, 2.25, -0.75),
        (0x5e, 1.5, 2.0, 0.75),
        (0x51, 123.0, 9.0, 3.0),
    ];
    for (opcode, left, right, expected) in cases {
        let upper = 0xfeed_face_cafe_beef_0123_4567_u128 << 32;
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x47600,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[8] = upper | u128::from(left.to_bits());
        cpu.vectors[9] = u128::from(right.to_bits());
        let instruction = X86ScalarDecoder::decode(&[0xf3, 0x45, 0x0f, opcode, 0xc1], cpu.rip).unwrap();
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(
            cpu.vectors[8],
            upper | u128::from(expected.to_bits()),
            "opcode={opcode:#x}"
        );
    }
}

#[test]
fn packed_float_arithmetic() {
    fn singles(values: [f32; 4]) -> u128 {
        let mut bytes = [0_u8; 16];
        for (index, value) in values.into_iter().enumerate() {
            bytes[index * 4..index * 4 + 4].copy_from_slice(&value.to_bits().to_le_bytes());
        }
        u128::from_le_bytes(bytes)
    }
    fn doubles(values: [f64; 2]) -> u128 {
        u128::from(values[0].to_bits()) | (u128::from(values[1].to_bits()) << 64)
    }

    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    for (prefix, opcode, left, right, expected) in [
        (
            None,
            0x58,
            singles([1.0, 2.0, 3.0, 4.0]),
            singles([0.5, 1.0, 1.5, 2.0]),
            singles([1.5, 3.0, 4.5, 6.0]),
        ),
        (
            None,
            0x5c,
            singles([1.0, 2.0, 3.0, 4.0]),
            singles([0.5, 1.0, 1.5, 2.0]),
            singles([0.5, 1.0, 1.5, 2.0]),
        ),
        (
            Some(0x66),
            0x59,
            doubles([1.5, 4.0]),
            doubles([2.0, 0.5]),
            doubles([3.0, 2.0]),
        ),
        (
            Some(0x66),
            0x51,
            doubles([99.0, 88.0]),
            doubles([9.0, 16.0]),
            doubles([3.0, 4.0]),
        ),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x47680,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[0] = left;
        cpu.vectors[1] = right;
        let mut bytes: Vec<u8> = prefix.into_iter().collect();
        bytes.extend_from_slice(&[0x0f, opcode, 0xc1]);
        let instruction = X86ScalarDecoder::decode(&bytes, cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[0], expected, "prefix={prefix:?} opcode={opcode:#x}");
    }
}

#[test]
fn packed_float_nan_and_fault() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x476c0,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = u128::from(0x7fc0_1234_u32) | (u128::from(0.0_f32.to_bits()) << 32);
    cpu.vectors[1] = u128::from(0x7f80_0001_u32) | (u128::from(f32::INFINITY.to_bits()) << 32);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let multiply = X86ScalarDecoder::decode(&[0x0f, 0x59, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, multiply),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u32, 0x7fc0_1234);
    assert_eq!((cpu.vectors[0] >> 32) as u32, 0xffc0_0000);
    assert_ne!(cpu.mxcsr & 1, 0);

    cpu.rip = 0x476e0;
    cpu.registers[3] = 0x1000;
    let original = cpu.clone();
    memory.base = 0x1000;
    memory.bytes = vec![0; 8];
    let load = X86ScalarDecoder::decode(&[0x0f, 0x58, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);
}

#[test]
fn packed_float_unpack() {
    let left = u128::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
    let right = u128::from_le_bytes([
        0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8d, 0x8e, 0x8f,
    ]);
    let forms = [
        (
            &[0x0f, 0x14, 0xc1][..],
            u128::from_le_bytes([0, 1, 2, 3, 0x80, 0x81, 0x82, 0x83, 4, 5, 6, 7, 0x84, 0x85, 0x86, 0x87]),
        ),
        (
            &[0x0f, 0x15, 0xc1][..],
            u128::from_le_bytes([
                8, 9, 10, 11, 0x88, 0x89, 0x8a, 0x8b, 12, 13, 14, 15, 0x8c, 0x8d, 0x8e, 0x8f,
            ]),
        ),
        (
            &[0x66, 0x0f, 0x14, 0xc1][..],
            u128::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7, 0x80, 0x81, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87]),
        ),
        (
            &[0x66, 0x0f, 0x15, 0xc1][..],
            u128::from_le_bytes([
                8, 9, 10, 11, 12, 13, 14, 15, 0x88, 0x89, 0x8a, 0x8b, 0x8c, 0x8d, 0x8e, 0x8f,
            ]),
        ),
    ];
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    for (bytes, expected) in forms {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x47700,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[0] = left;
        cpu.vectors[1] = right;
        let instruction = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[0], expected, "bytes={bytes:02x?}");
    }
}

#[test]
fn packed_float_unpack_fault() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x47740,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[0] = u128::MAX;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 8],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let instruction = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x14, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::OperandFault(access) if access.length() == 16 && access.address() == 0x1008
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x14, 0xc1], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x15, 0xc1], 0).is_err());
}

#[test]
fn byte_shuffle_control() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x47780,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[8] = u128::from_le_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
    cpu.vectors[9] = u128::from_le_bytes([0, 1, 15, 16, 31, 0x7f, 0x80, 0x8f, 2, 3, 4, 5, 6, 7, 8, 9]);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let instruction = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x38, 0x00, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[8].to_le_bytes(),
        [0, 1, 15, 0, 15, 15, 0, 0, 2, 3, 4, 5, 6, 7, 8, 9]
    );

    cpu.rip = 0x477c0;
    cpu.vectors[0] = u128::from_le_bytes([15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1, 0]);
    let alias = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x38, 0x00, 0xc0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, alias),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[0].to_le_bytes(),
        [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]
    );
}

#[test]
fn byte_shuffle_memory_fault() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x47800,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[0] = u128::MAX;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 8],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let instruction = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x38, 0x00, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::OperandFault(access) if access.length() == 16 && access.address() == 0x1008
    ));
    assert_eq!(cpu, original);
    for bytes in [
        &[0x0f, 0x38, 0x00, 0xc0][..],
        &[0xf2, 0x0f, 0x38, 0x00, 0xc0],
        &[0xf3, 0x0f, 0x38, 0x00, 0xc0],
        &[0xf0, 0x66, 0x0f, 0x38, 0x00, 0xc0],
    ] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_err());
    }
}

#[test]
fn single_nan_rounding() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let add = X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x58, 0xc1], 0x47700).unwrap();
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x47700,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = u128::from(0x7fc0_1234_u32);
    cpu.vectors[1] = u128::from(0x7f80_0001_u32);
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, add),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u32, 0x7fc0_1234);
    assert_ne!(cpu.mxcsr & 1, 0);

    cpu.rip = 0x47700;
    cpu.mxcsr = 0x1f80;
    cpu.vectors[0] = 0;
    cpu.vectors[1] = u128::from(f32::INFINITY.to_bits());
    let multiply = X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x59, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, multiply),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u32, 0xffc0_0000);
    assert_ne!(cpu.mxcsr & 1, 0);

    cpu.rip = 0x47700;
    cpu.mxcsr = 0x1f80;
    cpu.vectors[0] = u128::from(1.0_f32.to_bits());
    cpu.vectors[1] = 0;
    let divide = X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x5e, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, divide),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u32, f32::INFINITY.to_bits());
    assert_ne!(cpu.mxcsr & (1 << 2), 0);

    cpu.rip = 0x47700;
    cpu.mxcsr = 0x1f80;
    cpu.vectors[1] = u128::from((-1.0_f32).to_bits());
    let square_root = X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x51, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, square_root),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u32, 0xffc0_0000);
    assert_ne!(cpu.mxcsr & 1, 0);

    for (rounding, expected) in [(0, 1.0_f32.to_bits()), (2, 1.0_f32.to_bits() + 1)] {
        cpu.rip = 0x47700;
        cpu.mxcsr = 0x1f80 | rounding << 13;
        cpu.vectors[0] = u128::from(1.0_f32.to_bits());
        cpu.vectors[1] = u128::from((2_f32.powi(-24)).to_bits());
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, add),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[0] as u32, expected);
        assert_ne!(cpu.mxcsr & (1 << 5), 0);
    }
}

#[test]
fn scalar_single_conversions() {
    let upper = 0xfeed_face_cafe_beef_dead_beef_u128 << 32;
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x47800,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[1] = u64::from((-3_i32) as u32);
    cpu.vectors[0] = upper;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let from_integer = X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x2a, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, from_integer),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], upper | u128::from((-3.0_f32).to_bits()));

    cpu.rip = 0x47900;
    cpu.vectors[0] = u128::from(0x1234_5678_9abc_def0_u64) << 64;
    cpu.vectors[1] = u128::from(1.25_f32.to_bits());
    let widen = X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x5a, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, widen),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u64, 1.25_f64.to_bits());
    assert_eq!((cpu.vectors[0] >> 64) as u64, 0x1234_5678_9abc_def0);

    cpu.rip = 0x47a00;
    cpu.vectors[0] = 0;
    cpu.vectors[1] = u128::from(3.5_f32.to_bits());
    let to_integer = X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x2d, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, to_integer),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 4);
}

#[test]
fn legacy_scalar_single_integer_conversion_contract() {
    let upper = 0xfeed_face_dead_beef_cafe_babe_u128 << 32;
    let flags = FlagState::from_bits(0x8d5);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };

    for (rounding, signed, expected) in [
        (0, 16_777_217_i64, 16_777_216.0_f32),
        (1, 16_777_217, 16_777_216.0),
        (2, 16_777_217, 16_777_218.0),
        (3, -16_777_217, -16_777_216.0),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x47840,
                flags,
                ..Default::default()
            },
            mxcsr: 0x1f80 | rounding << 13,
            ..Default::default()
        };
        cpu.registers[9] = signed as u64;
        cpu.vectors[8] = upper;
        let convert = X86ScalarDecoder::decode(&[0xf3, 0x4d, 0x0f, 0x2a, 0xc1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, convert),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[8], upper | u128::from(expected.to_bits()));
        assert_ne!(cpu.mxcsr & (1 << 5), 0, "rounding={rounding}");
        assert_eq!(cpu.flags, flags);
    }

    for (wide, source, expected, invalid) in [
        (false, 2.9_f32, 2_u64, false),
        (false, f32::NAN, 0x8000_0000, true),
        (true, 9_223_372_036_854_775_808.0_f32, 0x8000_0000_0000_0000, true),
        (true, -9_223_372_036_854_775_808.0_f32, 0x8000_0000_0000_0000, false),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x47860,
                flags,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[8] = u64::MAX;
        cpu.vectors[9] = u128::from(source.to_bits());
        let bytes = if wide {
            &[0xf3, 0x4d, 0x0f, 0x2c, 0xc1][..]
        } else {
            &[0xf3, 0x45, 0x0f, 0x2c, 0xc1]
        };
        let convert = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        let vectors = cpu.vectors;
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, convert),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[8], expected);
        assert_eq!(cpu.mxcsr & 1 != 0, invalid);
        assert_eq!(cpu.flags, flags);
        assert_eq!(cpu.vectors, vectors);
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x47880,
            flags,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[0] = upper;
    let original = cpu.clone();
    memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let from_memory = X86ScalarDecoder::decode(&[0xf3, 0x48, 0x0f, 0x2a, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);

    let to_memory = X86ScalarDecoder::decode(&[0xf3, 0x48, 0x0f, 0x2c, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, to_memory),
        ExecutionExit::OperandFault(access) if access.length() == 4
    ));
    assert_eq!(cpu, original);
}

#[test]
fn packed_width_conversions() {
    fn singles(values: [u32; 2]) -> u128 {
        u128::from(values[0]) | u128::from(values[1]) << 32
    }
    fn doubles(values: [u64; 2]) -> u128 {
        u128::from(values[0]) | u128::from(values[1]) << 64
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x47a40,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[1] = singles([1.25_f32.to_bits(), (-3.5_f32).to_bits()]);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let widen = X86ScalarDecoder::decode(&[0x0f, 0x5a, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, widen),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], doubles([1.25_f64.to_bits(), (-3.5_f64).to_bits()]));

    cpu.rip = 0x47a80;
    cpu.vectors[0] = u128::MAX;
    cpu.vectors[1] = doubles([2.5_f64.to_bits(), (-7.0_f64).to_bits()]);
    let narrow = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x5a, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, narrow),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], singles([2.5_f32.to_bits(), (-7.0_f32).to_bits()]));

    for (bytes, instruction) in [(7, vec![0x0f, 0x5a, 0x03]), (15, vec![0x66, 0x0f, 0x5a, 0x03])] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x47ac0,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[3] = 0x5000;
        cpu.vectors[0] = u128::MAX;
        let original = cpu.clone();
        let mut short = ModelMemory {
            base: 0x5000,
            bytes: vec![0; bytes],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let decoded = X86ScalarDecoder::decode(&instruction, cpu.rip).unwrap();
        assert!(matches!(
            ScalarInterpreter::execute(&mut cpu, &mut short, decoded),
            ExecutionExit::OperandFault(access) if access.length() == if bytes == 7 { 8 } else { 16 }
        ));
        assert_eq!(cpu, original);
    }
}

#[test]
fn packed_single_conversions() {
    fn dwords(values: [u32; 4]) -> u128 {
        values
            .into_iter()
            .enumerate()
            .fold(0, |packed, (lane, value)| packed | u128::from(value) << (lane * 32))
    }

    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x47b00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[9] = dwords([0, 1, (-1_i32) as u32, 16_777_217]);
    let from_integer = X86ScalarDecoder::decode(&[0x45, 0x0f, 0x5b, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, from_integer),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[8],
        dwords([
            0.0_f32.to_bits(),
            1.0_f32.to_bits(),
            (-1.0_f32).to_bits(),
            16_777_216.0_f32.to_bits()
        ])
    );
    assert_ne!(cpu.mxcsr & (1 << 5), 0);

    let source = dwords([
        1.5_f32.to_bits(),
        (-1.5_f32).to_bits(),
        2_147_483_648.0_f32.to_bits(),
        0x7fc0_1234,
    ]);
    for (prefix, rounding, expected) in [
        (0x66, 0_u32, [2, (-2_i32) as u32, 0x8000_0000, 0x8000_0000]),
        (0x66, 2, [2, (-1_i32) as u32, 0x8000_0000, 0x8000_0000]),
        (0xf3, 0, [1, (-1_i32) as u32, 0x8000_0000, 0x8000_0000]),
    ] {
        cpu.rip = 0x47c00;
        cpu.mxcsr = 0x1f80 | rounding << 13;
        cpu.vectors[8] = u128::MAX;
        cpu.vectors[9] = source;
        let instruction = X86ScalarDecoder::decode(&[prefix as u8, 0x45, 0x0f, 0x5b, 0xc1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(
            cpu.vectors[8],
            dwords(expected),
            "prefix={prefix:#x} rounding={rounding}"
        );
        assert_ne!(cpu.mxcsr & 1, 0);
    }
}

#[test]
fn packed_single_conversion_fault() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x47d00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[0] = u128::MAX;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 8],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let instruction = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x5b, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::OperandFault(access) if access.length() == 16 && access.address() == 0x1008
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x5b, 0xc0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x0f, 0x5b, 0xc0], 0).is_err());
}

#[test]
fn vex_packed_single_conversion_family() {
    let pack = |values: [u32; 4]| {
        values
            .into_iter()
            .enumerate()
            .fold(0_u128, |bits, (lane, value)| bits | (u128::from(value) << (lane * 32)))
    };
    let source = pack([
        1.75_f32.to_bits(),
        (-1.75_f32).to_bits(),
        2_147_483_648.0_f32.to_bits(),
        f32::NAN.to_bits(),
    ]);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x47d80,
            ..Default::default()
        },
        mxcsr: 0x1f80 | (2 << 13),
        ..Default::default()
    };
    cpu.vectors[3] = source;
    cpu.vector_upper[4] = u128::MAX;
    let mut memory = ModelMemory {
        base: 0x2000,
        bytes: vec![0; 32],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let rounded = X86ScalarDecoder::decode(&[0xc5, 0xf9, 0x5b, 0xe3], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, rounded),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[4], pack([2, (-1_i32) as u32, 0x8000_0000, 0x8000_0000]));
    assert_eq!(cpu.vector_upper[4], 0);
    assert_ne!(cpu.mxcsr & 1, 0);
    assert_ne!(cpu.mxcsr & (1 << 5), 0);

    cpu.rip = 0x47d90;
    cpu.mxcsr = 0x1f80;
    let truncated = X86ScalarDecoder::decode(&[0xc5, 0xfa, 0x5b, 0xe3], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, truncated),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[4], pack([1, (-1_i32) as u32, 0x8000_0000, 0x8000_0000]));

    cpu.rip = 0x47da0;
    cpu.registers[3] = 0x2000;
    let low = pack([0, 1, (-1_i32) as u32, 16_777_217]);
    let high = pack([2, 3, 4, 5]);
    memory.bytes[..16].copy_from_slice(&low.to_le_bytes());
    memory.bytes[16..].copy_from_slice(&high.to_le_bytes());
    let integers = X86ScalarDecoder::decode(&[0xc5, 0xfc, 0x5b, 0x0b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, integers),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1] as u32, 0.0_f32.to_bits());
    assert_eq!((cpu.vectors[1] >> 32) as u32, 1.0_f32.to_bits());
    assert_eq!(cpu.vector_upper[1] as u32, 2.0_f32.to_bits());

    cpu.rip = 0x47db0;
    cpu.mxcsr = 0x1f80 | (1 << 6);
    cpu.vectors[3] = 1;
    let daz = X86ScalarDecoder::decode(&[0xc5, 0xf9, 0x5b, 0xe3], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, daz),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[4] as u32, 0);
    assert_eq!(cpu.mxcsr & 0x3f, 0);

    assert!(X86ScalarDecoder::decode(&[0xc5, 0xfb, 0x5b, 0xe3], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xe9, 0x5b, 0xe3], 0).is_err());
}

#[test]
fn vex_float_width_conversion_family() {
    let singles = |values: [f32; 4]| {
        values.into_iter().enumerate().fold(0_u128, |bits, (lane, value)| {
            bits | (u128::from(value.to_bits()) << (lane * 32))
        })
    };
    let doubles = |values: [f64; 2]| u128::from(values[0].to_bits()) | (u128::from(values[1].to_bits()) << 64);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x47dc0,
            ..Default::default()
        },
        mxcsr: 0x1f80,
        ..Default::default()
    };
    cpu.vectors[1] = singles([1.5, -2.25, 3.5, -4.25]);
    cpu.vector_upper[0] = u128::MAX;
    let mut memory = ModelMemory {
        base: 0x2000,
        bytes: vec![0; 32],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let widen = X86ScalarDecoder::decode(&[0xc5, 0xf8, 0x5a, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, widen),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], doubles([1.5, -2.25]));
    assert_eq!(cpu.vector_upper[0], 0);

    cpu.rip = 0x47dd0;
    cpu.vectors[1] = doubles([1.5, -2.25]);
    cpu.vector_upper[0] = u128::MAX;
    let narrow = X86ScalarDecoder::decode(&[0xc5, 0xf9, 0x5a, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, narrow),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], singles([1.5, -2.25, 0.0, 0.0]));
    assert_eq!(cpu.vector_upper[0], 0);

    cpu.rip = 0x47de0;
    cpu.vectors[2] = doubles([77.0, 88.0]);
    cpu.vectors[3] = u128::from(1.25_f32.to_bits());
    cpu.vector_upper[1] = u128::MAX;
    let scalar_widen = X86ScalarDecoder::decode(&[0xc5, 0xea, 0x5a, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, scalar_widen),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], doubles([1.25, 88.0]));
    assert_eq!(cpu.vector_upper[1], 0);

    cpu.rip = 0x47df0;
    cpu.registers[3] = 0x2000;
    memory.bytes[..16].copy_from_slice(&singles([1.0, 2.0, 3.0, 4.0]).to_le_bytes());
    let wide = X86ScalarDecoder::decode(&[0xc5, 0xfc, 0x5a, 0x0b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, wide),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], doubles([1.0, 2.0]));
    assert_eq!(cpu.vector_upper[1], doubles([3.0, 4.0]));

    cpu.rip = 0x47e00;
    cpu.mxcsr = 0x1f80 | (1 << 6);
    cpu.vectors[1] = 1;
    let daz = X86ScalarDecoder::decode(&[0xc5, 0xf8, 0x5a, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, daz),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u64, 0);
    assert_eq!(cpu.mxcsr & 0x3f, 0);

    cpu.rip = 0x47e10;
    cpu.mxcsr = 0x1f80;
    cpu.registers[3] = 0x2000;
    let original = cpu.clone();
    memory.fail_read = true;
    let fault = X86ScalarDecoder::decode(&[0xc5, 0xfc, 0x5a, 0x0b], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);

    assert!(X86ScalarDecoder::decode(&[0xc5, 0xec, 0x5a, 0xcb], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xfe, 0x5a, 0xcb], 0).is_err());
}

#[test]
fn packed_single_segments() {
    fn dwords(values: [u32; 4]) -> u128 {
        values
            .into_iter()
            .enumerate()
            .fold(0, |packed, (lane, value)| packed | u128::from(value) << (lane * 32))
    }
    let integers = dwords([0, 1, (-1_i32) as u32, 16_777_217]);
    let floats = dwords([
        1.5_f32.to_bits(),
        (-1.5_f32).to_bits(),
        2.0_f32.to_bits(),
        (-2.0_f32).to_bits(),
    ]);
    for (prefix, source, expected) in [
        (
            None,
            integers,
            dwords([
                0.0_f32.to_bits(),
                1.0_f32.to_bits(),
                (-1.0_f32).to_bits(),
                16_777_216.0_f32.to_bits(),
            ]),
        ),
        (Some(0x66), floats, dwords([2, (-2_i32) as u32, 2, (-2_i32) as u32])),
        (Some(0xf3), floats, dwords([1, (-1_i32) as u32, 2, (-2_i32) as u32])),
    ] {
        for (segment, base) in [(0x64, 0x5000), (0x65, 0x6000)] {
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x47e00,
                    fs_base: 0x5000,
                    gs_base: 0x6000,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.vectors[9] = source;
            let mut bytes: Vec<u8> = [segment].into_iter().chain(prefix).collect();
            bytes.extend_from_slice(&[0x45, 0x0f, 0x5b, 0xc1]);
            let register = X86ScalarDecoder::decode(&bytes, cpu.rip).unwrap();
            let mut memory = ModelMemory {
                base: 0,
                bytes: vec![],
                fail_read: false,
                fail_write: false,
                commits: 0,
            };
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, register),
                ExecutionExit::Continue
            );
            assert_eq!(cpu.vectors[8], expected);

            cpu.rip = 0x47e40;
            cpu.registers[3] = 0x20;
            memory.base = base + 0x20;
            memory.bytes = source.to_le_bytes().to_vec();
            bytes.truncate(usize::from(prefix.is_some()) + 1);
            bytes.extend_from_slice(&[0x44, 0x0f, 0x5b, 0x03]);
            let load = X86ScalarDecoder::decode(&bytes, cpu.rip).unwrap();
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, load),
                ExecutionExit::Continue
            );
            assert_eq!(cpu.vectors[8], expected);
        }
    }
}

#[test]
fn legacy_float_extrema_select_x86_operands() {
    fn dwords(values: [u32; 4]) -> u128 {
        values
            .into_iter()
            .enumerate()
            .fold(0, |packed, (lane, value)| packed | u128::from(value) << (lane * 32))
    }

    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x47e00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[8] = dwords([3.0_f32.to_bits(), (-2.0_f32).to_bits(), 0.0_f32.to_bits(), 0x7fc0_1234]);
    cpu.vectors[9] = dwords([
        2.0_f32.to_bits(),
        (-1.0_f32).to_bits(),
        (-0.0_f32).to_bits(),
        5.0_f32.to_bits(),
    ]);
    let minps = X86ScalarDecoder::decode(&[0x45, 0x0f, 0x5d, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, minps),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[8],
        dwords([
            2.0_f32.to_bits(),
            (-2.0_f32).to_bits(),
            (-0.0_f32).to_bits(),
            5.0_f32.to_bits()
        ])
    );
    assert_eq!(cpu.mxcsr & 1, 0);

    cpu.rip = 0x47e80;
    cpu.vectors[8] = dwords([(-0.0_f32).to_bits(); 4]);
    cpu.vectors[9] = dwords([0.0_f32.to_bits(); 4]);
    let zero_min = X86ScalarDecoder::decode(&[0x45, 0x0f, 0x5d, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, zero_min),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8], dwords([0.0_f32.to_bits(); 4]));

    cpu.rip = 0x47f00;
    cpu.vectors[8] = u128::from(4.0_f64.to_bits()) | u128::from(7.0_f64.to_bits()) << 64;
    cpu.vectors[9] = u128::from(0x7ff0_0000_0000_1234_u64) | u128::from(8.0_f64.to_bits()) << 64;
    let maxpd = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x5f, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, maxpd),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[8],
        u128::from(0x7ff0_0000_0000_1234_u64) | u128::from(8.0_f64.to_bits()) << 64
    );
    assert_ne!(cpu.mxcsr & 1, 0);
}

#[test]
fn legacy_float_extrema_preserve_scalar_lanes_and_faults() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x48000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[8] = u128::from(3.0_f32.to_bits()) | 0xfeed_face_cafe_beef_dead_beef_u128 << 32;
    cpu.vectors[9] = u128::from(2.0_f32.to_bits());
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let minss = X86ScalarDecoder::decode(&[0xf3, 0x45, 0x0f, 0x5d, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, minss),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8] as u32, 2.0_f32.to_bits());
    assert_eq!(cpu.vectors[8] >> 32, 0xfeed_face_cafe_beef_dead_beef_u128);

    cpu.rip = 0x48100;
    cpu.registers[3] = 0x1000;
    let original = cpu.clone();
    let mut short = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 7],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let maxsd = X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x5f, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut short, maxsd),
        ExecutionExit::OperandFault(access) if access.length() == 8 && access.address() == 0x1000
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x0f, 0x5d, 0xc0], 0).is_err());
}

#[test]
fn legacy_float_extrema_segments() {
    fn dwords(value: u32) -> u128 {
        (0..4).fold(0, |packed, lane| packed | u128::from(value) << (lane * 32))
    }
    fn qwords(value: u64) -> u128 {
        u128::from(value) | u128::from(value) << 64
    }
    let high = 0xfeed_face_cafe_beef_u128 << 64;
    for (prefix, left, right, minimum) in [
        (
            None,
            dwords(2.0_f32.to_bits()),
            dwords(1.0_f32.to_bits()),
            dwords(1.0_f32.to_bits()),
        ),
        (
            Some(0x66),
            qwords(2.0_f64.to_bits()),
            qwords(1.0_f64.to_bits()),
            qwords(1.0_f64.to_bits()),
        ),
        (
            Some(0xf3),
            high | u128::from(2.0_f32.to_bits()),
            u128::from(1.0_f32.to_bits()),
            high | u128::from(1.0_f32.to_bits()),
        ),
        (
            Some(0xf2),
            high | u128::from(2.0_f64.to_bits()),
            u128::from(1.0_f64.to_bits()),
            high | u128::from(1.0_f64.to_bits()),
        ),
    ] {
        for (segment, base) in [(0x64, 0x7000), (0x65, 0x8000)] {
            for (opcode, expected) in [(0x5d, minimum), (0x5f, left)] {
                let mut cpu = CpuState {
                    scalar: ScalarState {
                        rip: 0x48200,
                        fs_base: 0x7000,
                        gs_base: 0x8000,
                        ..Default::default()
                    },
                    ..Default::default()
                };
                cpu.vectors[8] = left;
                cpu.vectors[9] = right;
                let mut bytes: Vec<u8> = [segment].into_iter().chain(prefix).collect();
                bytes.extend_from_slice(&[0x45, 0x0f, opcode, 0xc1]);
                let register = X86ScalarDecoder::decode(&bytes, cpu.rip).unwrap();
                let mut memory = ModelMemory {
                    base: 0,
                    bytes: vec![],
                    fail_read: false,
                    fail_write: false,
                    commits: 0,
                };
                assert_eq!(
                    ScalarInterpreter::execute(&mut cpu, &mut memory, register),
                    ExecutionExit::Continue
                );
                assert_eq!(cpu.vectors[8], expected);

                cpu.rip = 0x48240;
                cpu.registers[3] = 0x20;
                cpu.vectors[8] = left;
                memory.base = base + 0x20;
                memory.bytes = right.to_le_bytes().to_vec();
                bytes.truncate(usize::from(prefix.is_some()) + 1);
                bytes.extend_from_slice(&[0x44, 0x0f, opcode, 0x03]);
                let load = X86ScalarDecoder::decode(&bytes, cpu.rip).unwrap();
                assert_eq!(
                    ScalarInterpreter::execute(&mut cpu, &mut memory, load),
                    ExecutionExit::Continue
                );
                assert_eq!(cpu.vectors[8], expected);
            }
        }
    }
}

#[test]
fn sse3_pair_arithmetic_and_fault() {
    fn singles(values: [f32; 4]) -> u128 {
        values.into_iter().enumerate().fold(0, |packed, (lane, value)| {
            packed | u128::from(value.to_bits()) << (lane * 32)
        })
    }
    fn doubles(values: [f64; 2]) -> u128 {
        u128::from(values[0].to_bits()) | u128::from(values[1].to_bits()) << 64
    }

    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    for (opcode, expected) in [
        (0x7c, [3.0, 7.0, 11.0, 15.0]),
        (0x7d, [-1.0, -1.0, -1.0, -1.0]),
        (0xd0, [-4.0, 8.0, -4.0, 12.0]),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x48140,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[0] = singles([1.0, 2.0, 3.0, 4.0]);
        cpu.vectors[1] = singles([5.0, 6.0, 7.0, 8.0]);
        let decoded = X86ScalarDecoder::decode(&[0xf2, 0x0f, opcode, 0xc1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[0], singles(expected));
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x48180,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = doubles([1.0, 2.0]);
    cpu.vectors[1] = doubles([3.0, 4.0]);
    let haddpd = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x7c, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, haddpd),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], doubles([3.0, 7.0]));

    cpu.rip = 0x481c0;
    cpu.registers[3] = 0x6000;
    let original = cpu.clone();
    let mut short = ModelMemory {
        base: 0x6000,
        bytes: vec![0; 15],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x7c, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut short, load),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0x0f, 0x7c, 0xc1], 0).is_err());
}

#[test]
fn vector_variable_shifts_and_fault() {
    let flags = FlagState::from_bits(u16::MAX);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x481e0,
            flags,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[5] = (0..8).fold(0_u128, |packed, lane| {
        packed | u128::from(if lane & 1 == 0 { 0x8000_u16 } else { 4 }) << (lane * 16)
    });
    cpu.vectors[7] = 1;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let psraw = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xe1, 0xef], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, psraw),
        ExecutionExit::Continue
    );
    let expected = (0..8).fold(0_u128, |packed, lane| {
        packed | u128::from(if lane & 1 == 0 { 0xc000_u16 } else { 2 }) << (lane * 16)
    });
    assert_eq!((cpu.vectors[5], cpu.flags), (expected, flags));

    cpu.rip = 0x48200;
    cpu.registers[3] = 0x6800;
    let original = cpu.clone();
    let mut short = ModelMemory {
        base: 0x6800,
        bytes: vec![0; 15],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xd2, 0x2b], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut short, load),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0xe1, 0xef], 0).is_err());
}

#[test]
fn legacy_float_compare_masks_and_predicate_aliases() {
    fn dwords(values: [u32; 4]) -> u128 {
        values
            .into_iter()
            .enumerate()
            .fold(0, |packed, (lane, value)| packed | u128::from(value) << (lane * 32))
    }

    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x48200,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[8] = dwords([1.0_f32.to_bits(), 2.0_f32.to_bits(), 0x7fc0_1234, 4.0_f32.to_bits()]);
    cpu.vectors[9] = dwords([1.0_f32.to_bits(), 3.0_f32.to_bits(), 5.0_f32.to_bits(), 0x7f80_1234]);
    let neq = X86ScalarDecoder::decode(&[0x45, 0x0f, 0xc2, 0xc1, 0xc4], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, neq),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8], dwords([0, u32::MAX, u32::MAX, u32::MAX]));
    assert_ne!(
        cpu.mxcsr & 1,
        0,
        "the signaling NaN raises invalid for a quiet predicate"
    );

    cpu.rip = 0x48300;
    cpu.mxcsr = 0x1f80;
    cpu.vectors[8] = u128::from(0x7ff8_0000_0000_1234_u64) | u128::from(0xfeed_face_cafe_beef_u64) << 64;
    cpu.vectors[9] = u128::from(1.0_f64.to_bits());
    let nlt = X86ScalarDecoder::decode(&[0xf2, 0x45, 0x0f, 0xc2, 0xc1, 0x05], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, nlt),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[8],
        u128::from(u64::MAX) | u128::from(0xfeed_face_cafe_beef_u64) << 64
    );
    assert_ne!(cpu.mxcsr & 1, 0, "a signaling predicate raises invalid for a quiet NaN");
}

#[test]
fn legacy_float_compare_scalar_memory_fault_is_atomic() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x48400,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[0] = u128::MAX;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 3],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let instruction = X86ScalarDecoder::decode(&[0xf3, 0x0f, 0xc2, 0x03, 0x00], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::OperandFault(access) if access.length() == 4 && access.address() == 0x1000
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x0f, 0xc2, 0xc0, 0], 0).is_err());
}

#[test]
fn mxcsr_control_roundtrip() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x48000,
            ..Default::default()
        },
        ..Default::default()
    };
    assert_eq!(cpu.mxcsr, 0x1f80);
    cpu.registers[3] = 0x1000;
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: 0x5f80_u32.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x0f, 0xae, 0x53, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.mxcsr, cpu.rip), (0x5f80, 0x48004));

    cpu.rip = 0x48100;
    cpu.mxcsr = 0x3f80;
    memory.bytes.fill(0);
    let store = X86ScalarDecoder::decode(&[0x0f, 0xae, 0x5b, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(u32::from_le_bytes(memory.bytes[..4].try_into().unwrap()), 0x3f80);
}

#[test]
fn mxcsr_control_contract() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x48200,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1001;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: vec![0; 4],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x0f, 0xae, 0x13], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 4
    ));
    assert_eq!(cpu, original);
    memory.fail_read = false;
    memory.fail_write = true;
    let store = X86ScalarDecoder::decode(&[0x0f, 0xae, 0x1b], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::OperandFault(access) if access.length() == 4
    ));
    assert_eq!(cpu, original);

    memory.fail_write = false;
    memory.bytes = 0x0001_0000_u32.to_le_bytes().to_vec();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::UndefinedInstruction {
            instruction: original.rip
        }
    );
    assert_eq!(cpu, original);
    for bytes in [
        &[0x0f, 0xae, 0xd0][..],
        &[0x66, 0x0f, 0xae, 0x13],
        &[0xf0, 0x0f, 0xae, 0x1b],
    ] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_err());
    }
}

#[test]
fn fxsave_image() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x49000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.x87_control = 0x0b7f;
    cpu.mxcsr = 0x5f80;
    for index in 0..16 {
        cpu.vectors[index] = u128::from(index as u64 + 1) * 0x0101_0101_0101_0101_0101_0101_0101_0101;
    }
    let architectural = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0xaa; 512],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let save = X86ScalarDecoder::decode(&[0x0f, 0xae, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, save),
        ExecutionExit::Continue
    );
    assert_eq!(u16::from_le_bytes(memory.bytes[0..2].try_into().unwrap()), 0x0b7f);
    assert_eq!(u32::from_le_bytes(memory.bytes[24..28].try_into().unwrap()), 0x5f80);
    assert_eq!(u32::from_le_bytes(memory.bytes[28..32].try_into().unwrap()), 0xffff);
    assert!(memory.bytes[2..24].iter().all(|byte| *byte == 0));
    assert!(memory.bytes[32..160].iter().all(|byte| *byte == 0));
    for index in 0..16 {
        assert_eq!(
            u128::from_le_bytes(memory.bytes[160 + index * 16..176 + index * 16].try_into().unwrap()),
            architectural.vectors[index]
        );
    }
    assert!(memory.bytes[416..].iter().all(|byte| *byte == 0));
    assert_eq!(cpu.rip, architectural.rip + 3);
    assert_eq!(cpu.vectors, architectural.vectors);
}

#[test]
fn fxsave_contract() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x49100,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1008;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1008,
        bytes: vec![0xaa; 512],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let save = X86ScalarDecoder::decode(&[0x0f, 0xae, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, save),
        ExecutionExit::AlignmentFault { .. }
    ));
    assert_eq!(cpu, original);
    assert!(memory.bytes.iter().all(|byte| *byte == 0xaa));
    cpu.registers[3] = 0x1000;
    memory.base = 0x1000;
    memory.fail_write = true;
    let aligned = cpu.clone();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, save),
        ExecutionExit::OperandFault(access) if access.length() == 512
    ));
    assert_eq!(cpu, aligned);
    assert!(memory.bytes.iter().all(|byte| *byte == 0xaa));

    for group in 0..8_u8 {
        let decoded = X86ScalarDecoder::decode(&[0x0f, 0xae, group << 3], 0);
        assert_eq!(decoded.is_ok(), matches!(group, 0..=3), "group={group}");
        let register = X86ScalarDecoder::decode(&[0x0f, 0xae, 0xc0 | group << 3], 0);
        if matches!(group, 5..=7) {
            assert_eq!(register.unwrap().instruction, ScalarInstruction::Nop);
        } else {
            assert!(register.is_err());
        }
    }
    for prefix in [0x66, 0xf2, 0xf3, 0xf0] {
        assert!(X86ScalarDecoder::decode(&[prefix, 0x0f, 0xae, 0x00], 0).is_err());
    }
}

#[test]
fn fxsave_mmx_alias() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x49180,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.write_mmx(3, 0x8877_6655_4433_2211);
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 512],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let save = X86ScalarDecoder::decode(&[0x0f, 0xae, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, save),
        ExecutionExit::Continue
    );
    cpu.empty_mmx();
    cpu.rip = 0x49190;
    let restore = X86ScalarDecoder::decode(&[0x0f, 0xae, 0x0b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, restore),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.read_mmx(3), 0x8877_6655_4433_2211);
    assert_ne!(cpu.x87_classes[3], ExtendedClass::Empty);
}

#[test]
fn fxrstor_image() {
    let mut image = vec![0_u8; 512];
    image[0..2].copy_from_slice(&0xffff_u16.to_le_bytes());
    image[24..28].copy_from_slice(&0x5f80_u32.to_le_bytes());
    let vectors: [u128; 16] =
        std::array::from_fn(|index| u128::from(index as u64 + 3) * 0x0102_0304_0506_0708_1112_1314_1516_1718);
    for (index, vector) in vectors.iter().enumerate() {
        image[160 + index * 16..176 + index * 16].copy_from_slice(&vector.to_le_bytes());
    }
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x49200,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors.fill(u128::MAX);
    cpu.mxcsr = 0x1f80;
    let registers = cpu.registers;
    let flags = cpu.flags;
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: image,
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let restore = X86ScalarDecoder::decode(&[0x0f, 0xae, 0x0b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, restore),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.x87_control, cpu.mxcsr, cpu.rip), (0x1f7f, 0x5f80, 0x49203));
    assert_eq!(cpu.vectors, vectors);
    assert_eq!(cpu.registers, registers);
    assert_eq!(cpu.flags, flags);
}

#[test]
fn fxrstor_contract() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x49300,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors.fill(0xaaaa);
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 504],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let restore = X86ScalarDecoder::decode(&[0x0f, 0xae, 0x0b], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, restore),
        ExecutionExit::OperandFault(access) if matches!(access.fault(), MemoryFault {
                address: 0x11f8,
                access: AccessKind::Read,
                ..
            }) && access.length() == 512
    ));
    assert_eq!(cpu, original);

    memory.bytes.resize(512, 0);
    memory.bytes[24..28].copy_from_slice(&0x0001_0000_u32.to_le_bytes());
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, restore),
        ExecutionExit::UndefinedInstruction {
            instruction: original.rip
        }
    );
    assert_eq!(cpu, original);
    cpu.registers[3] = 0x1008;
    let unaligned = cpu.clone();
    memory.base = 0x1008;
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, restore),
        ExecutionExit::AlignmentFault {
            access: AccessKind::Read,
            ..
        }
    ));
    assert_eq!(cpu, unaligned);
    for prefix in [0x66, 0xf2, 0xf3, 0xf0] {
        assert!(X86ScalarDecoder::decode(&[prefix, 0x0f, 0xae, 0x08], 0).is_err());
    }
}

#[test]
fn cvtsd2si_rounding_modes() {
    for (rounding, positive, negative) in [(0, 3, -3), (1, 2, -3), (2, 3, -2), (3, 2, -2)] {
        for (value, expected) in [(2.7_f64, positive), (-2.7_f64, negative)] {
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x4a000,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.mxcsr = 0x1f80 | rounding << 13;
            cpu.vectors[1] = u128::from(value.to_bits());
            cpu.flags = FlagState::from_bits(0x8d5);
            let vectors = cpu.vectors;
            let convert = X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x2d, 0xd1], cpu.rip).unwrap();
            let mut memory = ModelMemory {
                base: 0,
                bytes: vec![],
                fail_read: false,
                fail_write: false,
                commits: 0,
            };
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, convert),
                ExecutionExit::Continue
            );
            assert_eq!(
                cpu.registers[2], expected as u32 as u64,
                "rounding={rounding} value={value}"
            );
            assert_ne!(cpu.mxcsr & (1 << 5), 0);
            assert_eq!(cpu.flags.bits(), 0x8d5);
            assert_eq!(cpu.vectors, vectors);
        }
    }
}

#[test]
fn vex_scalar_to_integer_rounding_modes() {
    for (rounding, expected) in [(0, 3_u64), (1, 2), (2, 3), (3, 2)] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x4a080,
                ..Default::default()
            },
            mxcsr: 0x1f80 | rounding << 13,
            ..Default::default()
        };
        cpu.vectors[3] = u128::from(2.7_f32.to_bits());
        let rounded = X86ScalarDecoder::decode(&[0xc5, 0xfa, 0x2d, 0xc3], cpu.rip).unwrap();
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, rounded),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[0], expected);
        assert_ne!(cpu.mxcsr & (1 << 5), 0);

        cpu.rip = 0x4a090;
        cpu.registers[0] = u64::MAX;
        let truncated = X86ScalarDecoder::decode(&[0xc5, 0xfa, 0x2c, 0xc3], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, truncated),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[0], 2);
    }
}

#[test]
fn scalar_float_to_integer_denormal_raises_only_precision() {
    for bytes in [
        &[0xf3, 0x0f, 0x2d, 0xc1][..],
        &[0xf2, 0x0f, 0x2d, 0xc1][..],
        &[0xc5, 0xfa, 0x2d, 0xc1][..],
        &[0xc5, 0xfb, 0x2d, 0xc1][..],
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x4a0c0,
                ..Default::default()
            },
            mxcsr: 0x1f80,
            ..Default::default()
        };
        cpu.vectors[1] = 1;
        let instruction = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[0], 0);
        assert_eq!(cpu.mxcsr & 0x3f, 1 << 5);
    }
}

#[test]
fn cvtsd2si_boundaries() {
    let cases = [
        (2_147_483_647_f64, false, 0x0000_0000_7fff_ffff_u64, false),
        (2_147_483_648_f64, false, 0x0000_0000_8000_0000, true),
        (-2_147_483_648_f64, false, 0x0000_0000_8000_0000, false),
        (f64::NAN, false, 0x0000_0000_8000_0000, true),
        (9_223_372_036_854_775_808_f64, true, 0x8000_0000_0000_0000, true),
        (-9_223_372_036_854_775_808_f64, true, 0x8000_0000_0000_0000, false),
    ];
    for (value, wide, expected, invalid) in cases {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x4a100,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[8] = u64::MAX;
        cpu.vectors[9] = u128::from(value.to_bits());
        let bytes = if wide {
            &[0xf2, 0x4d, 0x0f, 0x2d, 0xc1][..]
        } else {
            &[0xf2, 0x45, 0x0f, 0x2d, 0xc1]
        };
        let convert = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, convert),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[8], expected);
        assert_eq!(cpu.mxcsr & 1 != 0, invalid);
    }
}

#[test]
fn cvtsd2si_memory_contract() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4a200,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.registers[2] = u64::MAX;
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: 3.5_f64.to_bits().to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let convert = X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x2d, 0x53, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, convert),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[2], 4);
    cpu.rip = 0x4a300;
    cpu.registers[2] = u64::MAX;
    let original = cpu.clone();
    memory.fail_read = true;
    let fault = X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x2d, 0x53, 0x01], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x2d, 0xc0], 0).is_ok());
    assert!(X86ScalarDecoder::decode(&[0x66, 0xf2, 0x0f, 0x2d, 0xc0], 0).is_ok());
    assert!(X86ScalarDecoder::decode(&[0xf0, 0xf2, 0x0f, 0x2d, 0xc0], 0).is_err());
}

#[test]
fn addsd_rounding() {
    for (rounding, expected) in [
        (0, 1.0_f64.to_bits()),
        (1, 1.0_f64.to_bits()),
        (2, 1.0_f64.to_bits() + 1),
        (3, 1.0_f64.to_bits()),
    ] {
        let upper = 0x1122_3344_5566_7788_u64;
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x4b000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.mxcsr = 0x1f80 | rounding << 13;
        cpu.vectors[8] = u128::from(1.0_f64.to_bits()) | (u128::from(upper) << 64);
        cpu.vectors[9] = u128::from((2_f64.powi(-53)).to_bits());
        let add = X86ScalarDecoder::decode(&[0xf2, 0x45, 0x0f, 0x58, 0xc1], cpu.rip).unwrap();
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, add),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[8] as u64, expected);
        assert_eq!((cpu.vectors[8] >> 64) as u64, upper);
        assert_ne!(cpu.mxcsr & (1 << 5), 0);
    }
}

#[test]
fn addsd_environment() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let add = X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x58, 0xc1], 0x4b100).unwrap();
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4b100,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[1] = 1;
    cpu.mxcsr = 0x1f80;
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, add),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u64, 1);
    assert_ne!(cpu.mxcsr & (1 << 1), 0);

    cpu.rip = 0x4b100;
    cpu.vectors[0] = 0;
    cpu.vectors[1] = 1;
    cpu.mxcsr = 0x1f80 | (1 << 6);
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, add),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u64, 0);
    assert_eq!(cpu.mxcsr & 0x3f, 0);

    cpu.rip = 0x4b100;
    cpu.vectors[0] = 0x0010_0000_0000_0000;
    cpu.vectors[1] = 0x800f_ffff_ffff_ffff;
    cpu.mxcsr = 0x1f80 | (1 << 15);
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, add),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u64, 0);
    assert_ne!(cpu.mxcsr & (1 << 4), 0);

    cpu.rip = 0x4b100;
    cpu.vectors[0] = 0x7ff0_0000_0000_0001;
    cpu.vectors[1] = 0;
    cpu.mxcsr = 0x1f80;
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, add),
        ExecutionExit::Continue
    );
    assert_ne!(cpu.mxcsr & 1, 0);
}

#[test]
fn addsd_memory() {
    let upper = 0xaabb_ccdd_eeff_0011_u64;
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4b200,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[8] = u128::from(1.25_f64.to_bits()) | (u128::from(upper) << 64);
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: 2.5_f64.to_bits().to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let add = X86ScalarDecoder::decode(&[0xf2, 0x44, 0x0f, 0x58, 0x43, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, add),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8] as u64, 3.75_f64.to_bits());
    assert_eq!((cpu.vectors[8] >> 64) as u64, upper);
    cpu.rip = 0x4b300;
    let original = cpu.clone();
    memory.fail_read = true;
    let fault = X86ScalarDecoder::decode(&[0xf2, 0x44, 0x0f, 0x58, 0x43, 0x01], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x58, 0xc0], 0).is_ok());
    assert!(X86ScalarDecoder::decode(&[0x66, 0xf2, 0x0f, 0x58, 0xc0], 0).is_ok());
    assert!(X86ScalarDecoder::decode(&[0xf0, 0xf2, 0x0f, 0x58, 0xc0], 0).is_err());
}

#[test]
fn subsd_rounding() {
    let one = (-1.0_f64).to_bits();
    let next = (-1.0000000000000002_f64).to_bits();
    for (rounding, expected) in [(0, one), (1, next), (2, one), (3, one)] {
        let upper = 0x8877_6655_4433_2211_u64;
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x4b400,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.mxcsr = 0x1f80 | rounding << 13;
        cpu.vectors[8] = u128::from(one) | (u128::from(upper) << 64);
        cpu.vectors[9] = u128::from((2_f64.powi(-53)).to_bits());
        let subtract = X86ScalarDecoder::decode(&[0xf2, 0x45, 0x0f, 0x5c, 0xc1], cpu.rip).unwrap();
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, subtract),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[8] as u64, expected, "rounding={rounding}");
        assert_eq!((cpu.vectors[8] >> 64) as u64, upper);
        assert_ne!(cpu.mxcsr & (1 << 5), 0);
    }
}

#[test]
fn subsd_forms() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4b500,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[8] = u128::from(5.0_f64.to_bits());
    cpu.vectors[9] = u128::from(2.0_f64.to_bits());
    let register = X86ScalarDecoder::decode(&[0xf2, 0x45, 0x0f, 0x5c, 0xc1], cpu.rip).unwrap();
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, register),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8] as u64, 3.0_f64.to_bits());

    cpu.rip = 0x4b600;
    cpu.registers[3] = 0x1000;
    cpu.vectors[8] = u128::from(5.5_f64.to_bits());
    memory = ModelMemory {
        base: 0x1001,
        bytes: 1.25_f64.to_bits().to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let from_memory = X86ScalarDecoder::decode(&[0xf2, 0x44, 0x0f, 0x5c, 0x43, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, from_memory),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8] as u64, 4.25_f64.to_bits());
    cpu.rip = 0x4b700;
    let original = cpu.clone();
    memory.fail_read = true;
    let fault = X86ScalarDecoder::decode(&[0xf2, 0x44, 0x0f, 0x5c, 0x43, 0x01], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x5c, 0xc0], 0).is_ok());
    assert!(X86ScalarDecoder::decode(&[0x66, 0xf2, 0x0f, 0x5c, 0xc0], 0).is_ok());
    assert!(X86ScalarDecoder::decode(&[0xf0, 0xf2, 0x0f, 0x5c, 0xc0], 0).is_err());
}

#[test]
fn mulsd_rounding() {
    let one = 1.0_f64.to_bits();
    let next = one + 1;
    for (rounding, expected) in [(0, one), (1, one), (2, next), (3, one)] {
        let upper = 0x1234_5678_9abc_def0_u64;
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x4b800,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.mxcsr = 0x1f80 | rounding << 13;
        cpu.vectors[8] = u128::from((1.0_f64 + 2_f64.powi(-52)).to_bits()) | (u128::from(upper) << 64);
        cpu.vectors[9] = u128::from((1.0_f64 - 2_f64.powi(-53)).to_bits());
        let multiply = X86ScalarDecoder::decode(&[0xf2, 0x45, 0x0f, 0x59, 0xc1], cpu.rip).unwrap();
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, multiply),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[8] as u64, expected, "rounding={rounding}");
        assert_eq!((cpu.vectors[8] >> 64) as u64, upper);
        assert_ne!(cpu.mxcsr & (1 << 5), 0);
    }
}

#[test]
fn mulsd_environment() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let multiply = X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x59, 0xc1], 0x4b900).unwrap();

    for (left, right, expected) in [
        (0x8000_0000_0000_0000_u64, 3.0_f64.to_bits(), 0x8000_0000_0000_0000),
        (0x8000_0000_0000_0000, (-3.0_f64).to_bits(), 0),
        (
            f64::INFINITY.to_bits(),
            (-2.0_f64).to_bits(),
            f64::NEG_INFINITY.to_bits(),
        ),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x4b900,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[0] = u128::from(left);
        cpu.vectors[1] = u128::from(right);
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, multiply),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[0] as u64, expected);
        assert_eq!(cpu.mxcsr & 1, 0);
    }

    for (left, right) in [
        (0_u64, f64::INFINITY.to_bits()),
        (0x8000_0000_0000_0000, f64::NEG_INFINITY.to_bits()),
        (0x7ff0_0000_0000_0001, 1.0_f64.to_bits()),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x4b900,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[0] = u128::from(left);
        cpu.vectors[1] = u128::from(right);
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, multiply),
            ExecutionExit::Continue
        );
        assert_ne!(cpu.mxcsr & 1, 0);
        assert_eq!(cpu.vectors[0] as u64 & 0x7ff8_0000_0000_0000, 0x7ff8_0000_0000_0000);
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4b900,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = u128::from(0x7ff8_0000_0000_1234_u64);
    cpu.vectors[1] = u128::from(2.0_f64.to_bits());
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, multiply),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.mxcsr & 1, 0);

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4b900,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = u128::from(1_u64);
    cpu.vectors[1] = u128::from(2.0_f64.to_bits());
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, multiply),
        ExecutionExit::Continue
    );
    assert_ne!(cpu.mxcsr & (1 << 1), 0);

    cpu.rip = 0x4b900;
    cpu.mxcsr = 0x1f80 | (1 << 6);
    cpu.vectors[0] = u128::from(1_u64);
    cpu.vectors[1] = u128::from(2.0_f64.to_bits());
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, multiply),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u64, 0);
    assert_eq!(cpu.mxcsr & 0x3f, 0);

    cpu.rip = 0x4b900;
    cpu.mxcsr = 0x1f80;
    cpu.vectors[0] = u128::from(f64::MAX.to_bits());
    cpu.vectors[1] = u128::from(2.0_f64.to_bits());
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, multiply),
        ExecutionExit::Continue
    );
    assert_ne!(cpu.mxcsr & (1 << 3), 0);
    assert_ne!(cpu.mxcsr & (1 << 5), 0);

    cpu.rip = 0x4b900;
    cpu.mxcsr = 0x1f80 | (1 << 15);
    cpu.vectors[0] = u128::from(f64::MIN_POSITIVE.to_bits());
    cpu.vectors[1] = u128::from((0.5_f64 + 2_f64.powi(-53)).to_bits());
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, multiply),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u64, 0);
    assert_ne!(cpu.mxcsr & (1 << 4), 0);
    assert_ne!(cpu.mxcsr & (1 << 5), 0);
}

#[test]
fn mulsd_forms() {
    let upper = 0xfedc_ba98_7654_3210_u64;
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4ba00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[8] = u128::from(1.5_f64.to_bits()) | (u128::from(upper) << 64);
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: 4.0_f64.to_bits().to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let from_memory = X86ScalarDecoder::decode(&[0xf2, 0x44, 0x0f, 0x59, 0x43, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, from_memory),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8] as u64, 6.0_f64.to_bits());
    assert_eq!((cpu.vectors[8] >> 64) as u64, upper);

    cpu.rip = 0x4bc00;
    cpu.vectors[0] = u128::from(3.0_f64.to_bits());
    memory = ModelMemory {
        base: 0x4bc10,
        bytes: 2.0_f64.to_bits().to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let rip_relative = X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x59, 0x05, 0x08, 0x00, 0x00, 0x00], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, rip_relative),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u64, 6.0_f64.to_bits());

    cpu.rip = 0x4bd00;
    cpu.registers[9] = 0x1000;
    cpu.registers[10] = 7;
    cpu.vectors[8] = u128::from(2.0_f64.to_bits()) | (u128::from(upper) << 64);
    memory = ModelMemory {
        base: 0x1011,
        bytes: 2.5_f64.to_bits().to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let sib = X86ScalarDecoder::decode(&[0xf2, 0x47, 0x0f, 0x59, 0x44, 0x51, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, sib),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[8] as u64, 5.0_f64.to_bits());
    assert_eq!((cpu.vectors[8] >> 64) as u64, upper);

    cpu.rip = 0x4bb00;
    let original = cpu.clone();
    memory.fail_read = true;
    let fault = X86ScalarDecoder::decode(&[0xf2, 0x44, 0x0f, 0x59, 0x43, 0x01], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x59, 0xc0], 0).is_ok());
    assert!(X86ScalarDecoder::decode(&[0x66, 0xf2, 0x0f, 0x59, 0xc0], 0).is_ok());
    assert!(X86ScalarDecoder::decode(&[0xf0, 0xf2, 0x0f, 0x59, 0xc0], 0).is_err());
}

#[test]
fn ucomisd_flags() {
    for (left, right, zero, parity, carry) in [
        (1.0_f64, 2.0_f64, false, false, true),
        (2.0, 2.0, true, false, false),
        (3.0, 2.0, false, false, false),
    ] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x4c000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[8] = u128::from(left.to_bits());
        cpu.vectors[9] = u128::from(right.to_bits());
        cpu.flags = FlagState::from_bits(u16::MAX);
        let compare = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x2e, 0xc1], cpu.rip).unwrap();
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, compare),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.flags.contains(Flag::Zero), zero);
        assert_eq!(cpu.flags.contains(Flag::Parity), parity);
        assert_eq!(cpu.flags.contains(Flag::Carry), carry);
        for flag in [Flag::Overflow, Flag::Sign, Flag::Auxiliary] {
            assert!(!cpu.flags.contains(flag));
        }
        assert_ne!(cpu.flags.bits() & (1 << 9), 0);
    }
}

#[test]
fn ucomisd_nan() {
    for (nan, invalid) in [(0x7ff8_0000_0000_0001_u64, false), (0x7ff0_0000_0000_0001, true)] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x4c100,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[0] = u128::from(nan);
        cpu.vectors[1] = u128::from(1.0_f64.to_bits());
        let compare = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x2e, 0xc1], cpu.rip).unwrap();
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, compare),
            ExecutionExit::Continue
        );
        assert!(cpu.flags.contains(Flag::Zero));
        assert!(cpu.flags.contains(Flag::Parity));
        assert!(cpu.flags.contains(Flag::Carry));
        assert_eq!(cpu.mxcsr & 1 != 0, invalid);
    }
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4c100,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = 1;
    cpu.mxcsr = 0x1f80 | (1 << 6);
    let compare = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x2e, 0xc1], cpu.rip).unwrap();
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, compare),
        ExecutionExit::Continue
    );
    assert!(cpu.flags.contains(Flag::Zero));
    assert_eq!(cpu.mxcsr & 0x3f, 0);
}

#[test]
fn ucomisd_memory() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4c200,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = u128::from(1.0_f64.to_bits());
    let address = cpu.rip + 8 + 9;
    let mut memory = ModelMemory {
        base: address,
        bytes: 2.0_f64.to_bits().to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let compare = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x2e, 0x05, 9, 0, 0, 0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, compare),
        ExecutionExit::Continue
    );
    assert!(cpu.flags.contains(Flag::Carry));
    cpu.rip = 0x4c300;
    let original = cpu.clone();
    memory.fail_read = true;
    let fault = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x2e, 0x05, 9, 0, 0, 0], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    for bytes in [
        &[0xf2, 0x0f, 0x2e, 0xc0][..],
        &[0xf3, 0x0f, 0x2e, 0xc0],
        &[0xf0, 0x66, 0x0f, 0x2e, 0xc0],
    ] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_err());
    }
}

#[test]
fn comisd_nan() {
    for nan in [0x7ff8_0000_0000_0001_u64, 0x7ff0_0000_0000_0001] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x4c400,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[8] = u128::from(nan);
        cpu.vectors[9] = u128::from(1.0_f64.to_bits());
        let compare = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x2f, 0xc1], cpu.rip).unwrap();
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, compare),
            ExecutionExit::Continue
        );
        assert!(cpu.flags.contains(Flag::Zero));
        assert!(cpu.flags.contains(Flag::Parity));
        assert!(cpu.flags.contains(Flag::Carry));
        assert_ne!(cpu.mxcsr & 1, 0);
        for flag in [Flag::Overflow, Flag::Sign, Flag::Auxiliary] {
            assert!(!cpu.flags.contains(flag));
        }
    }
}

#[test]
fn comisd_forms() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4c500,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[8] = u128::from(4.0_f64.to_bits());
    cpu.vectors[9] = u128::from(3.0_f64.to_bits());
    let register = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x2f, 0xc1], cpu.rip).unwrap();
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, register),
        ExecutionExit::Continue
    );
    assert!(!cpu.flags.contains(Flag::Zero));
    assert!(!cpu.flags.contains(Flag::Parity));
    assert!(!cpu.flags.contains(Flag::Carry));

    cpu.rip = 0x4c600;
    cpu.vectors[8] = u128::from(1.0_f64.to_bits());
    let address = cpu.rip + 9 + 5;
    memory = ModelMemory {
        base: address,
        bytes: 2.0_f64.to_bits().to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let rip = X86ScalarDecoder::decode(&[0x66, 0x44, 0x0f, 0x2f, 0x05, 5, 0, 0, 0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, rip),
        ExecutionExit::Continue
    );
    assert!(cpu.flags.contains(Flag::Carry));

    cpu.rip = 0x4c700;
    cpu.registers[11] = 0x1000;
    cpu.registers[10] = 0;
    cpu.vectors[8] = 1;
    memory = ModelMemory {
        base: 0x1003,
        bytes: 0_u64.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let sib = X86ScalarDecoder::decode(&[0x66, 0x47, 0x0f, 0x2f, 0x44, 0x53, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, sib),
        ExecutionExit::Continue
    );
    assert!(!cpu.flags.contains(Flag::Carry));
    assert_ne!(cpu.mxcsr & (1 << 1), 0);
    cpu.rip = 0x4c800;
    let original = cpu.clone();
    memory.fail_read = true;
    let fault = X86ScalarDecoder::decode(&[0x66, 0x47, 0x0f, 0x2f, 0x44, 0x53, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    for bytes in [
        &[0xf2, 0x0f, 0x2f, 0xc0][..],
        &[0xf3, 0x0f, 0x2f, 0xc0],
        &[0xf0, 0x66, 0x0f, 0x2f, 0xc0],
    ] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_err());
    }
}

#[test]
fn packed_extrema() {
    let left_bytes = [0, 255, 127, 128, 1, 254, 64, 192, 10, 20, 30, 40, 250, 5, 200, 100];
    let right_bytes = [255, 0, 128, 127, 2, 253, 192, 64, 20, 10, 40, 30, 5, 250, 100, 200];
    for (opcode, choose) in [(0xda, u8::min as fn(u8, u8) -> u8), (0xde, u8::max)] {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x47000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[8] = u128::from_le_bytes(left_bytes);
        cpu.vectors[9] = u128::from_le_bytes(right_bytes);
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let ir = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, opcode, 0xc1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(
            cpu.vectors[8],
            u128::from_le_bytes(std::array::from_fn(|i| choose(left_bytes[i], right_bytes[i])))
        );
    }

    let left_words = [i16::MIN, i16::MAX, -1, 0, 1, -32767, 12345, -12345];
    let right_words = [i16::MAX, i16::MIN, 0, -1, -1, 32767, -12345, 12345];
    for (opcode, choose) in [(0xea, i16::min as fn(i16, i16) -> i16), (0xee, i16::max)] {
        let left = left_words.map(i16::to_le_bytes).concat();
        let right = right_words.map(i16::to_le_bytes).concat();
        let expected = std::array::from_fn::<_, 8, _>(|i| choose(left_words[i], right_words[i]))
            .map(i16::to_le_bytes)
            .concat();
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x48000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[0] = u128::from_le_bytes(left.try_into().unwrap());
        cpu.vectors[1] = u128::from_le_bytes(right.try_into().unwrap());
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let ir = X86ScalarDecoder::decode(&[0x66, 0x0f, opcode, 0xc1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[0], u128::from_le_bytes(expected.try_into().unwrap()));
    }
}

#[test]
fn packed_extrema_memory() {
    let source = [255, 1, 200, 3, 128, 5, 100, 7, 64, 9, 32, 11, 16, 13, 8, 15];
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x49000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[11] = 0x1000;
    cpu.vectors[8] = u128::from_le_bytes([0, 2, 199, 4, 127, 6, 99, 8, 63, 10, 31, 12, 15, 14, 7, 16]);
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: source.to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0xda, 0x43, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[8].to_le_bytes(),
        [0, 1, 199, 3, 127, 5, 99, 7, 63, 9, 31, 11, 15, 13, 7, 15]
    );

    cpu.rip = 0x4a000;
    let original = cpu.clone();
    memory.fail_read = true;
    let fault = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0xde, 0x43, 0x01], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);
    for prefix in [0xf2, 0xf3] {
        assert!(X86ScalarDecoder::decode(&[prefix, 0x0f, 0xda, 0xc0], 0).is_err());
    }
    assert!(matches!(
        X86ScalarDecoder::decode(&[0x0f, 0xda, 0xc0], 0).unwrap().instruction,
        ScalarInstruction::MmxPacked {
            operation: crate::MmxOperation::Extrema { .. },
            ..
        }
    ));
}

#[test]
fn reverse_scan_basis() {
    for (prefixes, width, bits) in [
        (&[0x66][..], ScalarWidth::Word, 16_u32),
        (&[][..], ScalarWidth::Dword, 32),
        (&[0x48][..], ScalarWidth::Qword, 64),
    ] {
        for bit in 0..bits {
            let mut bytes = prefixes.to_vec();
            bytes.extend_from_slice(&[0x0f, 0xbd, 0xc1]);
            let ir = X86ScalarDecoder::decode(&bytes, 0x4b000).unwrap();
            assert_eq!(ir.width, width);
            let flags = FlagState::from_bits((1 << Flag::Carry as u8) | (1 << Flag::Overflow as u8));
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x4b000,
                    flags,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.registers[0] = u64::MAX;
            cpu.registers[1] = 1_u64 << bit;
            let mut memory = ModelMemory {
                base: 0,
                bytes: Vec::new(),
                fail_read: false,
                fail_write: false,
                commits: 0,
            };
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
                ExecutionExit::Continue
            );
            assert_eq!(cpu.read_register(ScalarRegister::General(0), width), u64::from(bit));
            assert!(!cpu.flags.contains(Flag::Zero));
            assert!(cpu.flags.contains(Flag::Carry));
            assert!(cpu.flags.contains(Flag::Overflow));
        }
    }
}

#[test]
fn reverse_scan_zero() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4c000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[8] = 0;
    cpu.registers[9] = 0x1122_3344_5566_7788;
    let flags = FlagState::from_bits((1 << Flag::Carry as u8) | (1 << Flag::Sign as u8));
    cpu.flags = flags;
    let mut memory = ModelMemory {
        base: 0,
        bytes: Vec::new(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let zero = X86ScalarDecoder::decode(&[0x4d, 0x0f, 0xbd, 0xc8], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, zero),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[9], 0x1122_3344_5566_7788);
    assert!(cpu.flags.contains(Flag::Zero));
    assert!(cpu.flags.contains(Flag::Carry));
    assert!(cpu.flags.contains(Flag::Sign));

    cpu.rip = 0x4d000;
    cpu.registers[0] = 0xffff_ffff_8000_0001;
    let narrow = X86ScalarDecoder::decode(&[0x0f, 0xbd, 0xc0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, narrow),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[0], 31);
}

#[test]
fn reverse_scan_memory() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x4e000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1001;
    cpu.registers[8] = u64::MAX;
    let mut memory = ModelMemory {
        base: 0x1001,
        bytes: (1_u64 << 47).to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x4c, 0x0f, 0xbd, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[8], 47);

    cpu.rip = 0x4f000;
    cpu.registers[8] = u64::MAX;
    let original = cpu.clone();
    memory.fail_read = true;
    let fault = X86ScalarDecoder::decode(&[0x4c, 0x0f, 0xbd, 0x03], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, fault),
        ExecutionExit::OperandFault(access) if access.length() == 8
    ));
    assert_eq!(cpu, original);
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x0f, 0xbd, 0xc0], 0).is_err());
}

#[test]
fn compare_decode() {
    for case in 0_u16..1024 {
        let format = if case & 512 == 0 {
            FloatWidth::Single
        } else {
            FloatWidth::Double
        };
        let signaling_only = case & 256 != 0;
        let left = ((case >> 4) & 15) as u8;
        let right = (case & 15) as u8;
        let mut bytes = Vec::new();
        if format == FloatWidth::Double {
            bytes.push(0x66);
        }
        let rex = 0x40 | ((left >> 3) << 2) | (right >> 3);
        if rex != 0x40 {
            bytes.push(rex);
        }
        bytes.extend_from_slice(&[
            0x0f,
            if signaling_only { 0x2e } else { 0x2f },
            0xc0 | ((left & 7) << 3) | (right & 7),
        ]);
        let instruction = X86ScalarDecoder::decode(&bytes, 0x50000).unwrap();
        assert_eq!(
            instruction.instruction,
            ScalarInstruction::VectorScalarCompare {
                left,
                right: VectorSource::Register(right),
                format,
                signaling_only,
            }
        );
    }
    for bytes in [&[0xf2, 0x0f, 0x2e, 0xc0][..], &[0xf3, 0x0f, 0x2f, 0xc0][..]] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_err());
    }
}

#[test]
fn scalar_compare_formats() {
    let cases = [
        (FloatWidth::Single, 1.0_f32.to_bits() as u64, 2.0_f32.to_bits() as u64),
        (FloatWidth::Double, 1.0_f64.to_bits(), 2.0_f64.to_bits()),
    ];
    for (format, lesser, greater) in cases {
        let mut bytes = Vec::new();
        if format == FloatWidth::Double {
            bytes.push(0x66);
        }
        bytes.extend_from_slice(&[0x45, 0x0f, 0x2e, 0xc1]);
        let instruction = X86ScalarDecoder::decode(&bytes, 0x51000).unwrap();
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x51000,
                flags: FlagState::from_bits(u16::MAX),
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[8] = u128::from(lesser) | (u128::MAX << 64);
        cpu.vectors[9] = u128::from(greater) | (u128::MAX << 64);
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert!(cpu.flags.contains(Flag::Carry));
        assert!(!cpu.flags.contains(Flag::Zero));
        assert!(!cpu.flags.contains(Flag::Parity));
        assert!(!cpu.flags.contains(Flag::Overflow));
        assert!(!cpu.flags.contains(Flag::Sign));
        assert!(!cpu.flags.contains(Flag::Auxiliary));
    }
}

#[test]
fn compare_nan_fault() {
    for (prefix, quiet_nan, signaling_nan, denormal) in [
        (&[][..], 0x7fc0_0001_u64, 0x7f80_0001, 1_u64),
        (&[0x66][..], 0x7ff8_0000_0000_0001, 0x7ff0_0000_0000_0001, 1_u64),
    ] {
        for (opcode, operand, invalid) in [
            (0x2e, quiet_nan, false),
            (0x2e, signaling_nan, true),
            (0x2f, quiet_nan, true),
        ] {
            let mut bytes = prefix.to_vec();
            bytes.extend_from_slice(&[0x0f, opcode, 0xc1]);
            let instruction = X86ScalarDecoder::decode(&bytes, 0x52000).unwrap();
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x52000,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.vectors[1] = u128::from(operand);
            let mut memory = ModelMemory {
                base: 0,
                bytes: vec![],
                fail_read: false,
                fail_write: false,
                commits: 0,
            };
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
                ExecutionExit::Continue
            );
            assert!(cpu.flags.contains(Flag::Carry));
            assert!(cpu.flags.contains(Flag::Zero));
            assert!(cpu.flags.contains(Flag::Parity));
            assert_eq!(cpu.mxcsr & 1 != 0, invalid);
        }

        let mut bytes = prefix.to_vec();
        bytes.extend_from_slice(&[0x0f, 0x2e, 0xc1]);
        let instruction = X86ScalarDecoder::decode(&bytes, 0x53000).unwrap();
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x53000,
                ..Default::default()
            },
            mxcsr: 1 << 6,
            ..Default::default()
        };
        cpu.vectors[1] = u128::from(denormal);
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert!(cpu.flags.contains(Flag::Zero));
        assert_eq!(cpu.mxcsr & (1 << 1), 0);
    }

    let instruction = X86ScalarDecoder::decode(&[0x0f, 0x2e, 0x03], 0x54000).unwrap();
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x54000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    let original = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 4],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::OperandFault(access) if access.length() == 4
    ));
    assert_eq!(cpu, original);
}

#[test]
fn vex_scalar_merge_uses_vvvv_and_clears_upper() {
    for (bytes, element_mask) in [
        (&[0xc5, 0xf2, 0x10, 0xc2][..], u128::from(u32::MAX)),
        (&[0xc5, 0xf3, 0x10, 0xc2][..], u128::from(u64::MAX)),
    ] {
        let ir = X86ScalarDecoder::decode(bytes, 0x6000).unwrap();
        assert!(matches!(
            ir.instruction,
            ScalarInstruction::VexScalarMerge {
                destination: 0,
                first: 1,
                second: VectorSource::Register(2),
                ..
            }
        ));
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x6000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[1] = 0x1111_1111_2222_2222_3333_3333_4444_4444;
        cpu.vectors[2] = 0xaaaa_aaaa_bbbb_bbbb_cccc_cccc_dddd_dddd;
        cpu.vector_upper[0] = u128::MAX;
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(
            cpu.vectors[0],
            (0x1111_1111_2222_2222_3333_3333_4444_4444 & !element_mask)
                | (0xaaaa_aaaa_bbbb_bbbb_cccc_cccc_dddd_dddd & element_mask)
        );
        assert_eq!(cpu.vector_upper[0], 0);
    }
}

#[test]
fn vex_transport_and_zero_instructions_observe_upper_rules() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = 0x1234;
    cpu.vector_upper[1] = u128::MAX;
    cpu.vector_upper[2] = 0x5678;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };

    let move128 = X86ScalarDecoder::decode(&[0xc5, 0xfa, 0x6f, 0xca], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, move128),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 0x1234);
    assert_eq!(cpu.vector_upper[1], 0);

    cpu.vector_upper.fill(u128::MAX);
    let zero_upper = X86ScalarDecoder::decode(&[0xc5, 0xf8, 0x77], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, zero_upper),
        ExecutionExit::Continue
    );
    assert!(cpu.vector_upper.iter().all(|value| *value == 0));
    assert_eq!(cpu.vectors[1], 0x1234);

    cpu.vectors.fill(u128::MAX);
    cpu.vector_upper.fill(u128::MAX);
    let zero_all = X86ScalarDecoder::decode(&[0xc5, 0xfc, 0x77], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, zero_all),
        ExecutionExit::Continue
    );
    assert!(cpu.vectors.iter().chain(&cpu.vector_upper).all(|value| *value == 0));
}

#[test]
fn vex_streaming_load_family() {
    for (prefix, wide) in [(0x79, false), (0x7d, true)] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, prefix, 0x2a, 0x0b], 0x7ff0).unwrap();
        assert!(matches!(decoded.instruction, ScalarInstruction::VexVectorTransport {
            vector: 1, operand: VectorSource::Memory(_), store: false, wide: actual,
        } if actual == wide));
    }
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7d, 0x2a, 0xcb], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x75, 0x2a, 0x0b], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7c, 0x2a, 0x0b], 0).is_err());

    for (prefix, wide) in [(0x7b, false), (0x7f, true)] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe1, prefix, 0xf0, 0x0b], 0x7ff4).unwrap();
        assert!(matches!(decoded.instruction, ScalarInstruction::VexVectorTransport {
            vector: 1, operand: VectorSource::Memory(_), store: false, wide: actual,
        } if actual == wide));
    }
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x7f, 0xf0, 0xcb], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x77, 0xf0, 0x0b], 0).is_err());
}

#[test]
fn vex_packed_double_conversion_family() {
    for (prefix, from_integer, truncate, wide) in [
        (0x79, false, true, false),
        (0x7d, false, true, true),
        (0x7a, true, false, false),
        (0x7e, true, false, true),
        (0x7b, false, false, false),
        (0x7f, false, false, true),
    ] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe1, prefix, 0xe6, 0xc1], 0x7ff6).unwrap();
        assert!(
            matches!(decoded.instruction, ScalarInstruction::VexPackedDoubleConvert {
            destination: 0, source: VectorSource::Register(1), from_integer: actual_from,
            truncate: actual_truncate, wide: actual_wide,
        } if (actual_from, actual_truncate, actual_wide) == (from_integer, truncate, wide))
        );
    }
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7ff6,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[1] = u128::from(1.9_f64.to_bits()) | (u128::from((-2.9_f64).to_bits()) << 64);
    cpu.vector_upper[1] = u128::from(3.9_f64.to_bits()) | (u128::from((-4.9_f64).to_bits()) << 64);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let truncate = X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x7d, 0xe6, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, truncate),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[0],
        1 | (u128::from((-2_i32) as u32) << 32) | (3 << 64) | (u128::from((-4_i32) as u32) << 96)
    );
    assert_eq!(cpu.vector_upper[0], 0);

    cpu.rip = 0x7ff7;
    cpu.vectors[1] = 7 | (u128::from((-8_i32) as u32) << 32);
    let widen = X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x7a, 0xe6, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, widen),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[0],
        u128::from(7.0_f64.to_bits()) | (u128::from((-8.0_f64).to_bits()) << 64)
    );
    assert_eq!(cpu.vector_upper[0], 0);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x78, 0xe6, 0xc1], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x73, 0xe6, 0xc1], 0).is_err());
}

#[test]
fn vex_aes_family() {
    use crate::x86::X86AesOperation;
    for (opcode, operation) in [
        (0xdc, X86AesOperation::Encrypt),
        (0xdd, X86AesOperation::EncryptLast),
        (0xde, X86AesOperation::Decrypt),
        (0xdf, X86AesOperation::DecryptLast),
    ] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x69, opcode, 0xd9], 0x7ff9).unwrap();
        assert!(matches!(decoded.instruction, ScalarInstruction::VexAes {
            operation: actual, destination: 3, first: 2, second: VectorSource::Register(1),
        } if actual == operation));
    }
    let inverse = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x79, 0xdb, 0xd9], 0).unwrap();
    assert!(matches!(
        inverse.instruction,
        ScalarInstruction::VexAes {
            operation: X86AesOperation::InverseMix,
            destination: 3,
            first: 0,
            ..
        }
    ));
    let key = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x79, 0xdf, 0xd9, 1], 0).unwrap();
    assert!(matches!(
        key.instruction,
        ScalarInstruction::VexAes {
            operation: X86AesOperation::KeyAssist(1),
            destination: 3,
            first: 0,
            ..
        }
    ));
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7ff9,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[1] = 0xd6ab76fe_daa678f1_d2af72fa_d6aa74fd;
    cpu.vectors[2] = 0xf0e0d0c0_b0a09080_70605040_30201000;
    cpu.vector_upper[3] = u128::MAX;
    let expected = crate::x86::vector::Aes::execute(cpu.vectors[2], cpu.vectors[1], X86AesOperation::Encrypt);
    let encrypt = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x69, 0xdc, 0xd9], cpu.rip).unwrap();
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, encrypt),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[3], expected);
    assert_eq!(cpu.vector_upper[3], 0);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, 0xdb, 0xd9], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7d, 0xdc, 0xd9], 0).is_err());

    cpu.rip = 0x7ffa;
    cpu.vectors[1] = 0x0123_4567_89ab_cdef;
    cpu.vectors[2] = u128::from(0xfedc_ba98_7654_3210_u64) << 64;
    let multiply = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x69, 0x44, 0xd9, 0x01], cpu.rip).unwrap();
    assert!(matches!(
        multiply.instruction,
        ScalarInstruction::VexBinary {
            operation: VexOperation::CarrylessMultiply,
            destination: 3,
            first: 2,
            second: VectorSource::Register(1),
            immediate: 1,
            wide: false,
        }
    ));
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, multiply),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[3],
        VectorLane::carryless_multiply(0xfedc_ba98_7654_3210, 0x0123_4567_89ab_cdef)
    );
    assert_eq!(cpu.vector_upper[3], 0);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x6d, 0x44, 0xd9, 0], 0).is_err());

    cpu.rip = 0x7ffb;
    cpu.vectors[2] = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
    cpu.vector_upper[2] = 0x1f1e_1d1c_1b1a_1918_1716_1514_1312_1110;
    cpu.vectors[1] = 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff;
    cpu.vector_upper[1] = 0xffee_ddcc_bbaa_9988_7766_5544_3322_1100;
    let sad = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x6d, 0x42, 0xd9, 0x27], cpu.rip).unwrap();
    assert!(matches!(
        sad.instruction,
        ScalarInstruction::VexBinary {
            operation: VexOperation::MultipleSad,
            destination: 3,
            first: 2,
            second: VectorSource::Register(1),
            immediate: 0x27,
            wide: true,
        }
    ));
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, sad),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[3], VectorLane::sad(cpu.vectors[2], cpu.vectors[1], 7));
    assert_eq!(
        cpu.vector_upper[3],
        VectorLane::sad(cpu.vector_upper[2], cpu.vector_upper[1], 4)
    );
}

#[test]
fn vex_masked_memory_family() {
    for (opcode, lane, store) in [(0x2c, 4, false), (0x2d, 8, false), (0x2e, 4, true), (0x2f, 8, true)] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, opcode, 0x0b], 0x7ffd).unwrap();
        assert!(matches!(decoded.instruction, ScalarInstruction::VexMaskedMemory {
            vector: 1, mask: 2, lane: actual_lane, store: actual_store, wide: true, ..
        } if (actual_lane, actual_store) == (lane, store)));
    }
    for (prefix, lane) in [(0x6d, 4), (0xed, 8)] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, prefix, 0x8c, 0x0b], 0).unwrap();
        assert!(
            matches!(decoded.instruction, ScalarInstruction::VexMaskedMemory { lane: actual, .. } if actual == lane)
        );
    }
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7ffd,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[1] = u128::MAX;
    cpu.vector_upper[1] = u128::MAX;
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let masked_off = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, 0x2c, 0x0b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, masked_off),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.vectors[1], cpu.vector_upper[1]), (0, 0));
    cpu.rip = 0x7ffe;
    cpu.vectors[1] = 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00;
    cpu.vectors[2] = 1_u128 << 31;
    memory.fail_read = false;
    let store = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x69, 0x2e, 0x0b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(&memory.bytes[..4], &0xddee_ff00_u32.to_le_bytes());
    assert_eq!(memory.commits, 1);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, 0x2c, 0xcb], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6c, 0x2c, 0x0b], 0).is_err());
}

#[test]
fn legacy_streaming_load_and_masked_store() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x70_000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: (0_u8..16).collect(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let load = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x38, 0x2a, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[0],
        u128::from_le_bytes(std::array::from_fn(|index| index as u8))
    );

    cpu.rip = 0x70_100;
    cpu.registers[7] = 0x1000;
    cpu.vectors[0] = u128::from_le_bytes(std::array::from_fn(|index| 0x80 + index as u8));
    cpu.vectors[1] = u128::from_le_bytes(std::array::from_fn(|index| if index & 1 == 0 { 0x80 } else { 0 }));
    let store = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xf7, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(
        memory.bytes,
        (0_u8..16)
            .map(|index| if index & 1 == 0 { 0x80 + index } else { index })
            .collect::<Vec<_>>()
    );

    cpu.rip = 0x70_200;
    cpu.vectors[1] = 0;
    memory.fail_write = true;
    let masked_off = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xf7, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, masked_off),
        ExecutionExit::Continue
    );
}

#[test]
fn vex_vector_test_family() {
    for (opcode, lane) in [(0x17, 0), (0x0e, 4), (0x0f, 8)] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7d, opcode, 0xc1], 0x7ff8).unwrap();
        assert!(matches!(decoded.instruction, ScalarInstruction::VexVectorTest {
            left: 0, right: VectorSource::Register(1), lane: actual, wide: true,
        } if actual == lane));
    }
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7ff8,
            flags: FlagState::from_bits(u16::MAX),
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vector_upper[0] = 1;
    cpu.vector_upper[1] = 1;
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let test = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7d, 0x17, 0xc1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, test),
        ExecutionExit::Continue
    );
    assert!(!cpu.flags.contains(Flag::Zero));
    assert!(cpu.flags.contains(Flag::Carry));
    assert!(!cpu.flags.contains(Flag::Overflow));

    cpu.rip = 0x7ffc;
    cpu.registers[3] = 0x1000;
    memory.fail_read = true;
    let before = cpu.clone();
    let faulting = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7d, 0x0e, 0x0b], cpu.rip).unwrap();
    assert!(matches!(ScalarInterpreter::execute(&mut cpu, &mut memory, faulting),
        ExecutionExit::OperandFault(access) if access.length() == 32));
    assert_eq!(cpu, before);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x75, 0x17, 0xc1], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7c, 0x17, 0xc1], 0).is_err());
}

#[test]
fn vex_horizontal_minimum_word_family() {
    let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x79, 0x41, 0xc1], 0x7ffe).unwrap();
    assert!(matches!(
        decoded.instruction,
        ScalarInstruction::VexBinary {
            operation: VexOperation::HorizontalMinimumWord,
            destination: 0,
            first: 0,
            second: VectorSource::Register(1),
            wide: false,
            ..
        }
    ));
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7ffe,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[1] = [9_u16, 3, 7, 3, 8, 6, 5, 4]
        .into_iter()
        .enumerate()
        .fold(0_u128, |bits, (lane, value)| bits | (u128::from(value) << (lane * 16)));
    cpu.vector_upper[0] = u128::MAX;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], 3 | (1 << 16));
    assert_eq!(cpu.vector_upper[0], 0);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7d, 0x41, 0xc1], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x71, 0x41, 0xc1], 0).is_err());
}

#[test]
fn vex_wide_memory_transport_is_transactional() {
    let address = EffectiveAddress {
        base: Some(3),
        ..Default::default()
    };
    let load = ScalarIr {
        length: 4,
        width: ScalarWidth::Dword,
        instruction: ScalarInstruction::VexVectorTransport {
            vector: 1,
            operand: VectorSource::Memory(address),
            store: false,
            wide: true,
        },
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x8000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    let original = cpu.clone();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut faulting, load),
        ExecutionExit::OperandFault(access) if access.length() == 32
    ));
    assert_eq!(cpu, original);

    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: (0_u8..32).collect(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    let loaded = cpu.clone();
    cpu.rip = 0x9000;
    let store = ScalarIr {
        length: 4,
        width: ScalarWidth::Dword,
        instruction: ScalarInstruction::VexVectorTransport {
            vector: 1,
            operand: VectorSource::Memory(address),
            store: true,
            wide: true,
        },
    };
    memory.bytes.fill(0);
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(memory.commits, 4);
    assert_eq!(cpu.vectors[1], loaded.vectors[1]);
    assert_eq!(cpu.vector_upper[1], loaded.vector_upper[1]);
    assert_eq!(memory.bytes, (0_u8..32).collect::<Vec<_>>());
}

#[test]
fn vex_wide_arithmetic_and_lane_permute() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0xa000,
            ..Default::default()
        },
        ..Default::default()
    };
    let singles = |values: [f32; 4]| {
        values.into_iter().enumerate().fold(0_u128, |bits, (lane, value)| {
            bits | (u128::from(value.to_bits()) << (lane * 32))
        })
    };
    cpu.vectors[1] = singles([1.0, 2.0, 3.0, 4.0]);
    cpu.vector_upper[1] = singles([5.0, 6.0, 7.0, 8.0]);
    cpu.vectors[2] = singles([2.0; 4]);
    cpu.vector_upper[2] = singles([2.0; 4]);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let add = ScalarIr {
        length: 4,
        width: ScalarWidth::Dword,
        instruction: ScalarInstruction::VexBinary {
            operation: VexOperation::AddSingle,
            destination: 0,
            first: 1,
            second: VectorSource::Register(2),
            wide: true,
            immediate: 0,
        },
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, add),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], singles([3.0, 4.0, 5.0, 6.0]));
    assert_eq!(cpu.vector_upper[0], singles([7.0, 8.0, 9.0, 10.0]));
}

#[test]
fn vex_integer_logical_family() {
    for (opcode, operation) in [
        (0xdb, VexOperation::And),
        (0xdf, VexOperation::AndNot),
        (0xeb, VexOperation::Or),
        (0xef, VexOperation::Xor),
    ] {
        for (prefix, wide) in [(0xf1, false), (0xf5, true)] {
            let decoded = X86ScalarDecoder::decode(&[0xc5, prefix, opcode, 0xc2], 0x7000).unwrap();
            assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
                operation: actual, destination: 0, first: 1, second: VectorSource::Register(2), wide: actual_wide, ..
            } if actual == operation && actual_wide == wide));
        }
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7100,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[1] = 0xff00_ff00_ff00_ff00_ff00_ff00_ff00_ff00;
    cpu.vector_upper[1] = u128::MAX;
    cpu.vectors[2] = 0x0f0f_0f0f_0f0f_0f0f_0f0f_0f0f_0f0f_0f0f;
    cpu.vector_upper[2] = 0x5555_5555_5555_5555_5555_5555_5555_5555;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let xor = X86ScalarDecoder::decode(&[0xc5, 0xf5, 0xef, 0xc2], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, xor),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0], cpu.vectors[1] ^ cpu.vectors[2]);
    assert_eq!(cpu.vector_upper[0], cpu.vector_upper[1] ^ cpu.vector_upper[2]);

    cpu.rip = 0x7200;
    cpu.registers[3] = 0x1000;
    cpu.vectors[0] = 0x1234;
    cpu.vector_upper[0] = 0x5678;
    let original = cpu.clone();
    let memory_xor = X86ScalarDecoder::decode(&[0xc5, 0xf5, 0xef, 0x03], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut faulting, memory_xor),
        ExecutionExit::OperandFault(access) if access.length() == 32
    ));
    assert_eq!(cpu, original);
}

#[test]
fn vex_integer_broadcast_family() {
    for (opcode, operation, bytes) in [
        (0x78, VexOperation::BroadcastByte, 1_u64),
        (0x79, VexOperation::BroadcastWord, 2),
        (0x58, VexOperation::BroadcastDword, 4),
        (0x59, VexOperation::BroadcastQword, 8),
    ] {
        for (prefix, wide) in [(0x79, false), (0x7d, true)] {
            let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, prefix, opcode, 0xca], 0x7300).unwrap();
            assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
                operation: actual, destination: 1, second: VectorSource::Register(2), wide: actual_wide, ..
            } if actual == operation && actual_wide == wide));

            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x7300,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.vectors[1] = u128::MAX;
            cpu.vector_upper[1] = u128::MAX;
            cpu.vectors[2] = 0xfedc_ba98_7654_3210;
            let mut memory = ModelMemory {
                base: 0,
                bytes: vec![],
                fail_read: false,
                fail_write: false,
                commits: 0,
            };
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
                ExecutionExit::Continue
            );
            let mask = if bytes == 8 {
                u64::MAX
            } else {
                (1_u64 << (bytes * 8)) - 1
            };
            let element = 0xfedc_ba98_7654_3210 & mask;
            let repeated = (0..16 / bytes).fold(0_u128, |value, lane| {
                value | (u128::from(element) << (lane * bytes * 8))
            });
            assert_eq!(cpu.vectors[1], repeated);
            assert_eq!(cpu.vector_upper[1], if wide { repeated } else { 0 });
        }

        let memory_bytes = [0xc4, 0xe2, 0x7d, opcode, 0x0b];
        let decoded = X86ScalarDecoder::decode(&memory_bytes, 0x7400).unwrap();
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x7400,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[3] = 0x1000;
        cpu.vectors[1] = 0x1234;
        cpu.vector_upper[1] = 0x5678;
        let original = cpu.clone();
        let mut faulting = ModelMemory {
            base: 0x1000,
            bytes: vec![0; bytes as usize],
            fail_read: true,
            fail_write: false,
            commits: 0,
        };
        assert!(matches!(
            ScalarInterpreter::execute(&mut cpu, &mut faulting, decoded),
            ExecutionExit::OperandFault(access) if access.length() == bytes
        ));
        assert_eq!(cpu, original);
    }
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7c, 0x59, 0xca], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0xfd, 0x59, 0xca], 0).is_err());
}

#[test]
fn vex_128_bit_broadcast_family() {
    for opcode in [0x1a, 0x5a] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7d, opcode, 0x0b], 0x7450).unwrap();
        assert!(matches!(
            decoded.instruction,
            ScalarInstruction::VexBinary {
                operation: VexOperation::Broadcast128,
                destination: 1,
                first: 0,
                second: VectorSource::Memory(_),
                wide: true,
                ..
            }
        ));
        let value = 0xfedc_ba98_7654_3210_0123_4567_89ab_cdef_u128;
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x7450,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[3] = 0x1000;
        let mut memory = ModelMemory {
            base: 0x1000,
            bytes: value.to_le_bytes().to_vec(),
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::Continue
        );
        assert_eq!((cpu.vectors[1], cpu.vector_upper[1]), (value, value));
    }
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x79, 0x1a, 0x0b], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7d, 0x1a, 0xcb], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x75, 0x1a, 0x0b], 0).is_err());
}

#[test]
fn vex_variable_packed_shift_family() {
    for (opcode, w, operation) in [
        (0x45, false, VexOperation::ShiftRightVariableDword),
        (0x45, true, VexOperation::ShiftRightVariableQword),
        (0x46, false, VexOperation::ShiftArithmeticVariableDword),
        (0x47, false, VexOperation::ShiftLeftVariableDword),
        (0x47, true, VexOperation::ShiftLeftVariableQword),
    ] {
        for (wide, low) in [(false, 0x79), (true, 0x7d)] {
            let prefix = if w { low | 0x80 } else { low };
            let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, prefix, opcode, 0xca], 0x7500).unwrap();
            assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
                operation: actual, destination: 1, first: 0, second: VectorSource::Register(2), wide: actual_wide, ..
            } if actual == operation && actual_wide == wide));
        }
    }
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0xfd, 0x46, 0xca], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7c, 0x45, 0xca], 0).is_err());

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7600,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = 0x0000_0001_ffff_ffff_0000_0008_8000_0000;
    cpu.vector_upper[0] = 0x8000_0000_7fff_ffff_ffff_ffff_0000_0010;
    cpu.vectors[2] = 0x0000_0000_0000_0020_0000_0003_0000_001f;
    cpu.vector_upper[2] = 0x0000_0028_0000_001f_0000_0020_0000_0004;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let logical = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7d, 0x45, 0xca], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, logical),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 0x0000_0001_0000_0000_0000_0001_0000_0001);
    assert_eq!(cpu.vector_upper[1], 0x0000_0000_0000_0000_0000_0000_0000_0001);

    cpu.rip = 0x7610;
    let arithmetic = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7d, 0x46, 0xca], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, arithmetic),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 0x0000_0001_ffff_ffff_0000_0001_ffff_ffff);
    assert_eq!(cpu.vector_upper[1], 0xffff_ffff_0000_0000_ffff_ffff_0000_0001);

    cpu.registers[3] = 0x1000;
    cpu.rip = 0x7620;
    let original = cpu.clone();
    let memory_shift = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0xfd, 0x45, 0x0b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut faulting, memory_shift),
        ExecutionExit::OperandFault(access) if access.length() == 32
    ));
    assert_eq!(cpu, original);
}

#[test]
fn vex_permute_two_128_lanes() {
    for opcode in [0x06, 0x46] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x7d, opcode, 0xca, 0x21], 0x7700).unwrap();
        assert!(matches!(
            decoded.instruction,
            ScalarInstruction::VexBinary {
                operation: VexOperation::Permute128,
                destination: 1,
                first: 0,
                second: VectorSource::Register(2),
                wide: true,
                immediate: 0x21,
            }
        ));
    }
    for bytes in [
        [0xc4, 0xe3, 0x79, 0x46, 0xca, 0x21],
        [0xc4, 0xe3, 0xfd, 0x46, 0xca, 0x21],
        [0xc4, 0xe3, 0x7c, 0x46, 0xca, 0x21],
    ] {
        assert!(X86ScalarDecoder::decode(&bytes, 0).is_err());
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7700,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[0] = 0xaaaa;
    cpu.vector_upper[0] = 0xbbbb;
    cpu.vectors[2] = 0xcccc;
    cpu.vector_upper[2] = 0xdddd;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let select = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x7d, 0x46, 0xca, 0x21], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, select),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 0xbbbb);
    assert_eq!(cpu.vector_upper[1], 0xcccc);

    cpu.rip = 0x7710;
    let zero = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x7d, 0x46, 0xca, 0x88], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, zero),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 0);
    assert_eq!(cpu.vector_upper[1], 0);

    cpu.rip = 0x7720;
    cpu.registers[3] = 0x1000;
    cpu.vectors[1] = 0x1234;
    cpu.vector_upper[1] = 0x5678;
    let original = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x7d, 0x46, 0x0b, 0x21], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32
    ));
    assert_eq!(cpu, original);
}

#[test]
fn vex_full_lane_permutation_family() {
    for opcode in [0x00, 0x01] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0xfd, opcode, 0xd2, 0xd8], 0x7780).unwrap();
        assert!(matches!(
            decoded.instruction,
            ScalarInstruction::VexBinary {
                operation: VexOperation::PermuteQword,
                destination: 2,
                first: 0,
                second: VectorSource::Register(2),
                wide: true,
                immediate: 0xd8,
            }
        ));
    }
    for opcode in [0x16, 0x36] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, opcode, 0xcb], 0x7780).unwrap();
        assert!(matches!(
            decoded.instruction,
            ScalarInstruction::VexBinary {
                operation: VexOperation::PermuteDword,
                destination: 1,
                first: 2,
                second: VectorSource::Register(3),
                wide: true,
                ..
            }
        ));
    }
    for invalid in [
        &[0xc4, 0xe3, 0xf9, 0x00, 0xcb, 0][..],
        &[0xc4, 0xe3, 0x7d, 0x00, 0xcb, 0],
        &[0xc4, 0xe3, 0xed, 0x00, 0xcb, 0],
        &[0xc4, 0xe2, 0x69, 0x36, 0xcb],
        &[0xc4, 0xe2, 0xed, 0x36, 0xcb],
        &[0xc4, 0xe2, 0x6c, 0x16, 0xcb],
    ] {
        assert!(X86ScalarDecoder::decode(invalid, 0).is_err());
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7780,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = u128::from(0x11_u64) | (u128::from(0x22_u64) << 64);
    cpu.vector_upper[2] = u128::from(0x33_u64) | (u128::from(0x44_u64) << 64);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let qwords = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0xfd, 0x00, 0xd2, 0xd8], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, qwords),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[2], u128::from(0x11_u64) | (u128::from(0x33_u64) << 64));
    assert_eq!(cpu.vector_upper[2], u128::from(0x22_u64) | (u128::from(0x44_u64) << 64));

    cpu.rip = 0x7790;
    cpu.vectors[2] = 2 | (7_u128 << 64) | (4_u128 << 96);
    cpu.vector_upper[2] = 1 | (6_u128 << 32) | (3_u128 << 64) | (5_u128 << 96);
    cpu.vectors[3] = 10 | (20_u128 << 32) | (30_u128 << 64) | (40_u128 << 96);
    cpu.vector_upper[3] = 50 | (60_u128 << 32) | (70_u128 << 64) | (80_u128 << 96);
    let dwords = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, 0x36, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, dwords),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 30 | (10_u128 << 32) | (80_u128 << 64) | (50_u128 << 96));
    assert_eq!(
        cpu.vector_upper[1],
        20 | (70_u128 << 32) | (40_u128 << 64) | (60_u128 << 96)
    );

    cpu.rip = 0x77a0;
    cpu.registers[3] = 0x1000;
    let original = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0xfd, 0x00, 0x0b, 0x1b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32)
    );
    assert_eq!(cpu, original);
}

#[test]
fn vex_lane_local_permutation_family() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x8080,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = 1 | (2 << 32) | (3 << 64) | (4 << 96);
    cpu.vector_upper[2] = 5 | (6 << 32) | (7 << 64) | (8 << 96);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let immediate = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x7d, 0x04, 0xca, 0x93], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, immediate),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 4 | (1 << 32) | (2 << 64) | (3 << 96));
    assert_eq!(cpu.vector_upper[1], 8 | (5 << 32) | (6 << 64) | (7 << 96));

    cpu.rip = 0x8090;
    cpu.vectors[2] = 0x1111 | (0x2222 << 64);
    cpu.vector_upper[2] = 0x3333 | (0x4444 << 64);
    let immediate_double = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x7d, 0x05, 0xca, 0x09], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, immediate_double),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 0x2222 | (0x1111 << 64));
    assert_eq!(cpu.vector_upper[1], 0x3333 | (0x4444 << 64));

    cpu.rip = 0x80a0;
    cpu.vectors[2] = 10 | (20 << 32) | (30 << 64) | (40 << 96);
    cpu.vector_upper[2] = 50 | (60 << 32) | (70 << 64) | (80 << 96);
    cpu.vectors[3] = 3 | (2 << 32) | (1 << 64);
    cpu.vector_upper[3] = 2 | (3 << 32) | (1 << 64);
    let variable = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, 0x0c, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, variable),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 40 | (30 << 32) | (20 << 64) | (10 << 96));
    assert_eq!(cpu.vector_upper[1], 70 | (80 << 32) | (60 << 64) | (50 << 96));

    cpu.rip = 0x80a8;
    cpu.vectors[2] = 0x1111 | (0x2222 << 64);
    cpu.vectors[3] = 2;
    cpu.vector_upper[1] = u128::MAX;
    let variable_double = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x69, 0x0d, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, variable_double),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 0x2222 | (0x1111 << 64));
    assert_eq!(cpu.vector_upper[1], 0);

    cpu.rip = 0x80b0;
    cpu.registers[3] = 0x1000;
    let before = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x7d, 0x04, 0x0b, 0x1b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32
    ));
    assert_eq!(cpu, before);

    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x6d, 0x04, 0xca, 0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x7a, 0x04, 0xca, 0], 0).is_err());
}

#[test]
fn vex_insert_single_family() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x80c0,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[1] = 10 | (20 << 32) | (30 << 64) | (40 << 96);
    cpu.vectors[2] = 100 | (200 << 32) | (300 << 64) | (400 << 96);
    cpu.vector_upper[3] = u128::MAX;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let register = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x71, 0x21, 0xda, 0x69], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, register),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[3], (20 << 32) | (200 << 64));
    assert_eq!(cpu.vector_upper[3], 0);

    cpu.rip = 0x80d0;
    cpu.registers[3] = 0x1000;
    let before = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x71, 0x21, 0x1b, 0x20], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 4],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 4
    ));
    assert_eq!(cpu, before);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x75, 0x21, 0xda, 0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x70, 0x21, 0xda, 0], 0).is_err());
}

#[test]
fn vex_128_lane_transport_family() {
    for opcode in [0x18, 0x38] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x6d, opcode, 0xcb, 1], 0x77b0).unwrap();
        assert!(matches!(
            decoded.instruction,
            ScalarInstruction::VexBinary {
                operation: VexOperation::Insert128,
                destination: 1,
                first: 2,
                second: VectorSource::Register(3),
                wide: true,
                immediate: 1,
            }
        ));
    }
    for opcode in [0x19, 0x39] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x7d, opcode, 0xca, 1], 0x77b0).unwrap();
        assert!(matches!(
            decoded.instruction,
            ScalarInstruction::VexExtract128 {
                source: 1,
                destination: VectorSource::Register(2),
                high: true,
            }
        ));
    }
    for invalid in [
        &[0xc4, 0xe3, 0x69, 0x38, 0xcb, 0][..],
        &[0xc4, 0xe3, 0xed, 0x38, 0xcb, 0],
        &[0xc4, 0xe3, 0x7c, 0x18, 0xcb, 0],
        &[0xc4, 0xe3, 0x79, 0x39, 0xca, 0],
        &[0xc4, 0xe3, 0xfd, 0x39, 0xca, 0],
        &[0xc4, 0xe3, 0x6d, 0x39, 0xca, 0],
    ] {
        assert!(X86ScalarDecoder::decode(invalid, 0).is_err());
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x77b0,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = 0x11;
    cpu.vector_upper[2] = 0x22;
    cpu.vectors[3] = 0x33;
    cpu.vector_upper[3] = 0x44;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let insert_high = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x6d, 0x38, 0xcb, 1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, insert_high),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.vectors[1], cpu.vector_upper[1]), (0x11, 0x33));

    cpu.rip = 0x77c0;
    cpu.vectors[4] = u128::MAX;
    cpu.vector_upper[4] = u128::MAX;
    let extract_low = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x7d, 0x19, 0xcc, 0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, extract_low),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.vectors[4], cpu.vector_upper[4]), (0x11, 0));

    cpu.rip = 0x77d0;
    cpu.registers[3] = 0x1000;
    let extract_memory = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x7d, 0x39, 0x0b, 1], cpu.rip).unwrap();
    let mut output = ModelMemory {
        base: 0x1000,
        bytes: vec![0xaa; 16],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut output, extract_memory),
        ExecutionExit::Continue
    );
    assert_eq!(output.bytes, 0x33_u128.to_le_bytes());
    assert_eq!(output.commits, 2);

    cpu.rip = 0x77e0;
    let original_cpu = cpu.clone();
    let original_memory = vec![0xaa; 16];
    let extract_fault = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x7d, 0x39, 0x0b, 1], cpu.rip).unwrap();
    let mut faulting_write = ModelMemory {
        base: 0x1000,
        bytes: original_memory.clone(),
        fail_read: false,
        fail_write: true,
        commits: 0,
    };
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut faulting_write, extract_fault),
        ExecutionExit::OperandFault(access) if access.length() == 16)
    );
    assert_eq!(cpu, original_cpu);
    assert_eq!(faulting_write.bytes, original_memory);

    cpu.rip = 0x77f0;
    let original_cpu = cpu.clone();
    let insert_memory = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x6d, 0x18, 0x0b, 0], cpu.rip).unwrap();
    let mut faulting_read = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 16],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut faulting_read, insert_memory),
        ExecutionExit::OperandFault(access) if access.length() == 16)
    );
    assert_eq!(cpu, original_cpu);
}

#[test]
fn vex_byte_shuffle_family() {
    for (prefix, wide) in [(0x69, false), (0x6d, true), (0xed, true)] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, prefix, 0x00, 0xcb], 0x77c0).unwrap();
        assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
            operation: VexOperation::ShuffleByte, destination: 1, first: 2,
            second: VectorSource::Register(3), wide: actual_wide, ..
        } if actual_wide == wide));
    }
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6c, 0x00, 0xcb], 0).is_err());

    let data = (0_u8..16).fold(0_u128, |value, lane| {
        value | (u128::from(lane) << (u32::from(lane) * 8))
    });
    let upper = (0_u8..16).fold(0_u128, |value, lane| {
        value | (u128::from(0x40 + lane) << (u32::from(lane) * 8))
    });
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    for control in 0_u8..=u8::MAX {
        let controls = u128::from(control) * 0x0101_0101_0101_0101_0101_0101_0101_0101;
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x77c0,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[2] = data;
        cpu.vector_upper[2] = upper;
        cpu.vectors[3] = controls;
        cpu.vector_upper[3] = controls;
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, 0x00, 0xcb], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::Continue
        );
        let selected = if control & 0x80 == 0 {
            u128::from(control & 15)
        } else {
            0
        };
        let selected_upper = if control & 0x80 == 0 {
            u128::from(0x40 + (control & 15))
        } else {
            0
        };
        assert_eq!(cpu.vectors[1], selected * 0x0101_0101_0101_0101_0101_0101_0101_0101);
        assert_eq!(
            cpu.vector_upper[1],
            selected_upper * 0x0101_0101_0101_0101_0101_0101_0101_0101
        );
    }

    let mut vex_cpu = CpuState {
        scalar: ScalarState {
            rip: 0x77d0,
            ..Default::default()
        },
        ..Default::default()
    };
    vex_cpu.vectors[2] = data;
    vex_cpu.vectors[3] = 0x8f0e_8d0c_8b0a_8908_8706_8504_8302_8100;
    vex_cpu.vector_upper[1] = u128::MAX;
    let mut legacy_cpu = vex_cpu.clone();
    legacy_cpu.rip = 0x77d0;
    legacy_cpu.vectors[1] = data;
    let vex = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x69, 0x00, 0xcb], vex_cpu.rip).unwrap();
    let legacy = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x38, 0x00, 0xcb], legacy_cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut vex_cpu, &mut memory, vex),
        ExecutionExit::Continue
    );
    assert_eq!(
        ScalarInterpreter::execute(&mut legacy_cpu, &mut memory, legacy),
        ExecutionExit::Continue
    );
    assert_eq!(vex_cpu.vectors[1], legacy_cpu.vectors[1]);
    assert_eq!(vex_cpu.vector_upper[1], 0);

    vex_cpu.rip = 0x77e0;
    vex_cpu.registers[3] = 0x1000;
    let original = vex_cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, 0x00, 0x0b], vex_cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(
        matches!(ScalarInterpreter::execute(&mut vex_cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32)
    );
    assert_eq!(vex_cpu, original);
}

#[test]
fn vex_packed_unpack_family() {
    for (opcode, pp, operation) in [
        (0x14, 0, VexOperation::UnpackLowDword),
        (0x14, 1, VexOperation::UnpackLowQword),
        (0x15, 0, VexOperation::UnpackHighDword),
        (0x15, 1, VexOperation::UnpackHighQword),
    ] {
        let prefix = 0xe8 | pp | 4;
        let decoded = X86ScalarDecoder::decode(&[0xc5, prefix, opcode, 0xcb], 0x7800).unwrap();
        assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
            operation: actual, destination: 1, first: 2, second: VectorSource::Register(3),
            wide: true, ..
        } if actual == operation));
    }
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xea, 0x14, 0xcb], 0).is_err());

    for (opcode, operation) in [
        (0x60, VexOperation::UnpackLowByte),
        (0x61, VexOperation::UnpackLowWord),
        (0x62, VexOperation::UnpackLowDword),
        (0x6c, VexOperation::UnpackLowQword),
        (0x68, VexOperation::UnpackHighByte),
        (0x69, VexOperation::UnpackHighWord),
        (0x6a, VexOperation::UnpackHighDword),
        (0x6d, VexOperation::UnpackHighQword),
    ] {
        for (prefix, wide) in [(0xe9, false), (0xed, true)] {
            let decoded = X86ScalarDecoder::decode(&[0xc5, prefix, opcode, 0xcb], 0x7800).unwrap();
            assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
                operation: actual, destination: 1, first: 2, second: VectorSource::Register(3),
                wide: actual_wide, ..
            } if actual == operation && actual_wide == wide));
        }
    }
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xec, 0x6c, 0xcb], 0).is_err());

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7800,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = u128::from(0xaaaa_u64) | (u128::from(0xbbbb_u64) << 64);
    cpu.vector_upper[2] = u128::from(0x1111_u64) | (u128::from(0x2222_u64) << 64);
    cpu.vectors[3] = u128::from(0xcccc_u64) | (u128::from(0xdddd_u64) << 64);
    cpu.vector_upper[3] = u128::from(0x3333_u64) | (u128::from(0x4444_u64) << 64);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let low = X86ScalarDecoder::decode(&[0xc5, 0xed, 0x6c, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, low),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], u128::from(0xaaaa_u64) | (u128::from(0xcccc_u64) << 64));
    assert_eq!(
        cpu.vector_upper[1],
        u128::from(0x1111_u64) | (u128::from(0x3333_u64) << 64)
    );

    cpu.rip = 0x7810;
    let high = X86ScalarDecoder::decode(&[0xc5, 0xe9, 0x6d, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, high),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], u128::from(0xbbbb_u64) | (u128::from(0xdddd_u64) << 64));
    assert_eq!(cpu.vector_upper[1], 0);

    cpu.rip = 0x7820;
    cpu.registers[3] = 0x1000;
    cpu.vectors[1] = 0x1234;
    cpu.vector_upper[1] = 0x5678;
    let original = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc5, 0xed, 0x6c, 0x0b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32
    ));
    assert_eq!(cpu, original);

    cpu.rip = 0x7830;
    cpu.vectors[2] = 1 | (2 << 32) | (3 << 64) | (4 << 96);
    cpu.vector_upper[2] = 5 | (6 << 32) | (7 << 64) | (8 << 96);
    cpu.vectors[3] = 11 | (12 << 32) | (13 << 64) | (14 << 96);
    cpu.vector_upper[3] = 15 | (16 << 32) | (17 << 64) | (18 << 96);
    let float_low = X86ScalarDecoder::decode(&[0xc5, 0xec, 0x14, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, float_low),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 1 | (11 << 32) | (2 << 64) | (12 << 96));
    assert_eq!(cpu.vector_upper[1], 5 | (15 << 32) | (6 << 64) | (16 << 96));

    cpu.rip = 0x7840;
    let double_high = X86ScalarDecoder::decode(&[0xc5, 0xe9, 0x15, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, double_high),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 3 | (4 << 32) | (13 << 64) | (14 << 96));
    assert_eq!(cpu.vector_upper[1], 0);

    cpu.rip = 0x7850;
    cpu.registers[3] = 0x1000;
    let before = cpu.clone();
    let float_memory = X86ScalarDecoder::decode(&[0xc5, 0xec, 0x14, 0x0b], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut faulting, float_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32
    ));
    assert_eq!(cpu, before);
}

#[test]
fn vex_packed_multiply_family() {
    for (bytes, operation, wide) in [
        ([0xc5, 0xe9, 0xd5, 0xcb, 0], VexOperation::MultiplyLowWord, false),
        (
            [0xc5, 0xed, 0xe4, 0xcb, 0],
            VexOperation::MultiplyHighWordUnsigned,
            true,
        ),
        ([0xc5, 0xed, 0xe5, 0xcb, 0], VexOperation::MultiplyHighWordSigned, true),
        ([0xc5, 0xed, 0xf4, 0xcb, 0], VexOperation::MultiplyDwordUnsigned, true),
        ([0xc4, 0xe2, 0x69, 0x28, 0xcb], VexOperation::MultiplyDwordSigned, false),
        ([0xc4, 0xe2, 0x6d, 0x40, 0xcb], VexOperation::MultiplyLowDword, true),
    ] {
        let decoded = X86ScalarDecoder::decode(&bytes[..4 + usize::from(bytes[0] == 0xc4)], 0x7900).unwrap();
        assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
            operation: actual, destination: 1, first: 2, second: VectorSource::Register(3),
            wide: actual_wide, ..
        } if actual == operation && actual_wide == wide));
    }
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0xed, 0x28, 0xcb], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0xed, 0x40, 0xcb], 0).is_err());

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7900,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = u128::from(0xffff_8000_ffff_0002_u64) | (u128::from(0x8000_7fff_0003_ffff_u64) << 64);
    cpu.vector_upper[2] = cpu.vectors[2];
    cpu.vectors[3] = u128::from(0x0002_0002_0002_0003_u64) | (u128::from(0x0002_0002_0004_0002_u64) << 64);
    cpu.vector_upper[3] = cpu.vectors[3];
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let signed_high = X86ScalarDecoder::decode(&[0xc5, 0xed, 0xe5, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, signed_high),
        ExecutionExit::Continue
    );
    for lane in 0..8 {
        let shift = lane * 16;
        let a = (cpu.vectors[2] >> shift) as u16 as i16 as i32;
        let b = (cpu.vectors[3] >> shift) as u16 as i16 as i32;
        assert_eq!((cpu.vectors[1] >> shift) as u16, ((a * b) >> 16) as u16);
    }
    assert_eq!(cpu.vector_upper[1], cpu.vectors[1]);

    cpu.rip = 0x7910;
    cpu.vectors[2] = u128::from(0xffff_ffff_u64) | (u128::from(0x8000_0000_u64) << 64);
    cpu.vectors[3] = u128::from(2_u64) | (u128::from(3_u64) << 64);
    let unsigned = X86ScalarDecoder::decode(&[0xc5, 0xe9, 0xf4, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, unsigned),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[1],
        u128::from(0x1_ffff_fffe_u64) | (u128::from(0x1_8000_0000_u64) << 64)
    );
    assert_eq!(cpu.vector_upper[1], 0);

    cpu.rip = 0x7920;
    cpu.registers[3] = 0x1000;
    let original = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc5, 0xed, 0xf4, 0x0b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32
    ));
    assert_eq!(cpu, original);
}

struct GatherMemory {
    fail_at: std::cell::Cell<Option<usize>>,
    reads: std::cell::RefCell<Vec<(u64, u8)>>,
}

impl GuestOperandMemory for GatherMemory {
    type Reservation = ();
    type BatchReservation = ();
    fn read(&self, address: u64, bytes: u8) -> Result<u64, ()> {
        let attempt = self.reads.borrow().len();
        self.reads.borrow_mut().push((address, bytes));
        if self.fail_at.get() == Some(attempt) {
            Err(())
        } else {
            Ok(address ^ 0x55)
        }
    }
    fn reserve_write(&self, _: u64, _: u8) -> Result<(), ()> {
        Err(())
    }
    fn commit_write(&mut self, (): (), _: u64) -> Result<(), ()> {
        Err(())
    }
    fn reserve_write_batch(&self, _: &[(u64, u8)]) -> Result<(), u64> {
        Err(0)
    }
    fn commit_write_batch(&mut self, (): (), _: &[u64]) -> Result<(), ()> {
        Err(())
    }
}

fn gather_dword(vector: [u128; 2], lane: usize) -> u32 {
    (vector[lane / 4] >> ((lane % 4) * 32)) as u32
}

fn gather_put(vector: &mut [u128; 2], lane: usize, value: u32) {
    let shift = (lane % 4) * 32;
    vector[lane / 4] = vector[lane / 4] & !(u128::from(u32::MAX) << shift) | (u128::from(value) << shift);
}

#[test]
fn vex_gather_decode_family() {
    for opcode in 0x90..=0x93 {
        for (prefix, element, wide) in [(0x41, 4, false), (0x45, 4, true), (0xc1, 8, false), (0xc5, 8, true)] {
            let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, prefix, opcode, 0x0c, 0x87], 0x7a00).unwrap();
            let index_bytes = if matches!(opcode, 0x90 | 0x92) { 4 } else { 8 };
            assert!(matches!(decoded.instruction, ScalarInstruction::VexGather {
                destination: 1, mask: 7, index: 0, element: actual_element,
                index_bytes: actual_index, wide: actual_wide, ..
            } if actual_element == element && actual_index == index_bytes && actual_wide == wide));
        }
    }
    for bytes in [
        [0xc4, 0xe2, 0x75, 0x90, 0x0c, 0x87],
        [0xc4, 0xe2, 0x45, 0x90, 0x0c, 0x8f],
        [0xc4, 0xe2, 0x45, 0x90, 0x0c, 0xbf],
        [0xc4, 0xe2, 0x45, 0x90, 0xc1, 0x00],
        [0xc4, 0xe2, 0x44, 0x90, 0x0c, 0x87],
    ] {
        assert!(X86ScalarDecoder::decode(&bytes, 0).is_err());
    }
}

#[test]
fn vex_gather_partial_fault_and_retry() {
    let address = EffectiveAddress {
        base: Some(3),
        index: Some(0),
        scale: 2,
        ..Default::default()
    };
    for fault_lane in 0..8 {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x7b00,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[3] = 0x1000;
        let (mut indices, mut destination, mut mask) = ([0_u128; 2], [0_u128; 2], [0_u128; 2]);
        for lane in 0..8 {
            gather_put(&mut indices, lane, lane as u32);
            gather_put(&mut destination, lane, 0xdead_0000 | lane as u32);
            gather_put(&mut mask, lane, 0x8000_0000);
        }
        cpu.vectors[0] = indices[0];
        cpu.vector_upper[0] = indices[1];
        cpu.vectors[1] = destination[0];
        cpu.vector_upper[1] = destination[1];
        cpu.vectors[7] = mask[0];
        cpu.vector_upper[7] = mask[1];
        let memory = GatherMemory {
            fail_at: std::cell::Cell::new(Some(fault_lane)),
            reads: std::cell::RefCell::new(Vec::new()),
        };
        let exit = ScalarInterpreter::vex_gather(&mut cpu, &memory, 1, 7, 0, address, 4, 4, true, 0x7b00, 0x7b06);
        assert!(matches!(exit, ExecutionExit::OperandFault(access)
            if access.address() == 0x1000 + fault_lane as u64 * 4 && access.length() == 4));
        assert_eq!(cpu.rip, 0x7b00);
        for lane in 0..8 {
            let expected = if lane < fault_lane {
                (0x1000 + lane as u64 * 4) as u32 ^ 0x55
            } else {
                0xdead_0000 | lane as u32
            };
            assert_eq!(gather_dword([cpu.vectors[1], cpu.vector_upper[1]], lane), expected);
            assert_eq!(
                gather_dword([cpu.vectors[7], cpu.vector_upper[7]], lane),
                if lane < fault_lane { 0 } else { 0x8000_0000 }
            );
        }
        memory.fail_at.set(None);
        memory.reads.borrow_mut().clear();
        assert_eq!(
            ScalarInterpreter::vex_gather(&mut cpu, &memory, 1, 7, 0, address, 4, 4, true, 0x7b00, 0x7b06),
            ExecutionExit::Continue
        );
        assert_eq!((cpu.rip, cpu.vectors[7], cpu.vector_upper[7]), (0x7b06, 0, 0));
        assert_eq!(memory.reads.borrow().len(), 8 - fault_lane);
    }
}

#[test]
fn vex_gather_fault_upper_and_address_projection() {
    let address = EffectiveAddress {
        base: Some(3),
        index: Some(0),
        scale: 1,
        ..Default::default()
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7c00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x2000;
    cpu.vectors[0] = u128::from(1_u64) | (u128::from(2_u64) << 64);
    cpu.vectors[1] = u128::MAX;
    cpu.vector_upper[1] = 0xfeed;
    cpu.vectors[7] = u128::from(0x8000_0000_u32) | (u128::from(0x8000_0000_u32) << 32);
    cpu.vector_upper[7] = 0xbeef;
    let memory = GatherMemory {
        fail_at: std::cell::Cell::new(Some(1)),
        reads: std::cell::RefCell::new(Vec::new()),
    };
    assert!(matches!(
        ScalarInterpreter::vex_gather(&mut cpu, &memory, 1, 7, 0, address, 4, 8, false, 0x7c00, 0x7c06),
        ExecutionExit::OperandFault(_)
    ));
    assert_eq!(
        (cpu.vector_upper[1], cpu.vector_upper[7], (cpu.vectors[1] >> 64) as u64),
        (0xfeed, 0xbeef, u64::MAX)
    );
    memory.fail_at.set(None);
    assert_eq!(
        ScalarInterpreter::vex_gather(&mut cpu, &memory, 1, 7, 0, address, 4, 8, false, 0x7c00, 0x7c06),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.vectors[1] >> 64, cpu.vector_upper[1]), (0, 0));

    let projected = EffectiveAddress {
        base: Some(3),
        index: Some(0),
        scale: 1,
        address_32: true,
        segment: Some(Segment::Fs),
        ..Default::default()
    };
    cpu.rip = 0x7d00;
    cpu.registers[3] = 0xffff_ffff_ffff_fff0;
    cpu.fs_base = 0x100;
    cpu.vectors[0] = 8;
    cpu.vectors[7] = 0x8000_0000;
    memory.reads.borrow_mut().clear();
    assert_eq!(
        ScalarInterpreter::vex_gather(&mut cpu, &memory, 1, 7, 0, projected, 4, 4, false, 0x7d00, 0x7d06),
        ExecutionExit::Continue
    );
    assert_eq!(memory.reads.borrow().as_slice(), &[(0x100, 4)]);
}

#[test]
fn vex_immediate_blend_family() {
    for (opcode, operation) in [
        (0x02, VexOperation::BlendDword),
        (0x0c, VexOperation::BlendDword),
        (0x0d, VexOperation::BlendQword),
        (0x0e, VexOperation::BlendWord),
    ] {
        for (prefix, wide) in [(0x69, false), (0x6d, true)] {
            let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe3, prefix, opcode, 0xcb, 0xa5], 0x7e00).unwrap();
            assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
                operation: actual, destination: 1, first: 2, second: VectorSource::Register(3),
                wide: actual_wide, immediate: 0xa5,
            } if actual == operation && actual_wide == wide));
        }
    }
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe3, 0xed, 0x0e, 0xcb, 1], 0).is_err());

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7e00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = 0x1111_1111_1111_1111_1111_1111_1111_1111;
    cpu.vector_upper[2] = cpu.vectors[2];
    cpu.vectors[3] = 0xaaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa_aaaa;
    cpu.vector_upper[3] = cpu.vectors[3];
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let words = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x6d, 0x0e, 0xcb, 0x55], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, words),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 0x1111_aaaa_1111_aaaa_1111_aaaa_1111_aaaa);
    assert_eq!(cpu.vector_upper[1], cpu.vectors[1]);

    cpu.rip = 0x7e10;
    cpu.registers[3] = 0x1000;
    let original = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x6d, 0x02, 0x0b, 0xff], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32)
    );
    assert_eq!(cpu, original);
}

#[test]
fn vex_packed_saturating_family() {
    for (map, opcode, operation) in [
        (0xe1, 0x63, VexOperation::PackSignedWordByte),
        (0xe1, 0x67, VexOperation::PackUnsignedWordByte),
        (0xe1, 0x6b, VexOperation::PackSignedDwordWord),
        (0xe2, 0x2b, VexOperation::PackUnsignedDwordWord),
    ] {
        for (prefix, wide) in [(0x69, false), (0x6d, true)] {
            let decoded = X86ScalarDecoder::decode(&[0xc4, map, prefix, opcode, 0xcb], 0x7f00).unwrap();
            assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
                operation: actual, destination: 1, first: 2, second: VectorSource::Register(3),
                wide: actual_wide, ..
            } if actual == operation && actual_wide == wide));
        }
    }
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x68, 0x63, 0xcb], 0).is_err());

    let words = |values: [i16; 8]| {
        values.into_iter().enumerate().fold(0_u128, |packed, (lane, value)| {
            packed | (u128::from(value as u16) << (lane * 16))
        })
    };
    let bytes = |values: [i16; 16], unsigned: bool| {
        values.into_iter().enumerate().fold(0_u128, |packed, (lane, value)| {
            let value = if unsigned {
                value.clamp(0, 255) as u8
            } else {
                value.clamp(-128, 127) as i8 as u8
            };
            packed | (u128::from(value) << (lane * 8))
        })
    };
    let left = [-32768, -129, -128, -1, 0, 1, 127, 128];
    let right = [255, 256, 32767, -255, -2, 2, 126, 129];
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7f00,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = words(left);
    cpu.vector_upper[2] = words(right);
    cpu.vectors[3] = words(right);
    cpu.vector_upper[3] = words(left);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let signed = X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x6d, 0x63, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, signed),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], bytes([left, right].concat().try_into().unwrap(), false));
    assert_eq!(
        cpu.vector_upper[1],
        bytes([right, left].concat().try_into().unwrap(), false)
    );

    cpu.rip = 0x7f10;
    cpu.vector_upper[1] = u128::MAX;
    let unsigned = X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x69, 0x67, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, unsigned),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], bytes([left, right].concat().try_into().unwrap(), true));
    assert_eq!(cpu.vector_upper[1], 0);

    cpu.rip = 0x7f20;
    cpu.registers[3] = 0x1000;
    let original = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, 0x2b, 0x0b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32)
    );
    assert_eq!(cpu, original);
}

#[test]
fn vex_movemask_family() {
    for (prefix, opcode, lane, wide) in [
        (0xf8, 0x50, 4, false),
        (0xfc, 0x50, 4, true),
        (0xf9, 0x50, 8, false),
        (0xfd, 0x50, 8, true),
        (0xf9, 0xd7, 1, false),
        (0xfd, 0xd7, 1, true),
    ] {
        let decoded = X86ScalarDecoder::decode(&[0xc5, prefix, opcode, 0xcb], 0x7f80).unwrap();
        assert!(matches!(decoded.instruction, ScalarInstruction::VexMask {
            destination: ScalarRegister::General(1), source: 3, lane: actual_lane, wide: actual_wide,
        } if actual_lane == lane && actual_wide == wide));
    }
    let extended = X86ScalarDecoder::decode(&[0xc5, 0x7c, 0x50, 0xcb], 0).unwrap();
    assert!(matches!(
        extended.instruction,
        ScalarInstruction::VexMask {
            destination: ScalarRegister::General(9),
            ..
        }
    ));
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe1, 0xfc, 0x50, 0xcb], 0).is_ok());
    for invalid in [
        &[0xc5, 0xec, 0x50, 0xcb][..],
        &[0xc5, 0xfe, 0x50, 0xcb],
        &[0xc5, 0xfd, 0xd7, 0x0b],
        &[0xc5, 0xfc, 0xd7, 0xcb],
    ] {
        assert!(X86ScalarDecoder::decode(invalid, 0).is_err());
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7f80,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.flags = cpu.flags.with(Flag::Carry, true).with(Flag::Zero, true);
    cpu.registers[9] = u64::MAX;
    cpu.vectors[3] = (1_u128 << 7) | (1_u128 << 31) | (1_u128 << 127);
    cpu.vector_upper[3] = (1_u128 << 7) | (1_u128 << 63) | (1_u128 << 127);
    let flags = cpu.flags;
    let vectors = (cpu.vectors[3], cpu.vector_upper[3]);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let bytes = X86ScalarDecoder::decode(&[0xc5, 0x7d, 0xd7, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, bytes),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.registers[9],
        (1 << 0) | (1 << 3) | (1 << 15) | (1 << 16) | (1 << 23) | (1 << 31)
    );
    assert_eq!(cpu.flags, flags);
    assert_eq!((cpu.vectors[3], cpu.vector_upper[3]), vectors);

    cpu.rip = 0x7f90;
    cpu.registers[1] = u64::MAX;
    cpu.vectors[3] = (1_u128 << 31) | (1_u128 << 95);
    cpu.vector_upper[3] = (1_u128 << 63) | (1_u128 << 127);
    let singles = X86ScalarDecoder::decode(&[0xc5, 0xfc, 0x50, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, singles),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[1], 0xa5);

    cpu.rip = 0x7fa0;
    cpu.registers[1] = u64::MAX;
    cpu.vectors[3] = 1_u128 << 127;
    cpu.vector_upper[3] = 1_u128 << 63;
    let doubles = X86ScalarDecoder::decode(&[0xc5, 0xfd, 0x50, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, doubles),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[1], 6);
}

#[test]
fn vex_round_family() {
    for (opcode, format, scalar) in [
        (0x08, FloatWidth::Single, false),
        (0x09, FloatWidth::Double, false),
        (0x0a, FloatWidth::Single, true),
        (0x0b, FloatWidth::Double, true),
    ] {
        for (prefix, wide) in if scalar {
            [(0x69, false), (0x69, false)]
        } else {
            [(0x79, false), (0x7d, true)]
        } {
            let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe3, prefix, opcode, 0xcb, 1], 0x7fb0).unwrap();
            assert!(matches!(decoded.instruction, ScalarInstruction::VexRound {
                destination: 1, source: VectorSource::Register(3), format: actual_format,
                scalar: actual_scalar, wide: actual_wide, control: 1, ..
            } if (actual_format, actual_scalar, actual_wide) == (format, scalar, wide)));
        }
    }
    for invalid in [
        &[0xc4, 0xe3, 0x78, 0x08, 0xcb, 0][..],
        &[0xc4, 0xe3, 0x6d, 0x08, 0xcb, 0],
        &[0xc4, 0xe3, 0x6d, 0x0a, 0xcb, 0],
    ] {
        assert!(X86ScalarDecoder::decode(invalid, 0).is_err());
    }

    let singles = |values: [f32; 4]| {
        values.into_iter().enumerate().fold(0_u128, |bits, (lane, value)| {
            bits | (u128::from(value.to_bits()) << (lane * 32))
        })
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7fb0,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[3] = singles([1.5, -1.5, 2.1, -2.1]);
    cpu.vector_upper[3] = singles([3.5, -3.5, 4.1, -4.1]);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let packed = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x7d, 0x08, 0xcb, 0], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, packed),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], singles([2.0, -2.0, 2.0, -2.0]));
    assert_eq!(cpu.vector_upper[1], singles([4.0, -4.0, 4.0, -4.0]));

    cpu.rip = 0x7fc0;
    cpu.vectors[2] = 0xfeed_face_dead_beef_0123_4567_89ab_cdef;
    cpu.vectors[3] = u128::from(1.1_f32.to_bits());
    cpu.vector_upper[1] = u128::MAX;
    cpu.mxcsr = 0x1f80 | (2 << 13);
    let scalar = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x69, 0x0a, 0xcb, 4], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, scalar),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1] >> 32, cpu.vectors[2] >> 32);
    assert_eq!(cpu.vectors[1] as u32, 2.0_f32.to_bits());
    assert_eq!(cpu.vector_upper[1], 0);

    cpu.rip = 0x7fd0;
    cpu.registers[3] = 0x1000;
    let original = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x7d, 0x09, 0x0b, 0], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32)
    );
    assert_eq!(cpu, original);
}

#[test]
fn vex_dot_product_family() {
    let singles = |values: [f32; 4]| {
        values.into_iter().enumerate().fold(0_u128, |bits, (lane, value)| {
            bits | (u128::from(value.to_bits()) << (lane * 32))
        })
    };
    let doubles = |values: [f64; 2]| u128::from(values[0].to_bits()) | (u128::from(values[1].to_bits()) << 64);
    let flags = FlagState::from_bits(0x8d5);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7fd8,
            flags,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[1] = singles([1.0, 2.0, 3.0, 4.0]);
    cpu.vector_upper[1] = singles([2.0, 3.0, 4.0, 5.0]);
    cpu.vectors[2] = singles([10.0, 20.0, 30.0, 40.0]);
    cpu.vector_upper[2] = singles([1.0, 2.0, 3.0, 4.0]);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let packed = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x75, 0x40, 0xda, 0xf5], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, packed),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[3], singles([300.0, 0.0, 300.0, 0.0]));
    assert_eq!(cpu.vector_upper[3], singles([40.0, 0.0, 40.0, 0.0]));
    assert_eq!(cpu.flags, flags);

    cpu.rip = 0x7fe0;
    cpu.vectors[0] = doubles([1.0, 2.0]);
    cpu.vectors[1] = doubles([10.0, 20.0]);
    cpu.vector_upper[2] = u128::MAX;
    let packed = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x79, 0x41, 0xd1, 0x31], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, packed),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[2], doubles([50.0, 0.0]));
    assert_eq!(cpu.vector_upper[2], 0);
    assert_eq!(cpu.flags, flags);

    cpu.rip = 0x7fe8;
    cpu.registers[3] = 0x1000;
    let before = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x75, 0x40, 0x1b, 0xff], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32
    ));
    assert_eq!(cpu, before);

    cpu.rip = 0x7fec;
    cpu.mxcsr = 0x1f80 | (1 << 6);
    cpu.vectors[0] = 1;
    cpu.vectors[1] = doubles([1.0, 1.0]);
    let legacy_daz = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x3a, 0x41, 0xc1, 0x31], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, legacy_daz),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u64, 0);
    assert_eq!(cpu.mxcsr & 0x3f, 0);

    cpu.rip = 0x7fee;
    cpu.vectors[1] = singles([f32::from_bits(1), 0.0, 0.0, 0.0]);
    cpu.vectors[2] = singles([1.0, 0.0, 0.0, 0.0]);
    let vex_daz = X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x71, 0x40, 0xc2, 0x11], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, vex_daz),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[0] as u32, 0);
    assert_eq!(cpu.mxcsr & 0x3f, 0);

    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x7d, 0x41, 0xd1, 0x31], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe3, 0x78, 0x40, 0xd1, 0x31], 0).is_err());
}

#[test]
fn vex_square_root_family() {
    let singles = |values: [f32; 4]| {
        values.into_iter().enumerate().fold(0_u128, |bits, (lane, value)| {
            bits | (u128::from(value.to_bits()) << (lane * 32))
        })
    };
    let doubles = |values: [f64; 2]| u128::from(values[0].to_bits()) | (u128::from(values[1].to_bits()) << 64);
    let flags = FlagState::from_bits(0x8d5);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7ff0,
            flags,
            ..Default::default()
        },
        mxcsr: 0x1f80,
        ..Default::default()
    };
    cpu.vectors[2] = singles([1.0, 4.0, 9.0, 16.0]);
    cpu.vector_upper[2] = singles([25.0, 36.0, 49.0, 64.0]);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let packed = X86ScalarDecoder::decode(&[0xc5, 0xfc, 0x51, 0xca], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, packed),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], singles([1.0, 2.0, 3.0, 4.0]));
    assert_eq!(cpu.vector_upper[1], singles([5.0, 6.0, 7.0, 8.0]));
    assert_eq!(cpu.flags, flags);

    cpu.rip = 0x8000;
    cpu.vectors[0] = doubles([81.0, 123.0]);
    cpu.vectors[1] = doubles([25.0, 36.0]);
    cpu.vector_upper[2] = u128::MAX;
    let scalar = X86ScalarDecoder::decode(&[0xc5, 0xfb, 0x51, 0xd1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, scalar),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[2], doubles([5.0, 123.0]));
    assert_eq!(cpu.vector_upper[2], 0);

    cpu.rip = 0x8010;
    cpu.mxcsr = 0x1f80;
    cpu.vectors[0] = singles([77.0, 88.0, 99.0, 111.0]);
    cpu.vectors[1] = u128::from((-1.0_f32).to_bits());
    let invalid = X86ScalarDecoder::decode(&[0xc5, 0xfa, 0x51, 0xd1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, invalid),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[2] as u32, 0xffc0_0000);
    assert_eq!(cpu.vectors[2] >> 32, cpu.vectors[0] >> 32);
    assert_ne!(cpu.mxcsr & 1, 0);

    cpu.rip = 0x8020;
    cpu.mxcsr = 0x1f80 | (1 << 6);
    cpu.vectors[1] = 1;
    let daz = X86ScalarDecoder::decode(&[0xc5, 0xfa, 0x51, 0xd1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, daz),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[2] as u32, 0);
    assert_eq!(cpu.mxcsr & (1 << 1), 0);

    for (rounding, expected) in [(0, 0x3fb5_04f3), (2, 0x3fb5_04f4)] {
        cpu.rip = 0x8028;
        cpu.mxcsr = 0x1f80 | (rounding << 13) | (1 << 15);
        cpu.vectors[1] = u128::from(2.0_f32.to_bits());
        let rounded = X86ScalarDecoder::decode(&[0xc5, 0xfa, 0x51, 0xd1], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, rounded),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[2] as u32, expected);
        assert_ne!(cpu.mxcsr & (1 << 5), 0);
    }

    cpu.rip = 0x8030;
    cpu.registers[3] = 0x1000;
    let before = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc5, 0xfc, 0x51, 0x0b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32
    ));
    assert_eq!(cpu, before);

    assert!(X86ScalarDecoder::decode(&[0xc5, 0xec, 0x51, 0xca], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xfe, 0x51, 0xca], 0).is_err());
}

#[test]
fn vex_reciprocal_family() {
    let singles = |values: [f32; 4]| {
        values.into_iter().enumerate().fold(0_u128, |bits, (lane, value)| {
            bits | (u128::from(value.to_bits()) << (lane * 32))
        })
    };
    let flags = FlagState::from_bits(0x8d5);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x8040,
            flags,
            ..Default::default()
        },
        mxcsr: 0x1f80,
        ..Default::default()
    };
    cpu.vectors[2] = singles([1.0, 2.0, 4.0, 8.0]);
    cpu.vector_upper[2] = singles([16.0, 32.0, 64.0, 128.0]);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let packed = X86ScalarDecoder::decode(&[0xc5, 0xfc, 0x53, 0xca], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, packed),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], singles([1.0, 0.5, 0.25, 0.125]));
    assert_eq!(cpu.vector_upper[1], singles([0.0625, 0.03125, 0.015625, 0.0078125]));
    assert_eq!(cpu.flags, flags);
    assert_eq!(cpu.mxcsr, 0x1f80);

    cpu.rip = 0x8050;
    cpu.vectors[0] = singles([77.0, 88.0, 99.0, 111.0]);
    cpu.vectors[1] = singles([4.0, 0.0, 0.0, 0.0]);
    cpu.vector_upper[2] = u128::MAX;
    let scalar = X86ScalarDecoder::decode(&[0xc5, 0xfa, 0x53, 0xd1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, scalar),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[2], singles([0.25, 88.0, 99.0, 111.0]));
    assert_eq!(cpu.vector_upper[2], 0);

    cpu.rip = 0x8060;
    cpu.vectors[1] = u128::from((-1.0_f32).to_bits());
    let negative = X86ScalarDecoder::decode(&[0xc5, 0xfa, 0x52, 0xd1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, negative),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[2] as u32, 0xffc0_0000);
    assert_eq!(cpu.mxcsr, 0x1f80);

    cpu.rip = 0x8070;
    cpu.registers[3] = 0x1000;
    let before = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc5, 0xfc, 0x52, 0x0b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32
    ));
    assert_eq!(cpu, before);

    assert!(X86ScalarDecoder::decode(&[0xc5, 0xfd, 0x53, 0xca], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xfe, 0x53, 0xca], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xee, 0x53, 0xca], 0).is_err());
}

#[test]
fn vex_qword_transport_family() {
    let load = X86ScalarDecoder::decode(&[0xc5, 0xfa, 0x7e, 0xcb], 0x7fe0).unwrap();
    assert!(matches!(
        load.instruction,
        ScalarInstruction::VexQword {
            vector: 1,
            operand: VectorSource::Register(3),
            store: false,
        }
    ));
    let store = X86ScalarDecoder::decode(&[0xc5, 0xf9, 0xd6, 0xcb], 0x7fe0).unwrap();
    assert!(matches!(
        store.instruction,
        ScalarInstruction::VexQword {
            vector: 1,
            operand: VectorSource::Register(3),
            store: true,
        }
    ));
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xfe, 0x7e, 0xcb], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xea, 0x7e, 0xcb], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xfd, 0xd6, 0xcb], 0).is_err());

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7fe0,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[1] = u128::MAX;
    cpu.vector_upper[1] = u128::MAX;
    cpu.vectors[3] = 0xaaaa_bbbb_cccc_dddd_0123_4567_89ab_cdef;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.vectors[1], cpu.vector_upper[1]), (0x0123_4567_89ab_cdef, 0));

    cpu.rip = 0x7ff0;
    cpu.vectors[1] = 0xfeed_face_dead_beef;
    cpu.vectors[3] = u128::MAX;
    cpu.vector_upper[3] = u128::MAX;
    let store = X86ScalarDecoder::decode(&[0xc5, 0xf9, 0xd6, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.vectors[3], cpu.vector_upper[3]), (0xfeed_face_dead_beef, 0));

    cpu.rip = 0x8000;
    cpu.registers[3] = 0x1000;
    let original = cpu.clone();
    let memory_load = X86ScalarDecoder::decode(&[0xc5, 0xfa, 0x7e, 0x0b], cpu.rip).unwrap();
    let mut faulting_read = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 8],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut faulting_read, memory_load),
        ExecutionExit::OperandFault(access) if access.length() == 8)
    );
    assert_eq!(cpu, original);

    let memory_store = X86ScalarDecoder::decode(&[0xc5, 0xf9, 0xd6, 0x0b], cpu.rip).unwrap();
    let mut faulting_write = ModelMemory {
        base: 0x1000,
        bytes: vec![0xaa; 8],
        fail_read: false,
        fail_write: true,
        commits: 0,
    };
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut faulting_write, memory_store),
        ExecutionExit::OperandFault(access) if access.length() == 8)
    );
    assert_eq!(cpu, original);
    assert_eq!(faulting_write.bytes, vec![0xaa; 8]);

    let low = X86ScalarDecoder::decode(&[0xc5, 0xe8, 0x12, 0xcb], 0x8010).unwrap();
    let high = X86ScalarDecoder::decode(&[0xc5, 0xe8, 0x16, 0xcb], 0x8010).unwrap();
    assert!(matches!(
        low.instruction,
        ScalarInstruction::VexHalfMove {
            destination: 1,
            first: 2,
            second: VectorSource::Register(3),
            high: false,
        }
    ));
    assert!(matches!(
        high.instruction,
        ScalarInstruction::VexHalfMove { high: true, .. }
    ));
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xf8, 0x13, 0xcb], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xfc, 0x13, 0x0b], 0).is_err());

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x8010,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = (u128::from(0xaaaa_u64) << 64) | 0xbbbb;
    cpu.vectors[3] = (u128::from(0xcccc_u64) << 64) | 0xdddd;
    cpu.vector_upper[1] = u128::MAX;
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, low),
        ExecutionExit::Continue
    );
    assert_eq!(
        (cpu.vectors[1], cpu.vector_upper[1]),
        ((u128::from(0xaaaa_u64) << 64) | 0xcccc, 0)
    );
    cpu.rip = 0x8020;
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, high),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], (u128::from(0xdddd_u64) << 64) | 0xbbbb);

    cpu.rip = 0x8030;
    cpu.registers[3] = 0x1000;
    cpu.vectors[1] = (u128::from(0x1234_u64) << 64) | 0x5678;
    let low_store = X86ScalarDecoder::decode(&[0xc5, 0xf8, 0x13, 0x0b], cpu.rip).unwrap();
    let mut output = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 8],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut output, low_store),
        ExecutionExit::Continue
    );
    assert_eq!(output.bytes, 0x5678_u64.to_le_bytes());
    cpu.rip = 0x8040;
    let before = cpu.clone();
    let high_store = X86ScalarDecoder::decode(&[0xc5, 0xf9, 0x17, 0x0b], cpu.rip).unwrap();
    let mut failed = ModelMemory {
        base: 0x1000,
        bytes: vec![0xaa; 8],
        fail_read: false,
        fail_write: true,
        commits: 0,
    };
    assert!(matches!(ScalarInterpreter::execute(&mut cpu, &mut failed, high_store),
        ExecutionExit::OperandFault(access) if access.length() == 8));
    assert_eq!(cpu, before);
    assert_eq!(failed.bytes, vec![0xaa; 8]);
}

#[test]
fn vex_packed_add_subtract_family() {
    let cases = [
        (0xfc, VexOperation::AddByte, 1_u32, false),
        (0xfd, VexOperation::AddWord, 2, false),
        (0xfe, VexOperation::AddDword, 4, false),
        (0xd4, VexOperation::AddQword, 8, false),
        (0xf8, VexOperation::SubtractByte, 1, true),
        (0xf9, VexOperation::SubtractWord, 2, true),
        (0xfa, VexOperation::SubtractDword, 4, true),
        (0xfb, VexOperation::SubtractQword, 8, true),
    ];
    let reference = |left: u128, right: u128, bytes: u32, subtract: bool| {
        let bits = bytes * 8;
        let mask = (1_u128 << bits) - 1;
        (0..16 / bytes).fold(0_u128, |result, lane| {
            let shift = lane * bits;
            let a = left >> shift & mask;
            let b = right >> shift & mask;
            let value = if subtract { a.wrapping_sub(b) } else { a.wrapping_add(b) } & mask;
            result | value << shift
        })
    };
    for (opcode, operation, bytes, subtract) in cases {
        for (prefix, wide) in [(0xe9, false), (0xed, true)] {
            let decoded = X86ScalarDecoder::decode(&[0xc5, prefix, opcode, 0xcb], 0x7fc0).unwrap();
            assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
                operation: actual, destination: 1, first: 2, second: VectorSource::Register(3),
                wide: actual_wide, ..
            } if actual == operation && actual_wide == wide));
        }
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x7fc0,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.flags = cpu.flags.with(Flag::Carry, true).with(Flag::Overflow, true);
        cpu.vectors[2] = 0xffff_ffff_ffff_ffff_8080_0100_00ff_7fff;
        cpu.vector_upper[2] = 0x7fff_ffff_0000_0000_ffff_ffff_ffff_ffff;
        cpu.vectors[3] = 0x0001_0000_0000_0001_0101_0101_0101_0101;
        cpu.vector_upper[3] = 0xffff_ffff_0000_0001_0000_0000_0000_0001;
        let flags = cpu.flags;
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let decoded = X86ScalarDecoder::decode(&[0xc5, 0xed, opcode, 0xcb], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::Continue
        );
        assert_eq!(
            cpu.vectors[1],
            reference(cpu.vectors[2], cpu.vectors[3], bytes, subtract)
        );
        assert_eq!(
            cpu.vector_upper[1],
            reference(cpu.vector_upper[2], cpu.vector_upper[3], bytes, subtract)
        );
        assert_eq!(cpu.flags, flags);
    }

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7fe0,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[1] = 0x1234;
    cpu.vector_upper[1] = 0x5678;
    let original = cpu.clone();
    let memory_subtract = X86ScalarDecoder::decode(&[0xc5, 0xed, 0xfa, 0x0b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut faulting, memory_subtract),
        ExecutionExit::OperandFault(access) if access.length() == 32)
    );
    assert_eq!(cpu, original);
}

#[test]
fn vex_saturating_arithmetic_family() {
    let cases = [
        (0xec, false, false, false),
        (0xed, false, false, true),
        (0xe8, true, false, false),
        (0xe9, true, false, true),
        (0xdc, false, true, false),
        (0xdd, false, true, true),
        (0xd8, true, true, false),
        (0xd9, true, true, true),
    ];
    for (opcode, subtract, unsigned, word) in cases {
        for (prefix, wide) in [(0xe9, false), (0xed, true)] {
            let decoded = X86ScalarDecoder::decode(&[0xc5, prefix, opcode, 0xcb], 0x7ff0).unwrap();
            assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
                operation: VexOperation::Saturating {
                    subtract: actual_subtract, unsigned: actual_unsigned, word: actual_word,
                }, destination: 1, first: 2, second: VectorSource::Register(3), wide: actual_wide, ..
            } if (actual_subtract, actual_unsigned, actual_word, actual_wide) == (subtract, unsigned, word, wide)));
        }
        let bits = if word { 16 } else { 8 };
        let mask = (1_u128 << bits) - 1;
        let left = 0x7fff_8000_ffff_0000_00ff_0001_fffe_0002_u128;
        let right = 0x0001_0001_ffff_0001_0001_0002_0002_ffff_u128;
        let reference = |a: u128, b: u128| {
            (0..128 / bits).fold(0_u128, |result, lane| {
                let shift = lane * bits;
                let a = a >> shift & mask;
                let b = b >> shift & mask;
                let value = if unsigned {
                    let value = if subtract { a as i64 - b as i64 } else { (a + b) as i64 };
                    value.clamp(0, mask as i64) as u128
                } else {
                    let sign = 1_u128 << (bits - 1);
                    let signed = |value: u128| ((value ^ sign) as i64) - sign as i64;
                    let value = if subtract {
                        signed(a) - signed(b)
                    } else {
                        signed(a) + signed(b)
                    };
                    (value.clamp(-(1_i64 << (bits - 1)), (1_i64 << (bits - 1)) - 1) as u128) & mask
                };
                result | value << shift
            })
        };
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x7ff0,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[2] = left;
        cpu.vector_upper[2] = !left;
        cpu.vectors[3] = right;
        cpu.vector_upper[3] = !right;
        cpu.flags = cpu.flags.with(Flag::Carry, true).with(Flag::Overflow, true);
        let flags = cpu.flags;
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let decoded = X86ScalarDecoder::decode(&[0xc5, 0xed, opcode, 0xcb], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[1], reference(left, right));
        assert_eq!(cpu.vector_upper[1], reference(!left, !right));
        assert_eq!(cpu.flags, flags);
    }
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xec, 0xec, 0xcb], 0).is_err());

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x8010,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[1] = 0x1234;
    cpu.vector_upper[1] = 0x5678;
    let original = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc5, 0xed, 0xec, 0x0b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32)
    );
    assert_eq!(cpu, original);
}

#[test]
fn vex_packed_extrema_family() {
    let cases = [
        (1, 0xda, false, true, 1_u8),
        (1, 0xde, true, true, 1),
        (1, 0xea, false, false, 2),
        (1, 0xee, true, false, 2),
        (2, 0x38, false, false, 1),
        (2, 0x39, false, false, 4),
        (2, 0x3a, false, true, 2),
        (2, 0x3b, false, true, 4),
        (2, 0x3c, true, false, 1),
        (2, 0x3d, true, false, 4),
        (2, 0x3e, true, true, 2),
        (2, 0x3f, true, true, 4),
    ];
    for (map, opcode, maximum, unsigned, bytes) in cases {
        let map_byte = if map == 1 { 0xe1 } else { 0xe2 };
        for (prefix, wide) in [(0x69, false), (0x6d, true)] {
            let decoded = X86ScalarDecoder::decode(&[0xc4, map_byte, prefix, opcode, 0xcb], 0x8020).unwrap();
            assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
                operation: VexOperation::Extrema {
                    maximum: actual_maximum, unsigned: actual_unsigned, bytes: actual_bytes,
                }, destination: 1, first: 2, second: VectorSource::Register(3), wide: actual_wide, ..
            } if (actual_maximum, actual_unsigned, actual_bytes, actual_wide) ==
                (maximum, unsigned, bytes, wide)));
        }
        let bits = u32::from(bytes) * 8;
        let mask = (1_u128 << bits) - 1;
        let sign = 1_u128 << (bits - 1);
        let left = 0x7fff_8000_ffff_0000_00ff_0001_fffe_0002_u128;
        let right = 0x8000_7fff_0001_ffff_0001_00ff_0002_fffe_u128;
        let reference = |a: u128, b: u128| {
            (0..128 / bits).fold(0_u128, |result, lane| {
                let shift = lane * bits;
                let a = a >> shift & mask;
                let b = b >> shift & mask;
                let order = if unsigned {
                    a.cmp(&b)
                } else {
                    (a ^ sign).cmp(&(b ^ sign))
                };
                let select_left = if maximum { order.is_gt() } else { order.is_lt() };
                result | (if select_left { a } else { b }) << shift
            })
        };
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x8020,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[2] = left;
        cpu.vector_upper[2] = !left;
        cpu.vectors[3] = right;
        cpu.vector_upper[3] = !right;
        cpu.flags = cpu.flags.with(Flag::Carry, true).with(Flag::Overflow, true);
        let flags = cpu.flags;
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let decoded = X86ScalarDecoder::decode(&[0xc4, map_byte, 0x6d, opcode, 0xcb], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[1], reference(left, right));
        assert_eq!(cpu.vector_upper[1], reference(!left, !right));
        assert_eq!(cpu.flags, flags);
    }
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x6c, 0xda, 0xcb], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0xed, 0x39, 0xcb], 0).is_ok());

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x8040,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[1] = 0x1234;
    cpu.vector_upper[1] = 0x5678;
    let original = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, 0x39, 0x0b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32)
    );
    assert_eq!(cpu, original);
}

#[test]
fn vex_packed_average_family() {
    for (opcode, word) in [(0xe0, false), (0xe3, true)] {
        for (prefix, wide) in [(0x69, false), (0x6d, true), (0xed, true)] {
            let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe1, prefix, opcode, 0xcb], 0x8050).unwrap();
            assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
                operation: VexOperation::Average { word: actual_word }, destination: 1, first: 2,
                second: VectorSource::Register(3), wide: actual_wide, ..
            } if actual_word == word && actual_wide == wide));
        }
        let bits = if word { 16 } else { 8 };
        let mask = (1_u128 << bits) - 1;
        let left = 0xffff_0000_0001_fffe_00ff_0001_ff00_007f_u128;
        let right = 0xffff_ffff_0002_0001_0000_00ff_0101_0080_u128;
        let reference = |a: u128, b: u128| {
            (0..128 / bits).fold(0_u128, |result, lane| {
                let shift = lane * bits;
                result | ((((a >> shift & mask) + (b >> shift & mask) + 1) >> 1) << shift)
            })
        };
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x8050,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[2] = left;
        cpu.vector_upper[2] = !left;
        cpu.vectors[3] = right;
        cpu.vector_upper[3] = !right;
        cpu.flags = cpu.flags.with(Flag::Carry, true).with(Flag::Overflow, true);
        let flags = cpu.flags;
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x6d, opcode, 0xcb], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[1], reference(left, right));
        assert_eq!(cpu.vector_upper[1], reference(!left, !right));
        assert_eq!(cpu.flags, flags);

        let mut legacy = CpuState {
            scalar: ScalarState {
                rip: 0x8060,
                ..Default::default()
            },
            ..Default::default()
        };
        legacy.vectors[1] = left;
        legacy.vectors[3] = right;
        let legacy_ir = X86ScalarDecoder::decode(&[0x66, 0x0f, opcode, 0xcb], legacy.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut legacy, &mut memory, legacy_ir),
            ExecutionExit::Continue
        );
        assert_eq!(legacy.vectors[1], reference(left, right));
    }
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x68, 0xe0, 0xcb], 0).is_err());

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x8070,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[3] = 0x1000;
    cpu.vectors[1] = 0x1234;
    cpu.vector_upper[1] = 0x5678;
    let original = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x6d, 0xe3, 0x0b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32)
    );
    assert_eq!(cpu, original);
}

#[test]
fn vex_packed_multiply_add_family() {
    for (map, opcode, operation) in [
        (0xe1, 0xf5, VexOperation::MultiplyAddWords),
        (0xe2, 0x04, VexOperation::MultiplyAddBytes),
    ] {
        for (prefix, wide) in [(0x69, false), (0x6d, true)] {
            let decoded = X86ScalarDecoder::decode(&[0xc4, map, prefix, opcode, 0xcb], 0x8080).unwrap();
            assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
                operation: actual, destination: 1, first: 2, second: VectorSource::Register(3),
                wide: actual_wide, ..
            } if actual == operation && actual_wide == wide));
        }
    }
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x68, 0xf5, 0xcb], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6c, 0x04, 0xcb], 0).is_err());

    let repeat_word = |word: u16| (0..8).fold(0_u128, |value, lane| value | (u128::from(word) << (lane * 16)));
    let repeat_dword = |dword: u32| (0..4).fold(0_u128, |value, lane| value | (u128::from(dword) << (lane * 32)));
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x8080,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = repeat_word(0x8000);
    cpu.vector_upper[2] = repeat_word(0x7fff);
    cpu.vectors[3] = repeat_word(0x8000);
    cpu.vector_upper[3] = repeat_word(0x7fff);
    cpu.flags = cpu.flags.with(Flag::Carry, true).with(Flag::Overflow, true);
    let flags = cpu.flags;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let words = X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x6d, 0xf5, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, words),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], repeat_dword(0x8000_0000));
    assert_eq!(cpu.vector_upper[1], repeat_dword(0x7ffe_0002));
    assert_eq!(cpu.flags, flags);

    cpu.rip = 0x8090;
    cpu.vectors[2] = u128::MAX;
    cpu.vector_upper[2] = u128::MAX;
    cpu.vectors[3] = u128::MAX / 0xff * 0x7f;
    cpu.vector_upper[3] = u128::MAX / 0xff * 0x80;
    let bytes = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, 0x04, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, bytes),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], repeat_word(0x7fff));
    assert_eq!(cpu.vector_upper[1], repeat_word(0x8000));

    let mut legacy = CpuState {
        scalar: ScalarState {
            rip: 0x80a0,
            ..Default::default()
        },
        ..Default::default()
    };
    legacy.vectors[1] = repeat_word(0x8000);
    legacy.vectors[3] = repeat_word(0x8000);
    let legacy_words = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xf5, 0xcb], legacy.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut legacy, &mut memory, legacy_words),
        ExecutionExit::Continue
    );
    assert_eq!(legacy.vectors[1], repeat_dword(0x8000_0000));
    legacy.rip = 0x80b0;
    legacy.vectors[1] = u128::MAX;
    legacy.vectors[3] = u128::MAX / 0xff * 0x7f;
    let legacy_bytes = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x38, 0x04, 0xcb], legacy.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut legacy, &mut memory, legacy_bytes),
        ExecutionExit::Continue
    );
    assert_eq!(legacy.vectors[1], repeat_word(0x7fff));

    cpu.rip = 0x80c0;
    cpu.registers[3] = 0x1000;
    let original = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x6d, 0xf5, 0x0b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32)
    );
    assert_eq!(cpu, original);
}

#[test]
fn vex_sum_absolute_differences_family() {
    for (prefix, wide) in [(0x69, false), (0x6d, true), (0xed, true)] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe1, prefix, 0xf6, 0xcb], 0x80d0).unwrap();
        assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
            operation: VexOperation::SumAbsoluteDifferences, destination: 1, first: 2,
            second: VectorSource::Register(3), wide: actual_wide, ..
        } if actual_wide == wide));
    }
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x68, 0xf6, 0xcb], 0).is_err());

    let ascending = (0_u8..16).fold(0_u128, |value, lane| {
        value | (u128::from(lane) << (u32::from(lane) * 8))
    });
    let descending = (0_u8..16).fold(0_u128, |value, lane| {
        value | (u128::from(15 - lane) << (u32::from(lane) * 8))
    });
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x80d0,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = u128::MAX;
    cpu.vector_upper[2] = ascending;
    cpu.vectors[3] = 0;
    cpu.vector_upper[3] = descending;
    cpu.flags = cpu.flags.with(Flag::Carry, true).with(Flag::Overflow, true);
    let flags = cpu.flags;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let wide = X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x6d, 0xf6, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, wide),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 0x07f8_0000_0000_0000_07f8);
    assert_eq!(cpu.vector_upper[1], 0x0040_0000_0000_0000_0040);
    assert_eq!(cpu.flags, flags);

    let mut legacy = CpuState {
        scalar: ScalarState {
            rip: 0x80e0,
            ..Default::default()
        },
        ..Default::default()
    };
    legacy.vectors[1] = u128::MAX;
    legacy.vectors[3] = 0;
    let legacy_ir = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xf6, 0xcb], legacy.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut legacy, &mut memory, legacy_ir),
        ExecutionExit::Continue
    );
    assert_eq!(legacy.vectors[1], cpu.vectors[1]);

    cpu.rip = 0x80f0;
    cpu.registers[3] = 0x1000;
    let original = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x6d, 0xf6, 0x0b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32)
    );
    assert_eq!(cpu, original);
}

#[test]
fn vex_immediate_shift_family() {
    for (opcode, extension, operation, lane) in [
        (0x71, 2, VexImmediateShift::LogicalRight, 2),
        (0x71, 4, VexImmediateShift::ArithmeticRight, 2),
        (0x71, 6, VexImmediateShift::LogicalLeft, 2),
        (0x72, 2, VexImmediateShift::LogicalRight, 4),
        (0x72, 4, VexImmediateShift::ArithmeticRight, 4),
        (0x72, 6, VexImmediateShift::LogicalLeft, 4),
        (0x73, 2, VexImmediateShift::LogicalRight, 8),
        (0x73, 3, VexImmediateShift::ByteRight, 8),
        (0x73, 6, VexImmediateShift::LogicalLeft, 8),
        (0x73, 7, VexImmediateShift::ByteLeft, 8),
    ] {
        let bytes = [0xc5, 0xf5, opcode, 0xc0 | extension << 3 | 2, 7];
        let decoded = X86ScalarDecoder::decode(&bytes, 0x8000).unwrap();
        assert!(matches!(decoded.instruction, ScalarInstruction::VexImmediateShift {
            operation: actual, destination: 1, source: 2, lane: actual_lane, wide: true, count: 7,
        } if actual == operation && actual_lane == lane));
    }
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xf5, 0x73, 0xe2, 1], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xf5, 0x72, 0x10, 1], 0).is_err());

    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x9000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = 0x8000_0000_7fff_ffff_0000_0001_ffff_ffff;
    cpu.vector_upper[2] = 0x7fff_ffff_8000_0000_ffff_ffff_0000_0001;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let logical = X86ScalarDecoder::decode(&[0xc5, 0xf5, 0x72, 0xd2, 31], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, logical),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 0x0000_0001_0000_0000_0000_0000_0000_0001);
    assert_eq!(cpu.vector_upper[1], 0x0000_0000_0000_0001_0000_0001_0000_0000);

    cpu.rip = 0x9010;
    let arithmetic = X86ScalarDecoder::decode(&[0xc5, 0xf5, 0x72, 0xe2, 40], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, arithmetic),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 0xffff_ffff_0000_0000_0000_0000_ffff_ffff);
    assert_eq!(cpu.vector_upper[1], 0x0000_0000_ffff_ffff_ffff_ffff_0000_0000);

    cpu.vectors[2] = 0x0f0e_0d0c_0b0a_0908_0706_0504_0302_0100;
    cpu.vector_upper[2] = 0x1f1e_1d1c_1b1a_1918_1716_1514_1312_1110;
    cpu.rip = 0x9020;
    let bytes = X86ScalarDecoder::decode(&[0xc5, 0xf5, 0x73, 0xda, 4], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, bytes),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 0x0000_0000_0f0e_0d0c_0b0a_0908_0706_0504);
    assert_eq!(cpu.vector_upper[1], 0x0000_0000_1f1e_1d1c_1b1a_1918_1716_1514);
}

#[test]
fn vex_scalar_count_packed_shift_family() {
    for (opcode, operation, lane) in [
        (0xd1, VexImmediateShift::LogicalRight, 2),
        (0xd2, VexImmediateShift::LogicalRight, 4),
        (0xd3, VexImmediateShift::LogicalRight, 8),
        (0xe1, VexImmediateShift::ArithmeticRight, 2),
        (0xe2, VexImmediateShift::ArithmeticRight, 4),
        (0xf1, VexImmediateShift::LogicalLeft, 2),
        (0xf2, VexImmediateShift::LogicalLeft, 4),
        (0xf3, VexImmediateShift::LogicalLeft, 8),
    ] {
        for (wide, prefix) in [(false, 0xe9), (true, 0xed)] {
            let decoded = X86ScalarDecoder::decode(&[0xc5, prefix, opcode, 0xca], 0xa000).unwrap();
            assert!(matches!(decoded.instruction, ScalarInstruction::VexScalarCountShift {
                operation: actual, destination: 1, source: 2, count: VectorSource::Register(2),
                lane: actual_lane, wide: actual_wide,
            } if actual == operation && actual_lane == lane && actual_wide == wide));
        }
    }
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xec, 0xd1, 0xca], 0).is_err());

    let flags = FlagState::from_bits(0x8d5);
    let source_low = 0x8000_7fff_ffff_0001_1234_5678_abcd_ef01_u128;
    let source_high = 0xffff_8000_7fff_0001_aaaa_5555_1357_2468_u128;
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0xa100,
            flags,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = source_low;
    cpu.vector_upper[2] = source_high;
    cpu.vectors[3] = 4;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let wide = X86ScalarDecoder::decode(&[0xc5, 0xed, 0xe1, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, wide),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[2], source_low);
    assert_eq!(cpu.vector_upper[2], source_high);
    assert_eq!(cpu.flags, flags);
    assert_eq!(cpu.vectors[1] as u16, 0xfef0);
    assert_eq!((cpu.vector_upper[1] >> 112) as u16, 0xffff);

    cpu.rip = 0xa110;
    cpu.vectors[3] = 64;
    cpu.vector_upper[1] = u128::MAX;
    let narrow = X86ScalarDecoder::decode(&[0xc5, 0xe9, 0xf2, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, narrow),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], 0);
    assert_eq!(cpu.vector_upper[1], 0);
    assert_eq!(cpu.flags, flags);

    cpu.rip = 0xa120;
    cpu.registers[3] = 0x1000;
    let original = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe1, 0x6d, 0xd3, 0x0b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 16],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, original);
}

#[test]
fn vex_f16c_startup_prerequisites_decode() {
    for bytes in [
        &[0xc5, 0xf9, 0x6e, 0xc0][..],
        &[0xc4, 0x41, 0x32, 0x2a, 0xd0][..],
        &[0xc5, 0xf9, 0x70, 0xc0, 0x00][..],
        &[0xc5, 0xf8, 0x5b, 0xc9][..],
        &[0xc4, 0x41, 0x28, 0xc6, 0xd2, 0x00][..],
        &[0xc4, 0xc3, 0x71, 0x4a, 0xcb, 0x20][..],
    ] {
        X86ScalarDecoder::decode(bytes, 0x1000).unwrap();
    }
}

#[test]
fn vex_horizontal_float_lanes_and_faults() {
    let singles = |values: [f32; 4]| {
        values.into_iter().enumerate().fold(0_u128, |bits, (lane, value)| {
            bits | (u128::from(value.to_bits()) << (lane * 32))
        })
    };
    let mut memory = ModelMemory {
        base: 0x2000,
        bytes: vec![0; 32],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x6000,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[1] = singles([1.0, 2.0, 4.0, 8.0]);
    cpu.vectors[2] = singles([16.0, 32.0, 64.0, 128.0]);
    let haddps = X86ScalarDecoder::decode(&[0xc5, 0xf3, 0x7c, 0xc2], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, haddps),
        ExecutionExit::Continue
    );
    assert_eq!(
        (cpu.vectors[0], cpu.vector_upper[0]),
        (singles([3.0, 12.0, 48.0, 192.0]), 0)
    );

    cpu.rip = 0x6010;
    cpu.vectors[1] = u128::from(9.0_f64.to_bits()) | (u128::from(4.0_f64.to_bits()) << 64);
    cpu.vector_upper[1] = u128::from(30.0_f64.to_bits()) | (u128::from(10.0_f64.to_bits()) << 64);
    cpu.vectors[2] = u128::from(7.0_f64.to_bits()) | (u128::from(2.0_f64.to_bits()) << 64);
    cpu.vector_upper[2] = u128::from(20.0_f64.to_bits()) | (u128::from(3.0_f64.to_bits()) << 64);
    let hsubpd = X86ScalarDecoder::decode(&[0xc5, 0xf5, 0x7d, 0xc2], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, hsubpd),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[0],
        u128::from(5.0_f64.to_bits()) | (u128::from(5.0_f64.to_bits()) << 64)
    );
    assert_eq!(
        cpu.vector_upper[0],
        u128::from(20.0_f64.to_bits()) | (u128::from(17.0_f64.to_bits()) << 64)
    );

    cpu.rip = 0x6018;
    cpu.vectors[0] = u128::from(10.0_f64.to_bits()) | (u128::from(20.0_f64.to_bits()) << 64);
    cpu.vector_upper[0] = u128::from(30.0_f64.to_bits()) | (u128::from(40.0_f64.to_bits()) << 64);
    cpu.vectors[1] = u128::from(1.0_f64.to_bits()) | (u128::from(2.0_f64.to_bits()) << 64);
    cpu.vector_upper[1] = u128::from(3.0_f64.to_bits()) | (u128::from(4.0_f64.to_bits()) << 64);
    let addsubpd = X86ScalarDecoder::decode(&[0xc5, 0xfd, 0xd0, 0xc9], cpu.rip).unwrap();
    assert!(matches!(
        addsubpd.instruction,
        ScalarInstruction::VexPairArithmetic {
            format: FloatWidth::Double,
            subtract: false,
            alternating: true,
            wide: true,
            ..
        }
    ));
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, addsubpd),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[1],
        u128::from(9.0_f64.to_bits()) | (u128::from(22.0_f64.to_bits()) << 64)
    );
    assert_eq!(
        cpu.vector_upper[1],
        u128::from(27.0_f64.to_bits()) | (u128::from(44.0_f64.to_bits()) << 64)
    );

    cpu.rip = 0x6020;
    cpu.registers[0] = 0x2000;
    memory.fail_read = true;
    let before = cpu.clone();
    let load = X86ScalarDecoder::decode(&[0xc5, 0xfd, 0xd0, 0x08], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(access) if access.length() == 32
    ));
    assert_eq!(cpu, before);
    assert!(X86ScalarDecoder::decode(&[0xc5, 0xfc, 0xd0, 0xc0], 0).is_err());
}

#[test]
fn selector_moves() {
    let cases = [
        (&[0x66, 0x8c, 0xd0][..], 0x1122_3344_5566_002b, ScalarWidth::Word),
        (&[0x8c, 0xc8], 0x33, ScalarWidth::Dword),
        (&[0x48, 0x8c, 0xc8], 0x33, ScalarWidth::Qword),
        (&[0x44, 0x8c, 0xf0], 0, ScalarWidth::Dword),
    ];
    for (bytes, expected, width) in cases {
        let ir = X86ScalarDecoder::decode(bytes, 0x4000).unwrap();
        assert_eq!(ir.width, width);
        let mut cpu = CpuState {
            scalar: ScalarState {
                registers: [0x1122_3344_5566_7788; 16],
                rip: 0x4000,
                ..Default::default()
            },
            ..Default::default()
        };
        let flags = cpu.flags;
        let mut memory = ModelMemory {
            base: 0,
            bytes: Vec::new(),
            fail_read: true,
            fail_write: true,
            commits: 0,
        };
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[0], expected);
        assert_eq!(cpu.flags, flags);
    }
}

#[test]
fn selector_memory_and_discard() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            registers: [0x1000; 16],
            rip: 0x5000,
            fs_base: 0x2000,
            gs_base: 0x3000,
            flags: FlagState::from_bits(0x8d5),
            ..Default::default()
        },
        ..Default::default()
    };
    let before = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0xaa; 2],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let store = X86ScalarDecoder::decode(&[0x8c, 0x10], cpu.rip).unwrap();
    assert_eq!(store.width, ScalarWidth::Word);
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::Continue
    );
    assert_eq!(memory.bytes, [0x2b, 0]);
    assert_eq!(memory.commits, 1);
    assert_eq!(cpu.flags, before.flags);
    assert_eq!((cpu.fs_base, cpu.gs_base), (before.fs_base, before.gs_base));

    cpu.rip = 0x6000;
    memory.fail_read = true;
    let load = X86ScalarDecoder::decode(&[0x8e, 0x28], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.rip, 0x6002);
    assert_eq!((cpu.fs_base, cpu.gs_base), (before.fs_base, before.gs_base));
    assert_eq!(cpu.flags, before.flags);
}

#[test]
fn selector_store_fault_rolls_back() {
    let mut cpu = CpuState {
        scalar: ScalarState {
            registers: [0x1000; 16],
            rip: 0x7000,
            flags: FlagState::from_bits(0x8d5),
            ..Default::default()
        },
        ..Default::default()
    };
    let before = cpu.clone();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0xaa; 2],
        fail_read: false,
        fail_write: true,
        commits: 0,
    };
    let store = X86ScalarDecoder::decode(&[0x8c, 0x08], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, store),
        ExecutionExit::OperandFault(access) if access.length() == 2
    ));
    assert_eq!(cpu, before);
    assert_eq!(memory.bytes, [0xaa; 2]);

    assert!(X86ScalarDecoder::decode(&[0xf0, 0x8c, 0xc8], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x8e, 0xc8], 0).is_err());
}

#[test]
fn iretq_restores_frame() {
    let frame = [0x8000_u64, 0x33, 0x240cc5, 0x9000, 0x2b];
    let bytes = frame.into_iter().flat_map(u64::to_le_bytes).collect();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes,
        fail_read: false,
        fail_write: true,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            registers: [0xaaaa; 16],
            rip: 0x7000,
            direction: false,
            alignment_check: false,
            id_flag: false,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[4] = 0x1000;
    let instruction = X86ScalarDecoder::decode(&[0x66, 0x48, 0xcf], cpu.rip).unwrap();
    assert_eq!(instruction.instruction, ScalarInstruction::Iret);
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!((cpu.rip, cpu.registers[4]), (0x8000, 0x9000));
    assert_eq!(cpu.flags.bits(), 0x8c5);
    assert!(cpu.direction);
    assert!(cpu.alignment_check);
    assert!(cpu.id_flag);
    assert_eq!(memory.commits, 0);
}

#[test]
fn iretq_faults_roll_back() {
    let frame = [0x8000_u64, 0x33, 0x8c5, 0x9000, 0x2b];
    let mut bytes = frame.into_iter().flat_map(u64::to_le_bytes).collect::<Vec<_>>();
    bytes.pop();
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes,
        fail_read: false,
        fail_write: true,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            registers: [0xaaaa; 16],
            rip: 0x7000,
            flags: FlagState::from_bits(0x55),
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[4] = 0x1000;
    let before = cpu.clone();
    let instruction = X86ScalarDecoder::decode(&[0x48, 0xcf], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::OperandFault(access) if access.address() == 0x1020 && access.length() == 8
    ));
    assert_eq!(cpu, before);

    memory.bytes = [0x0001_0000_0000_0000_u64, 0x33, 0x8c5, 0x9000, 0x2b]
        .into_iter()
        .flat_map(u64::to_le_bytes)
        .collect();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::NonCanonical {
            access: AccessKind::Execute,
            ..
        }
    ));
    assert_eq!(cpu, before);
}

#[test]
fn iretq_prefix_contract() {
    assert!(X86ScalarDecoder::decode(&[0xcf], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0x66, 0xcf], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf0, 0x48, 0xcf], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x48, 0xcf], 0).is_err());
    assert_eq!(
        X86ScalarDecoder::decode(&[0x64, 0x48, 0xcf], 0).unwrap().instruction,
        ScalarInstruction::Iret
    );
}

#[test]
fn ptest_defines_only_zero_and_carry_results() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let cases = [
        (0_u128, 1_u128, true, false),
        (1, 1, false, true),
        (0b0101, 0b1010, true, false),
        (0b0101, 0b0011, false, false),
    ];
    for (left, right, zero, carry) in cases {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x7000,
                flags: FlagState::from_bits(u16::MAX),
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[1] = left;
        cpu.vectors[2] = right;
        let ir = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x38, 0x17, 0xca], cpu.rip).unwrap();
        assert_eq!(
            ir.instruction,
            ScalarInstruction::VectorTest {
                left: 1,
                right: VectorSource::Register(2),
            }
        );
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.flags.contains(Flag::Zero), zero);
        assert_eq!(cpu.flags.contains(Flag::Carry), carry);
        for cleared in [Flag::Overflow, Flag::Sign, Flag::Auxiliary, Flag::Parity] {
            assert!(!cpu.flags.contains(cleared));
        }
        assert_eq!(cpu.rip, 0x7005);
    }
}

#[test]
fn ptest_memory_fault_preserves_all_architectural_state() {
    let mut memory = ModelMemory {
        base: 0x2000,
        bytes: vec![0; 16],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7100,
            flags: FlagState::from_bits(0x8d5),
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[0] = 0x2000;
    cpu.vectors[1] = u128::MAX;
    let before = cpu.clone();
    let ir = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x38, 0x17, 0x08], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, ir),
        ExecutionExit::OperandFault(access) if access.address() == 0x2000 && access.length() == 16
    ));
    assert_eq!(cpu, before);
}

#[test]
fn ptest_rejects_wrong_mandatory_prefixes() {
    assert!(X86ScalarDecoder::decode(&[0x0f, 0x38, 0x17, 0xc0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x38, 0x17, 0xc0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x38, 0x17, 0xc0], 0).is_err());
}

#[test]
fn legacy_sse41_round_family_covers_all_shapes() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7200,
            ..Default::default()
        },
        ..Default::default()
    };

    cpu.vectors[2] = [1.5_f32, -1.5, 2.1, -2.1]
        .into_iter()
        .enumerate()
        .fold(0_u128, |bits, (lane, value)| {
            bits | (u128::from(value.to_bits()) << (lane * 32))
        });
    let roundps = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x3a, 0x08, 0xca, 0x00], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, roundps),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[1],
        [2.0_f32, -2.0, 2.0, -2.0]
            .into_iter()
            .enumerate()
            .fold(0_u128, |bits, (lane, value)| bits
                | (u128::from(value.to_bits()) << (lane * 32)))
    );

    cpu.rip = 0x7210;
    cpu.vectors[2] = u128::from(1.9_f64.to_bits()) | (u128::from((-1.1_f64).to_bits()) << 64);
    let roundpd = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x3a, 0x09, 0xca, 0x01], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, roundpd),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[1],
        u128::from(1.0_f64.to_bits()) | (u128::from((-2.0_f64).to_bits()) << 64)
    );

    cpu.rip = 0x7220;
    cpu.vectors[1] = 0xfeed_face_dead_beef_0123_4567_89ab_cdef;
    cpu.vectors[2] = u128::from(1.1_f32.to_bits());
    cpu.mxcsr = 0x1f80 | (2 << 13);
    let roundss = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x3a, 0x0a, 0xca, 0x04], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, roundss),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1] >> 32, 0xfeed_face_dead_beef_0123_4567);
    assert_eq!(cpu.vectors[1] as u32, 2.0_f32.to_bits());

    cpu.rip = 0x7230;
    cpu.vectors[1] = 0xaabb_ccdd_eeff_0011_0123_4567_89ab_cdef;
    cpu.vectors[2] = u128::from((-1.9_f64).to_bits());
    let roundsd = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x3a, 0x0b, 0xca, 0x03], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, roundsd),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1] >> 64, 0xaabb_ccdd_eeff_0011);
    assert_eq!(cpu.vectors[1] as u64, (-1.0_f64).to_bits());
}

#[test]
fn legacy_sse41_round_flags_faults_and_prefixes() {
    let mut memory = ModelMemory {
        base: 0x2000,
        bytes: vec![0; 16],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7300,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[1] = u128::MAX;
    cpu.registers[0] = 0x2000;
    let before = cpu.clone();
    let load = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x3a, 0x08, 0x08, 0x00], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, load),
        ExecutionExit::OperandFault(_)
    ));
    assert_eq!(cpu, before);

    memory.fail_read = false;
    cpu.vectors[2] = u128::from(1.25_f32.to_bits());
    let quiet = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x3a, 0x0a, 0xca, 0x08], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, quiet),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.mxcsr & (1 << 5), 0);

    for bytes in [
        &[0x0f, 0x3a, 0x08, 0xc0, 0][..],
        &[0xf2, 0x0f, 0x3a, 0x08, 0xc0, 0],
        &[0xf3, 0x0f, 0x3a, 0x08, 0xc0, 0],
        &[0xf0, 0x66, 0x0f, 0x3a, 0x08, 0xc0, 0],
    ] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_err());
    }
}

#[test]
fn legacy_sse41_lane_transfer_register_memory_faults_and_prefixes() {
    let mut memory = ModelMemory {
        base: 0x3000,
        bytes: vec![0x78, 0x56, 0x34, 0x12, 0, 0, 0, 0],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7400,
            flags: FlagState::from_bits(0x8d5),
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[8] = 0x0123_4567_89ab_cdef;
    cpu.vectors[9] = 0x0011_2233_4455_6677_8899_aabb_ccdd_eeff;
    let pinsrb = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x3a, 0x20, 0xc8, 0x1f], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, pinsrb),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[9] >> 120, 0xef);
    cpu.rip = 0x7410;
    let pextrd = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x3a, 0x16, 0xc8, 0x02], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, pextrd),
        ExecutionExit::Continue
    );
    assert_eq!(
        (cpu.registers[8], cpu.flags),
        (0x4455_6677, FlagState::from_bits(0x8d5))
    );
    cpu.rip = 0x7420;
    cpu.registers[0] = 0x3000;
    let pinsrq = X86ScalarDecoder::decode(&[0x66, 0x4c, 0x0f, 0x3a, 0x22, 0x08, 1], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, pinsrq),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[9] >> 64, 0x0000_0000_1234_5678);
    cpu.rip = 0x7430;
    memory.fail_write = true;
    let before = cpu.clone();
    let pextrq = X86ScalarDecoder::decode(&[0x66, 0x4c, 0x0f, 0x3a, 0x16, 0x08, 1], cpu.rip).unwrap();
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut memory, pextrq), ExecutionExit::OperandFault(access) if access.length() == 8)
    );
    assert_eq!(cpu, before);
    for bytes in [
        &[0x0f, 0x3a, 0x20, 0xc0, 0][..],
        &[0xf2, 0x0f, 0x3a, 0x16, 0xc0, 0],
        &[0xf0, 0x66, 0x0f, 0x3a, 0x14, 0xc0, 0],
    ] {
        assert!(X86ScalarDecoder::decode(bytes, 0).is_err());
    }
}

fn crc32c_reference(mut crc: u32, value: u64, bytes: u8) -> u32 {
    for byte in 0..bytes {
        crc ^= (value >> (u32::from(byte) * 8)) as u8 as u32;
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0x82f6_3b78 & 0_u32.wrapping_sub(crc & 1));
        }
    }
    crc
}

#[test]
fn legacy_sse42_crc32c_covers_widths_registers_and_zero_extension() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let flags = FlagState::from_bits(0x8d5);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7500,
            flags,
            ..Default::default()
        },
        ..Default::default()
    };

    cpu.registers[0] = 0x0000_bc00;
    cpu.registers[1] = 0xffff_ffff_ffff_ffff;
    let byte = X86ScalarDecoder::decode(&[0xf2, 0x0f, 0x38, 0xf0, 0xcc], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, byte),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.registers[1], u64::from(crc32c_reference(u32::MAX, 0xbc, 1)));

    cpu.rip = 0x7510;
    cpu.registers[8] = 0x0123_4567_89ab_cdef;
    cpu.registers[9] = 0xaaaa_bbbb_ffff_ffff;
    let word = X86ScalarDecoder::decode(&[0x66, 0xf2, 0x45, 0x0f, 0x38, 0xf1, 0xc8], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, word),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.registers[9],
        u64::from(crc32c_reference(u32::MAX, cpu.registers[8], 2))
    );

    cpu.rip = 0x7520;
    cpu.registers[9] = 0xffff_ffff_1234_5678;
    let dword = X86ScalarDecoder::decode(&[0xf2, 0x45, 0x0f, 0x38, 0xf1, 0xc8], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, dword),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.registers[9],
        u64::from(crc32c_reference(0x1234_5678, cpu.registers[8], 4))
    );

    cpu.rip = 0x7530;
    cpu.registers[9] = 0xffff_ffff_89ab_cdef;
    let qword = X86ScalarDecoder::decode(&[0xf2, 0x4d, 0x0f, 0x38, 0xf1, 0xc8], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, qword),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.registers[9],
        u64::from(crc32c_reference(0x89ab_cdef, cpu.registers[8], 8))
    );
    assert_eq!(cpu.flags, flags);
}

#[test]
fn legacy_sse42_crc32c_memory_fault_is_atomic_and_prefix_is_mandatory() {
    let mut memory = ModelMemory {
        base: 0x3000,
        bytes: 0x0123_4567_89ab_cdef_u64.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7600,
            flags: FlagState::from_bits(0x8d5),
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[0] = 0x3000;
    cpu.registers[9] = 0xffff_ffff_dead_beef;
    let qword = X86ScalarDecoder::decode(&[0xf2, 0x4c, 0x0f, 0x38, 0xf1, 0x08], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, qword),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.registers[9],
        u64::from(crc32c_reference(0xdead_beef, 0x0123_4567_89ab_cdef, 8))
    );

    cpu.rip = 0x7610;
    cpu.registers[9] = 0xffff_ffff_dead_beef;
    memory.fail_read = true;
    let before = cpu.clone();
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut memory, qword), ExecutionExit::OperandFault(access) if access.length() == 8)
    );
    assert_eq!(cpu, before);

    assert!(X86ScalarDecoder::decode(&[0xf3, 0x0f, 0x38, 0xf0, 0xc0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf0, 0xf2, 0x0f, 0x38, 0xf1, 0xc0], 0).is_err());
}

fn carryless_reference(left: u64, right: u64) -> u128 {
    (0..64).fold(0_u128, |product, bit| {
        if right >> bit & 1 == 0 {
            product
        } else {
            product ^ (u128::from(left) << bit)
        }
    })
}

#[test]
fn legacy_pclmul_selects_halves_extended_registers_and_aliases() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let flags = FlagState::from_bits(0x8d5);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7700,
            flags,
            ..Default::default()
        },
        ..Default::default()
    };
    let left = 0xfedc_ba98_7654_3210_0123_4567_89ab_cdef_u128;
    let right = 0x8877_6655_4433_2211_0f1e_2d3c_4b5a_6978_u128;
    for (control, left_half, right_half) in [
        (0x00, left as u64, right as u64),
        (0x01, (left >> 64) as u64, right as u64),
        (0x10, left as u64, (right >> 64) as u64),
        (0xff, (left >> 64) as u64, (right >> 64) as u64),
    ] {
        cpu.rip = 0x7700;
        cpu.vectors[8] = left;
        cpu.vectors[9] = right;
        let bytes = [0x66, 0x45, 0x0f, 0x3a, 0x44, 0xc1, control];
        let instruction = X86ScalarDecoder::decode(&bytes, cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.vectors[8], carryless_reference(left_half, right_half));
        assert_eq!(cpu.flags, flags);
    }

    cpu.rip = 0x7710;
    cpu.vectors[8] = left;
    let alias = X86ScalarDecoder::decode(&[0x66, 0x45, 0x0f, 0x3a, 0x44, 0xc0, 0x11], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, alias),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[8],
        carryless_reference((left >> 64) as u64, (left >> 64) as u64)
    );
}

#[test]
fn legacy_pclmul_memory_fault_is_transactional_and_prefix_is_mandatory() {
    let source = 0x8877_6655_4433_2211_0f1e_2d3c_4b5a_6978_u128;
    let mut memory = ModelMemory {
        base: 0x3000,
        bytes: source.to_le_bytes().to_vec(),
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x7800,
            flags: FlagState::from_bits(0x8d5),
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.registers[0] = 0x3000;
    cpu.vectors[9] = 0xfedc_ba98_7654_3210_0123_4567_89ab_cdef_u128;
    let instruction = X86ScalarDecoder::decode(&[0x66, 0x44, 0x0f, 0x3a, 0x44, 0x08, 0x10], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, instruction),
        ExecutionExit::Continue
    );
    assert_eq!(
        cpu.vectors[9],
        carryless_reference(0x0123_4567_89ab_cdef, 0x8877_6655_4433_2211)
    );

    cpu.rip = 0x7810;
    cpu.vectors[9] = u128::MAX;
    memory.fail_read = true;
    let before = cpu.clone();
    let faulting = X86ScalarDecoder::decode(&[0x66, 0x44, 0x0f, 0x3a, 0x44, 0x08, 0x10], cpu.rip).unwrap();
    assert!(
        matches!(ScalarInterpreter::execute(&mut cpu, &mut memory, faulting), ExecutionExit::OperandFault(access) if access.length() == 16)
    );
    assert_eq!(cpu, before);
    assert!(X86ScalarDecoder::decode(&[0x0f, 0x3a, 0x44, 0xc0, 0], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xf3, 0x66, 0x0f, 0x3a, 0x44, 0xc0, 0], 0).is_err());
}

#[test]
fn vex_horizontal_integer_family() {
    for (opcode, subtract, saturating, dword) in [
        (0x01, false, false, false),
        (0x02, false, false, true),
        (0x03, false, true, false),
        (0x05, true, false, false),
        (0x06, true, false, true),
        (0x07, true, true, false),
    ] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, opcode, 0xcb], 0x8100).unwrap();
        assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
            operation: VexOperation::Horizontal {
                subtract: actual_subtract, saturating: actual_saturating, dword: actual_dword,
            }, destination: 1, first: 2, second: VectorSource::Register(3), wide: true, ..
        } if (actual_subtract, actual_saturating, actual_dword) == (subtract, saturating, dword)));
    }
    let words = |values: [u16; 8]| {
        values
            .into_iter()
            .enumerate()
            .fold(0_u128, |bits, (lane, value)| bits | (u128::from(value) << (lane * 16)))
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x8100,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = words([1, 2, 3, 4, 5, 6, 7, 8]);
    cpu.vector_upper[2] = words([9, 10, 11, 12, 13, 14, 15, 16]);
    cpu.vectors[3] = words([10, 20, 30, 40, 50, 60, 70, 80]);
    cpu.vector_upper[3] = words([90, 100, 110, 120, 130, 140, 150, 160]);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let add = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, 0x01, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, add),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1], words([3, 7, 11, 15, 30, 70, 110, 150]));
    assert_eq!(cpu.vector_upper[1], words([19, 23, 27, 31, 190, 230, 270, 310]));

    cpu.rip = 0x8110;
    cpu.vectors[2] = words([0x7fff, 1, 0x8000, 1, 0, 0, 0, 0]);
    cpu.vectors[3] = words([0x8000, 1, 0x7fff, 0xffff, 0, 0, 0, 0]);
    let saturated = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x69, 0x03, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, saturated),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1] as u64, 0x0000_0000_8001_7fff);
    assert_eq!(cpu.vector_upper[1], 0);

    cpu.rip = 0x8120;
    cpu.registers[3] = 0x1000;
    let before = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, 0x05, 0x0b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32
    ));
    assert_eq!(cpu, before);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6c, 0x01, 0xcb], 0).is_err());
}

#[test]
fn vex_packed_sign_family() {
    for (opcode, bytes) in [(0x08, 1), (0x09, 2), (0x0a, 4)] {
        for (prefix, wide) in [(0x69, false), (0x6d, true)] {
            let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, prefix, opcode, 0xcb], 0x8130).unwrap();
            assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
                operation: VexOperation::Sign { bytes: actual }, destination: 1, first: 2,
                second: VectorSource::Register(3), wide: actual_wide, ..
            } if actual == bytes && actual_wide == wide));
        }
    }
    let bytes = |values: [u8; 16]| u128::from_le_bytes(values);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x8130,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = bytes([1, 2, 0x80, 0xff, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16]);
    cpu.vector_upper[2] = bytes([17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32]);
    cpu.vectors[3] = bytes([1, 0, 0xff, 0x80, 1, 0, 0xff, 1, 1, 1, 1, 1, 1, 1, 1, 1]);
    cpu.vector_upper[3] = bytes([0xff, 0, 1, 0xff, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1]);
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let signed = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, 0x08, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, signed),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1].to_le_bytes()[..8], [1, 0, 0x80, 1, 5, 0, 0xf9, 8]);
    assert_eq!(cpu.vector_upper[1].to_le_bytes()[..4], [0xef, 0, 19, 0xec]);

    cpu.rip = 0x8140;
    cpu.vectors[2] = u128::from(0x8000_0000_u32) | (u128::from(7_u32) << 32);
    cpu.vectors[3] = u128::from(u32::MAX) | (u128::from(0_u32) << 32);
    let dwords = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x69, 0x0a, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, dwords),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1] as u64, 0x0000_0000_8000_0000);
    assert_eq!(cpu.vector_upper[1], 0);

    cpu.rip = 0x8150;
    cpu.registers[3] = 0x1000;
    let before = cpu.clone();
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, 0x09, 0x0b], cpu.rip).unwrap();
    let mut faulting = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: true,
        fail_write: false,
        commits: 0,
    };
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut faulting, from_memory),
        ExecutionExit::OperandFault(access) if access.length() == 32
    ));
    assert_eq!(cpu, before);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6c, 0x08, 0xcb], 0).is_err());
}

#[test]
fn vex_packed_absolute_family() {
    for (opcode, bytes) in [(0x1c, 1), (0x1d, 2), (0x1e, 4)] {
        for (prefix, wide) in [(0x79, false), (0x7d, true)] {
            let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, prefix, opcode, 0xcb], 0x8160).unwrap();
            assert!(matches!(decoded.instruction, ScalarInstruction::VexBinary {
                operation: VexOperation::Absolute { bytes: actual }, destination: 1, first: 0,
                second: VectorSource::Register(3), wide: actual_wide, ..
            } if actual == bytes && actual_wide == wide));
        }
    }
    let bytes = |values: [u8; 16]| u128::from_le_bytes(values);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x8160,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[3] = bytes([0, 1, 0xff, 0x80, 0x7f, 0xfe, 6, 0xfa, 8, 9, 10, 11, 12, 13, 14, 15]);
    cpu.vector_upper[3] = bytes([
        0x80, 0xff, 2, 0xfe, 4, 0xfc, 6, 0xfa, 8, 0xf8, 10, 0xf6, 12, 0xf4, 14, 0xf2,
    ]);
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let wide = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7d, 0x1c, 0xcb], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, wide),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1].to_le_bytes()[..8], [0, 1, 1, 0x80, 0x7f, 2, 6, 6]);
    assert_eq!(cpu.vector_upper[1].to_le_bytes()[..6], [0x80, 1, 2, 2, 4, 4]);

    cpu.rip = 0x8170;
    cpu.registers[3] = 0x1000;
    memory.bytes[..16]
        .copy_from_slice(&bytes([0, 0, 0, 0x80, 7, 0, 0, 0, 0xff, 0xff, 0xff, 0xff, 1, 0, 0, 0]).to_le_bytes());
    let from_memory = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x79, 0x1e, 0x0b], cpu.rip).unwrap();
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, from_memory),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1] as u64, 0x0000_0007_8000_0000);
    assert_eq!(cpu.vector_upper[1], 0);

    cpu.rip = 0x8180;
    memory.fail_read = true;
    let before = cpu.clone();
    let faulting = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7d, 0x1d, 0x0b], cpu.rip).unwrap();
    assert!(matches!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, faulting),
        ExecutionExit::OperandFault(access) if access.length() == 32
    ));
    assert_eq!(cpu, before);
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x75, 0x1c, 0xcb], 0).is_err());
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7c, 0x1c, 0xcb], 0).is_err());
}

#[test]
fn vex_high_round_word_family() {
    let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6d, 0x0b, 0xcb], 0x8190).unwrap();
    assert!(matches!(
        decoded.instruction,
        ScalarInstruction::VexBinary {
            operation: VexOperation::MultiplyHighRoundWord,
            destination: 1,
            first: 2,
            second: VectorSource::Register(3),
            wide: true,
            ..
        }
    ));
    let words = |values: [i16; 8]| {
        values.into_iter().enumerate().fold(0_u128, |bits, (lane, value)| {
            bits | (u128::from(value as u16) << (lane * 16))
        })
    };
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x8190,
            ..Default::default()
        },
        ..Default::default()
    };
    cpu.vectors[2] = words([0x4000, -0x4000, 0x7fff, i16::MIN, 1, -1, 12345, -23456]);
    cpu.vectors[3] = words([0x4000, 0x4000, 0x7fff, i16::MIN, -1, -1, -12345, 23456]);
    cpu.vector_upper[2] = words([0x4000; 8]);
    cpu.vector_upper[3] = words([-0x4000; 8]);
    let expected = |a: i16, b: i16| ((((i32::from(a) * i32::from(b)) >> 14) + 1) >> 1) as i16;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    assert_eq!(
        ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
        ExecutionExit::Continue
    );
    assert_eq!(cpu.vectors[1] as u16 as i16, expected(0x4000, 0x4000));
    assert_eq!((cpu.vectors[1] >> 48) as u16, 0x8000);
    assert_eq!(cpu.vector_upper[1], words([expected(0x4000, -0x4000); 8]));
    assert!(X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x6c, 0x0b, 0xcb], 0).is_err());
}

/// A qNaN source operand propagates verbatim through scalar arithmetic, sign bit included.
#[test]
fn scalar_arithmetic_propagates_negative_quiet_nan_verbatim() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let nan = 0xfff8_0000_0000_0042_u64;
    for (opcode, name) in [(0x58_u8, "addsd"), (0x5c, "subsd"), (0x59, "mulsd"), (0x5e, "divsd")] {
        for (destination, source) in [(0x3ff0_0000_0000_0000_u64, nan), (nan, 0x3ff0_0000_0000_0000)] {
            let decoded = X86ScalarDecoder::decode(&[0xf2, 0x0f, opcode, 0xc1], 0x4b100).unwrap();
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x4b100,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.vectors[0] = u128::from(destination);
            cpu.vectors[1] = u128::from(source);
            cpu.mxcsr = 0x1f80;
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
                ExecutionExit::Continue
            );
            assert_eq!(cpu.vectors[0] as u64, nan, "{name} destination {destination:#x}");
        }
    }
}

fn packed_compare_reference(left: u128, right: u128, lane: usize, greater: bool) -> u128 {
    let left_bytes = left.to_le_bytes();
    let right_bytes = right.to_le_bytes();
    let mut output = [0_u8; 16];
    for index in (0..16).step_by(lane) {
        let a = &left_bytes[index..index + lane];
        let b = &right_bytes[index..index + lane];
        let selected = match (lane, greater) {
            (1, false) => a[0] == b[0],
            (1, true) => a[0] as i8 > b[0] as i8,
            (2, false) => a == b,
            (2, true) => i16::from_le_bytes([a[0], a[1]]) > i16::from_le_bytes([b[0], b[1]]),
            (4, false) => a == b,
            (4, true) => i32::from_le_bytes([a[0], a[1], a[2], a[3]]) > i32::from_le_bytes([b[0], b[1], b[2], b[3]]),
            (8, false) => a == b,
            (8, true) => {
                i64::from_le_bytes([a[0], a[1], a[2], a[3], a[4], a[5], a[6], a[7]])
                    > i64::from_le_bytes([b[0], b[1], b[2], b[3], b[4], b[5], b[6], b[7]])
            }
            _ => unreachable!(),
        };
        if selected {
            output[index..index + lane].fill(0xff);
        }
    }
    u128::from_le_bytes(output)
}

/// `vpcmpeq`/`vpcmpgt` at every element width, against a reference built from Rust's
/// signed integer types rather than the sign-flip trick the interpreter uses.
#[test]
fn vex_integer_compare_family() {
    let low_left = 0x8000_0000_7fff_ffff_0102_0304_ffff_ff00_u128;
    let low_right = 0x8000_0001_7fff_fffe_0102_0304_0000_00ff_u128;
    let high_left = 0x0000_0000_ffff_ffff_8080_8080_7f7f_7f7f_u128;
    let high_right = 0x0000_0000_ffff_fffe_8080_8081_7f7f_7f7f_u128;
    let mut memory = ModelMemory {
        base: 0x1000,
        bytes: vec![0; 32],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    for (opcode, lane, greater) in [
        (0x74_u8, 1_usize, false),
        (0x75, 2, false),
        (0x76, 4, false),
        (0x64, 1, true),
        (0x65, 2, true),
        (0x66, 4, true),
    ] {
        for (prefix, wide) in [(0xf1_u8, false), (0xf5, true)] {
            let decoded = X86ScalarDecoder::decode(&[0xc5, prefix, opcode, 0xc2], 0x7000).unwrap();
            assert!(
                matches!(decoded.instruction, ScalarInstruction::VexBinary {
                    operation: VexOperation::Compare { comparison, lane: actual },
                    destination: 0, first: 1, second: VectorSource::Register(2), wide: actual_wide, ..
                } if usize::from(actual) == lane
                    && actual_wide == wide
                    && (comparison == VectorComparison::SignedGreater) == greater),
                "decode {opcode:#x} prefix {prefix:#x}"
            );

            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x7000,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.vectors[0] = u128::MAX;
            cpu.vector_upper[0] = u128::MAX;
            cpu.vectors[1] = low_left;
            cpu.vectors[2] = low_right;
            cpu.vector_upper[1] = high_left;
            cpu.vector_upper[2] = high_right;
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
                ExecutionExit::Continue
            );
            assert_eq!(
                cpu.vectors[0],
                packed_compare_reference(low_left, low_right, lane, greater),
                "low {opcode:#x} wide {wide}"
            );
            assert_eq!(
                cpu.vector_upper[0],
                if wide {
                    packed_compare_reference(high_left, high_right, lane, greater)
                } else {
                    0
                },
                "upper {opcode:#x} wide {wide}"
            );
        }

        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x7100,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[3] = 0x1000;
        cpu.vectors[1] = low_left;
        cpu.vector_upper[1] = high_left;
        memory.bytes[..16].copy_from_slice(&low_right.to_le_bytes());
        memory.bytes[16..].copy_from_slice(&high_right.to_le_bytes());
        let decoded = X86ScalarDecoder::decode(&[0xc5, 0xf5, opcode, 0x03], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::Continue
        );
        assert_eq!(
            cpu.vectors[0],
            packed_compare_reference(low_left, low_right, lane, greater),
            "memory low {opcode:#x}"
        );
        assert_eq!(
            cpu.vector_upper[0],
            packed_compare_reference(high_left, high_right, lane, greater),
            "memory upper {opcode:#x}"
        );
    }
}

/// The 0x0f38 qword compares, whose VEX forms share the same decode path.
#[test]
fn vex_qword_compare_family() {
    let left = 0x8000_0000_0000_0000_0000_0000_0000_0001_u128;
    let right = 0x8000_0000_0000_0001_0000_0000_0000_0001_u128;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    for (opcode, greater) in [(0x29_u8, false), (0x37, true)] {
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x71, opcode, 0xc2], 0x7200).unwrap();
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x7200,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[1] = left;
        cpu.vectors[2] = right;
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::Continue
        );
        assert_eq!(
            cpu.vectors[0],
            packed_compare_reference(left, right, 8, greater),
            "qword {opcode:#x}"
        );
    }
}

/// `vmovntdq`/`vmovntps`/`vmovntpd`: memory-only stores that write the whole
/// destination and, being stores, leave the source register untouched.
#[test]
fn vex_non_temporal_store_family() {
    for opcode in [0xe7_u8, 0x2b] {
        for (prefix, wide, bytes) in [(0xf9_u8, false, 16_usize), (0xfd, true, 32)] {
            let decoded = X86ScalarDecoder::decode(&[0xc5, prefix, opcode, 0x03], 0x7300).unwrap();
            assert!(
                matches!(decoded.instruction, ScalarInstruction::VexVectorTransport {
                    vector: 0, operand: VectorSource::Memory(_), store: true, wide: actual
                } if actual == wide),
                "decode {opcode:#x} prefix {prefix:#x}"
            );

            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x7300,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.registers[3] = 0x1000;
            cpu.vectors[0] = 0x0f1e_2d3c_4b5a_6978_8796_a5b4_c3d2_e1f0;
            cpu.vector_upper[0] = 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00;
            let original = cpu.clone();
            let mut memory = ModelMemory {
                base: 0x1000,
                bytes: vec![0xcc; 48],
                fail_read: false,
                fail_write: false,
                commits: 0,
            };
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
                ExecutionExit::Continue
            );
            assert_eq!(cpu.vectors[0], original.vectors[0]);
            assert_eq!(cpu.vector_upper[0], original.vector_upper[0]);
            let mut expected = vec![0xcc_u8; 48];
            expected[..16].copy_from_slice(&original.vectors[0].to_le_bytes());
            if wide {
                expected[16..32].copy_from_slice(&original.vector_upper[0].to_le_bytes());
            }
            assert_eq!(memory.bytes, expected, "store {opcode:#x} bytes {bytes}");

            // A non-temporal store must fault as a whole rather than partially commit.
            let mut cpu = original.clone();
            let mut faulting = ModelMemory {
                base: 0x1000,
                bytes: vec![0; 48],
                fail_read: false,
                fail_write: true,
                commits: 0,
            };
            assert!(matches!(
                ScalarInterpreter::execute(&mut cpu, &mut faulting, decoded),
                ExecutionExit::OperandFault(access) if access.length() as usize == bytes
            ));
            assert_eq!(cpu, original);
        }

        // The register form of a non-temporal store does not exist.
        assert!(X86ScalarDecoder::decode(&[0xc5, 0xf9, opcode, 0xc3], 0x7400).is_err());
    }
}

/// `vpshufhw`/`vpshuflw` against the legacy `pshufhw`/`pshuflw` they inherit, per
/// 128-bit lane, plus the untouched half and the 256-bit form.
#[test]
fn vex_word_shuffle_family() {
    let low = 0x000f_100e_200d_300c_400b_500a_6009_7008_u128;
    let high = 0x8817_9916_aa15_bb14_cc13_dd12_ee11_ff10_u128;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    for (legacy, prefix, wide) in [
        (0xf3_u8, 0xfa_u8, false),
        (0xf3, 0xfe, true),
        (0xf2, 0xfb, false),
        (0xf2, 0xff, true),
    ] {
        for immediate in [0x00_u8, 0x1b, 0xe4, 0xff, 0x93] {
            let mut legacy_cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x7000,
                    ..Default::default()
                },
                ..Default::default()
            };
            legacy_cpu.vectors[1] = low;
            let decoded = X86ScalarDecoder::decode(&[legacy, 0x0f, 0x70, 0xc1, immediate], legacy_cpu.rip).unwrap();
            assert_eq!(
                ScalarInterpreter::execute(&mut legacy_cpu, &mut memory, decoded),
                ExecutionExit::Continue
            );

            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x7000,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.vectors[1] = low;
            cpu.vector_upper[1] = high;
            cpu.vector_upper[0] = u128::MAX;
            let decoded = X86ScalarDecoder::decode(&[0xc5, prefix, 0x70, 0xc1, immediate], cpu.rip).unwrap();
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
                ExecutionExit::Continue
            );
            assert_eq!(cpu.vectors[0], legacy_cpu.vectors[0], "low {prefix:#x} {immediate:#x}");

            let mut upper_cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x7000,
                    ..Default::default()
                },
                ..Default::default()
            };
            upper_cpu.vectors[1] = high;
            let decoded = X86ScalarDecoder::decode(&[legacy, 0x0f, 0x70, 0xc1, immediate], upper_cpu.rip).unwrap();
            assert_eq!(
                ScalarInterpreter::execute(&mut upper_cpu, &mut memory, decoded),
                ExecutionExit::Continue
            );
            assert_eq!(
                cpu.vector_upper[0],
                if wide { upper_cpu.vectors[0] } else { 0 },
                "upper {prefix:#x} {immediate:#x}"
            );
        }
    }
}

/// `vucomiss`/`vucomisd`/`vcomiss`/`vcomisd` must set exactly the flags and the
/// exceptions their legacy encodings do.
#[test]
fn vex_scalar_ordered_compare_family() {
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let singles = [1.0_f32, 2.0, -0.0, 0.0, f32::NAN, f32::INFINITY];
    for opcode in [0x2e_u8, 0x2f] {
        for (legacy, prefix, double) in [(None, 0xf8_u8, false), (Some(0x66_u8), 0xf9, true)] {
            for left in singles {
                for right in singles {
                    let (a, b) = if double {
                        (
                            u128::from(f64::from(left).to_bits()),
                            u128::from(f64::from(right).to_bits()),
                        )
                    } else {
                        (u128::from(left.to_bits()), u128::from(right.to_bits()))
                    };
                    let mut legacy_bytes = Vec::new();
                    legacy_bytes.extend(legacy);
                    legacy_bytes.extend_from_slice(&[0x0f, opcode, 0xc1]);
                    let mut legacy_cpu = CpuState {
                        scalar: ScalarState {
                            rip: 0x7000,
                            ..Default::default()
                        },
                        mxcsr: 0x1f80,
                        ..Default::default()
                    };
                    legacy_cpu.vectors[0] = a;
                    legacy_cpu.vectors[1] = b;
                    let decoded = X86ScalarDecoder::decode(&legacy_bytes, legacy_cpu.rip).unwrap();
                    assert_eq!(
                        ScalarInterpreter::execute(&mut legacy_cpu, &mut memory, decoded),
                        ExecutionExit::Continue
                    );

                    let mut cpu = CpuState {
                        scalar: ScalarState {
                            rip: 0x7000,
                            ..Default::default()
                        },
                        mxcsr: 0x1f80,
                        ..Default::default()
                    };
                    cpu.vectors[0] = a;
                    cpu.vectors[1] = b;
                    let decoded = X86ScalarDecoder::decode(&[0xc5, prefix, opcode, 0xc1], cpu.rip).unwrap();
                    assert_eq!(
                        ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
                        ExecutionExit::Continue
                    );
                    assert_eq!(cpu.flags, legacy_cpu.flags, "{opcode:#x} {left} {right} flags");
                    assert_eq!(cpu.mxcsr, legacy_cpu.mxcsr, "{opcode:#x} {left} {right} mxcsr");
                }
            }
        }
    }
}

/// `vpalignr` against the legacy `palignr` it inherits, at every interesting count and
/// independently in each 128-bit lane.
#[test]
fn vex_align_family() {
    let first = [
        0x0102_0304_0506_0708_090a_0b0c_0d0e_0f10_u128,
        0x1112_1314_1516_1718_191a_1b1c_1d1e_1f20,
    ];
    let second = [
        0x2122_2324_2526_2728_292a_2b2c_2d2e_2f30_u128,
        0x3132_3334_3536_3738_393a_3b3c_3d3e_3f40,
    ];
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    let legacy = |count: u8, high: usize, memory: &mut ModelMemory| {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x7000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[0] = first[high];
        cpu.vectors[1] = second[high];
        let decoded = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x3a, 0x0f, 0xc1, count], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, memory, decoded),
            ExecutionExit::Continue
        );
        cpu.vectors[0]
    };
    for count in [0_u8, 1, 7, 8, 15, 16, 17, 31, 32, 33, 255] {
        for (prefix, wide) in [(0x71_u8, false), (0x75, true)] {
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x7000,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.vectors[1] = first[0];
            cpu.vector_upper[1] = first[1];
            cpu.vectors[2] = second[0];
            cpu.vector_upper[2] = second[1];
            let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe3, prefix, 0x0f, 0xc2, count], cpu.rip).unwrap();
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
                ExecutionExit::Continue
            );
            assert_eq!(cpu.vectors[0], legacy(count, 0, &mut memory), "low count {count}");
            assert_eq!(
                cpu.vector_upper[0],
                if wide { legacy(count, 1, &mut memory) } else { 0 },
                "upper count {count}"
            );
        }
    }
}

/// `vpmovsx`/`vpmovzx` at all six width pairs against the legacy `pmovsx`/`pmovzx`, with
/// the 256-bit form checked against the same routine applied to the upper source half.
#[test]
fn vex_widen_family() {
    let source = 0x8090_a0b0_c0d0_e0f0_0110_2130_4150_6170_u128;
    let mut memory = ModelMemory {
        base: 0,
        bytes: vec![],
        fail_read: false,
        fail_write: false,
        commits: 0,
    };
    for opcode in [
        0x20_u8, 0x21, 0x22, 0x23, 0x24, 0x25, 0x30, 0x31, 0x32, 0x33, 0x34, 0x35,
    ] {
        let (from, to) = match opcode & 0x0f {
            0 => (1_u32, 2_u32),
            1 => (1, 4),
            2 => (1, 8),
            3 => (2, 4),
            4 => (2, 8),
            _ => (4, 8),
        };
        let legacy = |value: u128, memory: &mut ModelMemory| {
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x7000,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.vectors[1] = value;
            let decoded = X86ScalarDecoder::decode(&[0x66, 0x0f, 0x38, opcode, 0xc1], cpu.rip).unwrap();
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, memory, decoded),
                ExecutionExit::Continue
            );
            cpu.vectors[0]
        };
        for (prefix, wide) in [(0x79_u8, false), (0x7d, true)] {
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x7000,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.vectors[1] = source;
            let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, prefix, opcode, 0xc1], cpu.rip).unwrap();
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
                ExecutionExit::Continue
            );
            assert_eq!(cpu.vectors[0], legacy(source, &mut memory), "low {opcode:#x}");
            assert_eq!(
                cpu.vector_upper[0],
                if wide {
                    legacy(source >> (128 * from / to), &mut memory)
                } else {
                    0
                },
                "upper {opcode:#x}"
            );
        }

        // The memory operand is only as wide as the source elements, not the destination.
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x7000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[3] = 0x1000;
        let bytes = (32 * from / to) as usize;
        let mut narrow = ModelMemory {
            base: 0x1000,
            bytes: vec![0x5a; bytes],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe2, 0x7d, opcode, 0x03], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut narrow, decoded),
            ExecutionExit::Continue,
            "memory {opcode:#x} reads {bytes} bytes"
        );
    }
}

/// `vpextrb/w/d/q`, `vextractps` and the `0f c5` form of `vpextrw`, each against the
/// legacy encoding it inherits, in both the register and the memory destination form.
#[test]
fn vex_lane_extract_family() {
    let value = 0x8090_a0b0_c0d0_e0f0_0110_2130_4150_6170_u128;
    for (opcode, vex_prefix, bytes) in [
        (0x14_u8, 0x79_u8, 1_u8),
        (0x15, 0x79, 2),
        (0x16, 0x79, 4),
        (0x16, 0xf9, 8),
        (0x17, 0x79, 4),
    ] {
        for lane in 0..16 / bytes {
            let mut legacy_cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x7000,
                    ..Default::default()
                },
                ..Default::default()
            };
            legacy_cpu.vectors[0] = value;
            let rex: &[u8] = if bytes == 8 { &[0x48] } else { &[] };
            let mut legacy_bytes = vec![0x66_u8];
            legacy_bytes.extend_from_slice(rex);
            legacy_bytes.extend_from_slice(&[0x0f, 0x3a, opcode, 0xc1, lane]);
            let decoded = X86ScalarDecoder::decode(&legacy_bytes, legacy_cpu.rip).unwrap();
            let mut memory = ModelMemory {
                base: 0,
                bytes: vec![],
                fail_read: false,
                fail_write: false,
                commits: 0,
            };
            assert_eq!(
                ScalarInterpreter::execute(&mut legacy_cpu, &mut memory, decoded),
                ExecutionExit::Continue
            );

            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x7000,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.vectors[0] = value;
            let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe3, vex_prefix, opcode, 0xc1, lane], cpu.rip).unwrap();
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
                ExecutionExit::Continue
            );
            assert_eq!(
                cpu.registers[1], legacy_cpu.registers[1],
                "{opcode:#x} bytes {bytes} lane {lane}"
            );

            // Memory destination, sized to exactly the architectural write.
            let mut store = ModelMemory {
                base: 0x1000,
                bytes: vec![0xcc; usize::from(bytes)],
                fail_read: false,
                fail_write: false,
                commits: 0,
            };
            let mut cpu = CpuState {
                scalar: ScalarState {
                    rip: 0x7000,
                    ..Default::default()
                },
                ..Default::default()
            };
            cpu.registers[3] = 0x1000;
            cpu.vectors[0] = value;
            let decoded = X86ScalarDecoder::decode(&[0xc4, 0xe3, vex_prefix, opcode, 0x03, lane], cpu.rip).unwrap();
            assert_eq!(
                ScalarInterpreter::execute(&mut cpu, &mut store, decoded),
                ExecutionExit::Continue
            );
            assert_eq!(
                store.bytes,
                legacy_cpu.registers[1].to_le_bytes()[..usize::from(bytes)],
                "{opcode:#x} memory bytes {bytes} lane {lane}"
            );
        }
    }

    for lane in 0..8_u8 {
        let mut legacy_cpu = CpuState {
            scalar: ScalarState {
                rip: 0x7000,
                ..Default::default()
            },
            ..Default::default()
        };
        legacy_cpu.vectors[1] = value;
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![],
            fail_read: false,
            fail_write: false,
            commits: 0,
        };
        let decoded = X86ScalarDecoder::decode(&[0x66, 0x0f, 0xc5, 0xc1, lane], legacy_cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut legacy_cpu, &mut memory, decoded),
            ExecutionExit::Continue
        );

        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x7000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.vectors[1] = value;
        let decoded = X86ScalarDecoder::decode(&[0xc5, 0xf9, 0xc5, 0xc1, lane], cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::Continue
        );
        assert_eq!(cpu.registers[0], legacy_cpu.registers[0], "vpextrw lane {lane}");
    }
}

/// The scalar staging scratch is what integer instructions copy in and out per step,
/// so it must stay a small fraction of the full architectural state.
#[test]
fn scalar_state_is_a_small_fraction_of_cpu_state() {
    let scalar = size_of::<ScalarState>();
    let full = size_of::<CpuState>();
    assert!(scalar <= 160, "scalar staging scratch grew to {scalar} bytes");
    assert!(
        scalar * 4 < full,
        "scalar half {scalar} is no longer small next to {full}"
    );
}

/// The in-place vector staging path has no rollback surface, so every fault must
/// leave the architectural state exactly as it was.
#[test]
fn in_place_vector_staging_rolls_back_on_faults() {
    let sequences: &[(&[u8], bool)] = &[
        (&[0x0f, 0x10, 0x0b], false),       // movups xmm1, [rbx]
        (&[0x0f, 0x11, 0x0b], true),        // movups [rbx], xmm1
        (&[0x66, 0x0f, 0x6e, 0x03], false), // movd xmm0, [rbx]
        (&[0x66, 0x0f, 0x7e, 0x03], true),  // movd [rbx], xmm0
        (&[0x66, 0x0f, 0x60, 0x03], false), // punpcklbw xmm0, [rbx]
        (&[0x66, 0x0f, 0xef, 0x03], false), // pxor xmm0, [rbx]
    ];
    for (bytes, store) in sequences {
        let mut cpu = CpuState {
            scalar: ScalarState {
                rip: 0x8000,
                ..Default::default()
            },
            ..Default::default()
        };
        cpu.registers[3] = 0x100;
        cpu.vectors[0] = 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210;
        cpu.vectors[1] = 0xdead_beef_cafe_f00d_0f0f_0f0f_0f0f_0f0f;
        let mut memory = ModelMemory {
            base: 0,
            bytes: vec![0x5a; 0x200],
            fail_read: !store,
            fail_write: *store,
            commits: 0,
        };
        let decoded = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        let original = cpu.clone();
        assert!(
            matches!(
                ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
                ExecutionExit::OperandFault(_)
            ),
            "{bytes:x?} did not fault"
        );
        assert_eq!(cpu, original, "{bytes:x?} mutated state on a fault");
        assert_eq!(memory.commits, 0, "{bytes:x?} committed a write on a fault");

        memory.fail_read = false;
        memory.fail_write = false;
        let decoded = X86ScalarDecoder::decode(bytes, cpu.rip).unwrap();
        assert_eq!(
            ScalarInterpreter::execute(&mut cpu, &mut memory, decoded),
            ExecutionExit::Continue,
            "{bytes:x?} did not retire"
        );
        assert_eq!(cpu.rip, 0x8000 + bytes.len() as u64, "{bytes:x?} rip");
    }
}
