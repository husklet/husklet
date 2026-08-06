use std::collections::HashMap;

use crate::{
    Aarch64CpuState, Aarch64Decoder, Aarch64ExecutionExit, Aarch64Instruction, Aarch64Interpreter, AccessKind,
    AtomicOperation, AtomicValue, BarrierKind, ExclusiveLoad, ExclusiveMemory, ExclusiveReservation, FaultAccess,
    GuestSystemPort, MappingGeneration, MemoryFault, MemoryOrder, MemoryWidth,
};
#[derive(Default)]
pub(super) struct Memory {
    bytes: HashMap<u64, u8>,
    epochs: HashMap<(u64, u8), u64>,
    fault: Option<u64>,
    pub(super) orders: Vec<MemoryOrder>,
    discarded: Vec<ExclusiveReservation>,
}
impl Memory {
    pub(super) fn fault_at(&mut self, address: Option<u64>) {
        self.fault = address;
    }
    pub(super) fn read(&self, address: u64, bytes: u8) -> u64 {
        let mut value = 0;
        for offset in 0..bytes {
            value |= u64::from(
                self.bytes
                    .get(&address.wrapping_add(u64::from(offset)))
                    .copied()
                    .unwrap_or(0),
            ) << (offset * 8);
        }
        value
    }
    pub(super) fn write(&mut self, address: u64, bytes: u8, value: u64) {
        for offset in 0..bytes {
            self.bytes
                .insert(address.wrapping_add(u64::from(offset)), (value >> (offset * 8)) as u8);
        }
        self.invalidate(address, bytes);
    }
    fn invalidate(&mut self, address: u64, bytes: u8) {
        for ((reserved, reserved_bytes), epoch) in &mut self.epochs {
            if Self::overlap(address, bytes, *reserved, *reserved_bytes) {
                *epoch = epoch.wrapping_add(1);
            }
        }
    }
    fn replace_mapping(&mut self) {
        for epoch in self.epochs.values_mut() {
            *epoch = epoch.wrapping_add(1);
        }
    }

    fn overlap(left: u64, left_bytes: u8, right: u64, right_bytes: u8) -> bool {
        (0..left_bytes).any(|left_offset| {
            (0..right_bytes).any(|right_offset| {
                left.wrapping_add(u64::from(left_offset)) == right.wrapping_add(u64::from(right_offset))
            })
        })
    }

    fn fails(&self, address: u64, bytes: u8) -> bool {
        self.fault
            .is_some_and(|fault| (0..bytes).any(|offset| address.wrapping_add(u64::from(offset)) == fault))
    }

    fn mask(bytes: u8) -> u64 {
        if bytes == 8 {
            u64::MAX
        } else {
            (1_u64 << (bytes * 8)) - 1
        }
    }

    fn updated(old: u64, operand: u64, bytes: u8, operation: AtomicOperation) -> u64 {
        let mask = Self::mask(bytes);
        let old = old & mask;
        let operand = operand & mask;
        match operation {
            AtomicOperation::Swap => operand,
            AtomicOperation::Add => old.wrapping_add(operand) & mask,
            AtomicOperation::Clear => old & !operand & mask,
            AtomicOperation::ExclusiveOr => old ^ operand,
            AtomicOperation::Set => old | operand,
            AtomicOperation::SignedMaximum | AtomicOperation::SignedMinimum => {
                let shift = 64 - bytes * 8;
                let old_signed = ((old << shift) as i64) >> shift;
                let operand_signed = ((operand << shift) as i64) >> shift;
                let choose_operand = if operation == AtomicOperation::SignedMaximum {
                    operand_signed > old_signed
                } else {
                    operand_signed < old_signed
                };
                if choose_operand { operand } else { old }
            }
            AtomicOperation::UnsignedMaximum => old.max(operand),
            AtomicOperation::UnsignedMinimum => old.min(operand),
        }
    }
}

impl ExclusiveMemory for Memory {
    fn load_ordered(&mut self, address: u64, bytes: u8, order: MemoryOrder) -> Result<u64, ()> {
        self.orders.push(order);
        if self.fails(address, bytes) {
            Err(())
        } else {
            Ok(self.read(address, bytes))
        }
    }

    fn store_ordered(&mut self, address: u64, bytes: u8, value: u64, order: MemoryOrder) -> Result<(), ()> {
        self.orders.push(order);
        if self.fails(address, bytes) {
            Err(())
        } else {
            self.write(address, bytes, value);
            Ok(())
        }
    }

    fn load_exclusive(
        &mut self,
        address: u64,
        element_bytes: u8,
        pair: bool,
        order: MemoryOrder,
    ) -> Result<ExclusiveLoad, ()> {
        self.orders.push(order);
        let total = element_bytes * if pair { 2 } else { 1 };
        if self.fails(address, total) {
            return Err(());
        }
        let epoch = *self.epochs.entry((address, total)).or_default();
        Ok(ExclusiveLoad {
            value: AtomicValue {
                low: self.read(address, element_bytes),
                high: if pair {
                    self.read(address.wrapping_add(u64::from(element_bytes)), element_bytes)
                } else {
                    0
                },
            },
            reservation: ExclusiveReservation::new(address, element_bytes, pair, MappingGeneration::new(epoch)),
        })
    }

    fn store_exclusive(
        &mut self,
        reservation: ExclusiveReservation,
        replacement: AtomicValue,
        order: MemoryOrder,
    ) -> Result<bool, ()> {
        self.orders.push(order);
        if self.fails(reservation.address(), reservation.bytes()) {
            return Err(());
        }
        let current = *self
            .epochs
            .entry((reservation.address(), reservation.bytes()))
            .or_default();
        if current != reservation.generation().value() {
            return Ok(false);
        }
        self.write(reservation.address(), reservation.element_bytes(), replacement.low);
        if reservation.pair() {
            self.write(
                reservation
                    .address()
                    .wrapping_add(u64::from(reservation.element_bytes())),
                reservation.element_bytes(),
                replacement.high,
            );
        }
        Ok(true)
    }

    fn discard_exclusive(&mut self, reservation: ExclusiveReservation) {
        self.discarded.push(reservation);
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
        self.orders.push(order);
        let total = element_bytes * if pair { 2 } else { 1 };
        if self.fails(address, total) {
            return Err(());
        }
        let observed = AtomicValue {
            low: self.read(address, element_bytes),
            high: if pair {
                self.read(address.wrapping_add(u64::from(element_bytes)), element_bytes)
            } else {
                0
            },
        };
        let mask = Self::mask(element_bytes);
        let matches =
            observed.low & mask == expected.low & mask && (!pair || observed.high & mask == expected.high & mask);
        if matches {
            self.write(address, element_bytes, replacement.low);
            if pair {
                self.write(
                    address.wrapping_add(u64::from(element_bytes)),
                    element_bytes,
                    replacement.high,
                );
            }
        }
        Ok(observed)
    }

    fn fetch_update(
        &mut self,
        address: u64,
        bytes: u8,
        operation: AtomicOperation,
        operand: u64,
        order: MemoryOrder,
    ) -> Result<u64, ()> {
        self.orders.push(order);
        if self.fails(address, bytes) {
            return Err(());
        }
        let old = self.read(address, bytes);
        self.write(address, bytes, Self::updated(old, operand, bytes, operation));
        Ok(old)
    }
}

#[derive(Default)]
pub(super) struct System;

impl GuestSystemPort for System {
    fn barrier(&mut self, _kind: BarrierKind, _option: u8) {}

    fn counter_frequency(&self) -> u64 {
        0
    }

    fn counter_value(&self) -> u64 {
        0
    }
}

pub(super) trait ExecuteWord {
    fn execute_word(&mut self, memory: &mut Memory, system: &mut System, word: u32) -> Aarch64ExecutionExit;
}

impl ExecuteWord for Aarch64CpuState {
    fn execute_word(&mut self, memory: &mut Memory, system: &mut System, word: u32) -> Aarch64ExecutionExit {
        Aarch64Interpreter::execute_concurrent(self, memory, system, word)
    }
}

#[test]
fn decoder_matches_all() {
    let words = [
        0x885f_7c20,
        0xc85f_fc62,
        0x8804_7cc5,
        0xc807_fd28,
        0x887f_2d8a,
        0xc87f_be0e,
        0x8831_4e92,
        0xc835_df16,
        0x88a0_7c41,
        0xc8e3_fca4,
        0x0826_7d48,
        0x486c_fe0e,
        0x38b1_8272,
        0x78f4_02d5,
        0xf877_1338,
        0xb8ba_239b,
        0xb820_3041,
        0xf8e3_40a4,
        0x3826_5107,
        0x7829_616a,
        0xb86c_71cd,
        0xd503_3f5f,
    ];
    for word in words {
        assert!(Aarch64Decoder::decode(word).is_ok(), "{word:#010x}");
    }
}

#[test]
fn decoder_retains_acquire() {
    assert!(matches!(
        Aarch64Decoder::decode(0xc8e3_fca4).unwrap().instruction,
        Aarch64Instruction::AtomicCompareExchange {
            width: MemoryWidth::Double,
            pair: false,
            order: MemoryOrder::AcquireRelease,
            ..
        }
    ));
    assert!(matches!(
        Aarch64Decoder::decode(0x78f4_02d5).unwrap().instruction,
        Aarch64Instruction::AtomicUpdate {
            width: MemoryWidth::Half,
            operation: AtomicOperation::Add,
            order: MemoryOrder::AcquireRelease,
            ..
        }
    ));
    assert!(matches!(
        Aarch64Decoder::decode(0xc87f_be0e).unwrap().instruction,
        Aarch64Instruction::ExclusiveLoad {
            second: Some(15),
            order: MemoryOrder::Acquire,
            ..
        }
    ));
}

#[test]
fn exclusive_interference_range() {
    let mut memory = Memory::default();
    let mut system = System;
    let mut first = Aarch64CpuState {
        pc: 0x1000,
        ..Default::default()
    };
    first.set_register(1, 0x8000);
    memory.write(0x8000, 4, 7);
    assert_eq!(
        first.execute_word(&mut memory, &mut system, 0x885f_7c20),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(first.register(0), 7);

    memory.write(0x9000, 4, 99);
    first.set_register(5, 8);
    first.set_register(6, 0x8000);
    assert_eq!(
        first.execute_word(&mut memory, &mut system, 0x8804_7cc5),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(first.register(4), 0);
    assert_eq!(memory.read(0x8000, 4), 8);

    first.pc = 0x1100;
    first.set_register(1, 0x8000);
    first.execute_word(&mut memory, &mut system, 0x885f_7c20);
    memory.write(0x8002, 1, 0xaa);
    first.set_register(5, 10);
    first.set_register(6, 0x8000);
    first.execute_word(&mut memory, &mut system, 0x8804_7cc5);
    assert_eq!(first.register(4), 1);

    first.pc = 0x1200;
    first.execute_word(&mut memory, &mut system, 0x885f_7c20);
    first.execute_word(&mut memory, &mut system, 0xd503_3f5f);
    first.execute_word(&mut memory, &mut system, 0x8804_7cc5);
    assert_eq!(first.register(4), 1);
}

#[test]
fn exclusive_address_mismatch() {
    let mut memory = Memory::default();
    let mut system = System;
    let mut cpu = Aarch64CpuState {
        pc: 0x1800,
        ..Default::default()
    };
    memory.write(0x8000, 4, 7);
    memory.write(0x9000, 4, 9);
    cpu.set_register(1, 0x8000);
    cpu.execute_word(&mut memory, &mut system, 0x885f_7c20);
    cpu.set_register(5, 11);
    cpu.set_register(6, 0x9000);

    assert_eq!(
        cpu.execute_word(&mut memory, &mut system, 0x8804_7cc5),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(4), 1);
    assert_eq!(cpu.exclusive, None);
    assert_eq!(memory.read(0x8000, 4), 7);
    assert_eq!(memory.read(0x9000, 4), 9);
    assert_eq!(memory.discarded.len(), 1);
}

#[test]
fn exclusive_width_mismatch() {
    let mut memory = Memory::default();
    let mut system = System;
    let mut cpu = Aarch64CpuState {
        pc: 0x1900,
        ..Default::default()
    };
    memory.write(0x8000, 8, 7);
    cpu.set_register(1, 0x8000);
    cpu.execute_word(&mut memory, &mut system, 0x885f_7c20);
    cpu.set_register(8, 11);
    cpu.set_register(9, 0x8000);

    assert_eq!(
        cpu.execute_word(&mut memory, &mut system, 0xc807_fd28),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(7), 1);
    assert_eq!(cpu.exclusive, None);
    assert_eq!(memory.read(0x8000, 8), 7);
    assert_eq!(memory.discarded.len(), 1);
}

#[test]
fn retry_requires_load() {
    let mut memory = Memory::default();
    let mut system = System;
    let mut cpu = Aarch64CpuState {
        pc: 0x1a00,
        ..Default::default()
    };
    memory.write(0x8000, 4, 7);
    cpu.set_register(1, 0x8000);
    cpu.execute_word(&mut memory, &mut system, 0x885f_7c20);
    memory.write(0x8000, 4, 8);
    cpu.set_register(5, 9);
    cpu.set_register(6, 0x8000);

    cpu.execute_word(&mut memory, &mut system, 0x8804_7cc5);
    assert_eq!(cpu.register(4), 1);
    assert_eq!(cpu.exclusive, None);
    assert_eq!(memory.read(0x8000, 4), 8);

    cpu.pc = 0x1b00;
    cpu.execute_word(&mut memory, &mut system, 0x8804_7cc5);
    assert_eq!(cpu.register(4), 1);
    assert_eq!(memory.read(0x8000, 4), 8);

    cpu.pc = 0x1c00;
    cpu.set_register(1, 0x8000);
    cpu.execute_word(&mut memory, &mut system, 0x885f_7c20);
    cpu.set_register(5, 10);
    cpu.execute_word(&mut memory, &mut system, 0x8804_7cc5);
    assert_eq!(cpu.register(4), 0);
    assert_eq!(cpu.exclusive, None);
    assert_eq!(memory.read(0x8000, 4), 10);
}

#[test]
fn clear_exclusive_discards() {
    let mut memory = Memory::default();
    let mut system = System;
    let mut cpu = Aarch64CpuState {
        pc: 0x1d00,
        ..Default::default()
    };
    memory.write(0x8000, 4, 7);
    cpu.set_register(1, 0x8000);
    cpu.execute_word(&mut memory, &mut system, 0x885f_7c20);

    cpu.execute_word(&mut memory, &mut system, 0xd503_3f5f);
    assert_eq!(cpu.exclusive, None);
    assert_eq!(memory.discarded.len(), 1);
}

#[test]
fn stale_mapping_fork() {
    let mut memory = Memory::default();
    let mut system = System;
    let mut cpu = Aarch64CpuState {
        pc: 0x2000,
        ..Default::default()
    };
    cpu.set_register(1, 0xa000);
    memory.write(0xa000, 4, 1);
    cpu.execute_word(&mut memory, &mut system, 0x885f_7c20);
    memory.replace_mapping();
    cpu.set_register(5, 2);
    cpu.set_register(6, 0xa000);
    cpu.execute_word(&mut memory, &mut system, 0x8804_7cc5);
    assert_eq!(cpu.register(4), 1);
    assert_eq!(memory.read(0xa000, 4), 1);

    cpu.pc = 0x2100;
    cpu.set_register(1, 0xa000);
    cpu.execute_word(&mut memory, &mut system, 0x885f_7c20);
    let mut child = cpu.clone();
    child.clear_exclusive_reservation();
    child.set_register(5, 3);
    child.set_register(6, 0xa000);
    child.execute_word(&mut memory, &mut system, 0x8804_7cc5);
    assert_eq!(child.register(4), 1);
    assert_eq!(memory.read(0xa000, 4), 1);
}

#[test]
fn pair_exclusives_and() {
    let mut memory = Memory::default();
    let mut system = System;
    let mut cpu = Aarch64CpuState {
        pc: 0x3000,
        ..Default::default()
    };
    cpu.set_register(12, 0xb000);
    memory.write(0xb000, 4, 0xaaaa_aaaa);
    memory.write(0xb004, 4, 0xbbbb_bbbb);
    assert_eq!(
        cpu.execute_word(&mut memory, &mut system, 0x887f_2d8a),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!((cpu.register(10), cpu.register(11)), (0xaaaa_aaaa, 0xbbbb_bbbb));

    cpu.set_register(18, 1);
    cpu.set_register(19, 2);
    cpu.set_register(20, 0xb000);
    cpu.execute_word(&mut memory, &mut system, 0x8831_4e92);
    assert_eq!(cpu.register(17), 0);
    assert_eq!((memory.read(0xb000, 4), memory.read(0xb004, 4)), (1, 2));

    cpu.pc = 0x3100;
    cpu.set_register(6, 1);
    cpu.set_register(7, 2);
    cpu.set_register(8, 3);
    cpu.set_register(9, 4);
    cpu.set_register(10, 0xb000);
    cpu.execute_word(&mut memory, &mut system, 0x0826_7d48);
    assert_eq!((cpu.register(6), cpu.register(7)), (1, 2));
    assert_eq!((memory.read(0xb000, 4), memory.read(0xb004, 4)), (3, 4));

    cpu.pc = 0x3200;
    cpu.set_register(10, 0xb004);
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_word(&mut memory, &mut system, 0x0826_7d48),
        Aarch64ExecutionExit::AlignmentFault {
            instruction: 0x3200,
            target: 0xb004,
            access: AccessKind::Write,
        }
    );
    assert_eq!(cpu, before);
}

#[test]
fn lse_contention_model() {
    let mut memory = Memory::default();
    let mut system = System;
    memory.write(0xc000, 8, 0);
    let mut cpus = [
        Aarch64CpuState {
            pc: 0x4000,
            ..Default::default()
        },
        Aarch64CpuState {
            pc: 0x5000,
            ..Default::default()
        },
    ];
    for iteration in 0..1_000 {
        let index = iteration & 1;
        cpus[index].set_register(3, 1);
        cpus[index].set_register(5, 0xc000);
        cpus[index].execute_word(&mut memory, &mut system, 0xf8e3_00a4);
        cpus[index].pc = if index == 0 { 0x4000 } else { 0x5000 };
    }
    assert_eq!(memory.read(0xc000, 8), 1_000);
}

#[test]
fn every_lse_operation() {
    let cases = [
        (0_u32, AtomicOperation::Add, 0x10_u64, 3_u64, 0x13_u64),
        (1, AtomicOperation::Clear, 0xff, 0x0f, 0xf0),
        (2, AtomicOperation::ExclusiveOr, 0xaa, 0x0f, 0xa5),
        (3, AtomicOperation::Set, 0xa0, 0x0f, 0xaf),
        (4, AtomicOperation::SignedMaximum, 0x80, 0x7f, 0x7f),
        (5, AtomicOperation::SignedMinimum, 0x7f, 0x80, 0x80),
        (6, AtomicOperation::UnsignedMaximum, 1, 2, 2),
        (7, AtomicOperation::UnsignedMinimum, 2, 1, 1),
    ];
    for (opcode, operation, old, operand, expected) in cases {
        let mut memory = Memory::default();
        let mut system = System;
        let mut cpu = Aarch64CpuState {
            pc: 0x1000,
            ..Default::default()
        };
        memory.write(0x8000, 1, old);
        cpu.set_register(1, operand);
        cpu.set_register(2, 0x8000);
        let word = 0x3820_0000 | 1 << 16 | opcode << 12 | 2 << 5 | 3;
        assert_eq!(
            cpu.execute_word(&mut memory, &mut system, word),
            Aarch64ExecutionExit::Continue,
            "{operation:?}"
        );
        assert_eq!(cpu.register(3), old, "{operation:?}");
        assert_eq!(memory.read(0x8000, 1), expected, "{operation:?}");
    }

    let mut memory = Memory::default();
    let mut system = System;
    let mut cpu = Aarch64CpuState {
        pc: 0x2000,
        ..Default::default()
    };
    memory.write(0x9000, 8, 0x1122);
    cpu.set_register(1, 0x3344);
    cpu.set_register(2, 0x9000);
    let swap = 0xf8a1_8043;
    assert_eq!(
        cpu.execute_word(&mut memory, &mut system, swap),
        Aarch64ExecutionExit::Continue
    );
    assert_eq!(cpu.register(3), 0x1122);
    assert_eq!(memory.read(0x9000, 8), 0x3344);
}

#[test]
fn all_lse_widths() {
    for size in 0_u32..4 {
        let bytes = 1_u8 << size;
        let mut memory = Memory::default();
        let mut system = System;
        let mut cpu = Aarch64CpuState {
            pc: 0x3000,
            ..Default::default()
        };
        let address = 0xa000;
        memory.write(address, bytes, u64::MAX);
        cpu.set_register(1, 2);
        cpu.set_register(2, address);
        let word = 0x3820_0000 | size << 30 | 1 << 16 | 2 << 5 | 3;
        cpu.execute_word(&mut memory, &mut system, word);
        let mask = if bytes == 8 {
            u64::MAX
        } else {
            (1_u64 << (bytes * 8)) - 1
        };
        assert_eq!(cpu.register(3), mask);
        assert_eq!(memory.read(address, bytes), 1);
    }
}

#[test]
fn compare_exchange_returns() {
    let mut memory = Memory::default();
    let mut system = System;
    let mut cpu = Aarch64CpuState {
        pc: 0x4000,
        ..Default::default()
    };
    memory.write(0xb000, 4, 7);
    cpu.set_register(0, 7);
    cpu.set_register(1, 9);
    cpu.set_register(2, 0xb000);
    cpu.execute_word(&mut memory, &mut system, 0x88a0_7c41);
    assert_eq!(cpu.register(0), 7);
    assert_eq!(memory.read(0xb000, 4), 9);

    cpu.pc = 0x4100;
    cpu.set_register(0, 8);
    cpu.set_register(1, 10);
    cpu.execute_word(&mut memory, &mut system, 0x88a0_7c41);
    assert_eq!(cpu.register(0), 9);
    assert_eq!(memory.read(0xb000, 4), 9);
}

#[test]
fn acquire_release_encodings() {
    let mut memory = Memory::default();
    let mut system = System;
    let mut cpu = Aarch64CpuState {
        pc: 0x5000,
        ..Default::default()
    };
    memory.write(0xc000, 8, 1);
    cpu.set_register(3, 0xc000);
    cpu.execute_word(&mut memory, &mut system, 0xc85f_fc62);
    cpu.set_register(8, 2);
    cpu.set_register(9, 0xc000);
    cpu.execute_word(&mut memory, &mut system, 0xc807_fd28);
    cpu.set_register(3, 2);
    cpu.set_register(4, 3);
    cpu.set_register(5, 0xc000);
    cpu.execute_word(&mut memory, &mut system, 0xc8e3_fca4);
    cpu.set_register(20, 1);
    cpu.set_register(22, 0xc000);
    cpu.execute_word(&mut memory, &mut system, 0x78f4_02d5);
    assert_eq!(
        memory.orders,
        [
            MemoryOrder::Acquire,
            MemoryOrder::Release,
            MemoryOrder::AcquireRelease,
            MemoryOrder::AcquireRelease,
        ]
    );
}

#[test]
fn ordered_access_family() {
    let encode = |size: u32, load: bool, limited: bool, transfer: u32| {
        size << 30
            | 0x0880_0000
            | u32::from(load) << 22
            | 31 << 16
            | u32::from(!limited) << 15
            | 31 << 10
            | 1 << 5
            | transfer
    };
    let mut memory = Memory::default();
    let mut system = System;
    let mut cpu = Aarch64CpuState {
        pc: 0x5800,
        ..Default::default()
    };
    cpu.set_register(1, 0xe000);
    for size in 0_u32..4 {
        let bytes = 1_u8 << size;
        let mask = if bytes == 8 {
            u64::MAX
        } else {
            (1_u64 << (bytes * 8)) - 1
        };
        for limited in [false, true] {
            cpu.set_register(2, u64::MAX);
            assert_eq!(
                cpu.execute_word(&mut memory, &mut system, encode(size, false, limited, 2)),
                Aarch64ExecutionExit::Continue,
            );
            assert_eq!(memory.read(0xe000, bytes), mask);
            cpu.set_register(3, 0);
            assert_eq!(
                cpu.execute_word(&mut memory, &mut system, encode(size, true, limited, 3)),
                Aarch64ExecutionExit::Continue,
            );
            assert_eq!(cpu.register(3), mask);
        }
    }
    assert_eq!(memory.orders, [MemoryOrder::Release, MemoryOrder::Acquire].repeat(8));
    memory.write(0xe000, 8, 0x55);
    assert_eq!(
        cpu.execute_word(&mut memory, &mut system, encode(3, true, false, 31)),
        Aarch64ExecutionExit::Continue,
    );
    assert_eq!(cpu.register(31), 0);
    assert_eq!(
        cpu.execute_word(&mut memory, &mut system, encode(3, false, false, 31)),
        Aarch64ExecutionExit::Continue,
    );
    assert_eq!(memory.read(0xe000, 8), 0);
}

#[test]
fn ordered_access_faults() {
    let mut memory = Memory::default();
    let mut system = System;
    let mut cpu = Aarch64CpuState {
        pc: 0x5900,
        ..Default::default()
    };
    cpu.set_register(0, 0x1122);
    cpu.set_register(1, 0xe001);
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_word(&mut memory, &mut system, 0xc89f_fc20),
        Aarch64ExecutionExit::AlignmentFault {
            instruction: 0x5900,
            target: 0xe001,
            access: AccessKind::Write,
        },
    );
    assert_eq!(cpu, before);

    cpu.pc = 0x5904;
    assert_eq!(
        cpu.execute_word(&mut memory, &mut system, 0xc8df_fc3f),
        Aarch64ExecutionExit::AlignmentFault {
            instruction: 0x5904,
            target: 0xe001,
            access: AccessKind::Read,
        },
    );

    cpu.set_register(1, 0xe000);
    cpu.pc = 0x5910;
    memory.fault_at(Some(0xe000));
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_word(&mut memory, &mut system, 0xc8df_fc3f),
        Aarch64ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x5910,
                    address: 0xe000,
                    access: AccessKind::Read,
                },
                8
            )
            .expect("fault width is nonzero")
        ),
    );
    assert_eq!(cpu, before);
    for word in [0xc880_fc20, 0xc89f_0020] {
        assert_eq!(Aarch64Decoder::decode(word), Err(crate::Aarch64DecodeError::Reserved));
    }
}

#[test]
fn faults_and() {
    let mut memory = Memory::default();
    let mut system = System;
    let mut cpu = Aarch64CpuState {
        pc: 0x6000,
        ..Default::default()
    };
    memory.write(0xd000, 8, 1);
    cpu.set_register(3, 2);
    cpu.set_register(4, 3);
    cpu.set_register(5, 0xd000);
    memory.fault_at(Some(0xd000));
    let before = cpu.clone();
    assert_eq!(
        cpu.execute_word(&mut memory, &mut system, 0xc8e3_fca4),
        Aarch64ExecutionExit::OperandFault(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x6000,
                    address: 0xd000,
                    access: AccessKind::Write,
                },
                8
            )
            .expect("fault width is nonzero")
        )
    );
    assert_eq!(cpu, before);
    assert_eq!(memory.read(0xd000, 8), 1);

    memory.fault_at(None);
    cpu.pc = 0x6100;
    cpu.set_register(16, 0xd000);
    cpu.set_register(12, 1);
    cpu.set_register(13, 2);
    cpu.set_register(14, 3);
    cpu.set_register(15, 4);
    memory.write(0xd000, 8, 1);
    memory.write(0xd008, 8, 2);
    memory.fault_at(Some(0xd008));
    let before = cpu.clone();
    assert!(matches!(
        cpu.execute_word(&mut memory, &mut system, 0x486c_fe0e),
        Aarch64ExecutionExit::OperandFault(access) if access.length() == 16
    ));
    assert_eq!(cpu, before);
    assert_eq!((memory.read(0xd000, 8), memory.read(0xd008, 8)), (1, 2));
}
