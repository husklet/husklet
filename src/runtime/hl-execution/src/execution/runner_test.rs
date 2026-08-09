use crate::{
    Aarch64CpuState, AccessKind, AtomicOperation, AtomicValue, CpuState, EXECUTION_SNAPSHOT_VERSION, ExclusiveLoad,
    ExclusiveMemory, ExclusiveReservation, ExecutionCpuSnapshot, ExecutionFault, ExecutionInstructionMemory,
    ExecutionMachine, ExecutionSnapshot, FaultAccess, GuestOperandMemory, MappingGeneration, MemoryFault, MemoryOrder,
    ScalarState, StepOutcome,
};

struct Memory {
    bytes: Vec<u8>,
    generation: u64,
    fetches: std::cell::Cell<u64>,
    cacheable: bool,
    invalidated: Vec<u64>,
    /// Bytes a peer executor publishes while this executor is inside a block.
    peer_write: Option<(usize, [u8; 4])>,
    /// End of the executable mapping. Like the engine's `read_spans`, a fetch that would
    /// cross it yields no bytes at all rather than a short window.
    executable_end: Option<usize>,
}

impl Memory {
    fn new(size: usize) -> Self {
        Self {
            bytes: vec![0; size],
            generation: 0,
            fetches: std::cell::Cell::new(0),
            cacheable: true,
            invalidated: Vec::new(),
            peer_write: None,
            executable_end: None,
        }
    }

    fn interpreted(size: usize) -> Self {
        Self {
            cacheable: false,
            ..Self::new(size)
        }
    }

    fn put(&mut self, address: usize, bytes: &[u8]) {
        self.bytes[address..address + bytes.len()].copy_from_slice(bytes);
        self.generation = self.generation.wrapping_add(1);
    }

    fn machine(cpu: ExecutionCpuSnapshot) -> ExecutionMachine {
        ExecutionMachine::new(ExecutionSnapshot {
            version: EXECUTION_SNAPSHOT_VERSION,
            cpu,
            cache_epoch: 1,
            fault: None,
        })
        .unwrap()
    }
}

impl GuestOperandMemory for Memory {
    type Reservation = (u64, u8);
    type BatchReservation = Vec<(u64, u8)>;

    fn read(&self, address: u64, bytes: u8) -> Result<u64, ()> {
        let start = usize::try_from(address).map_err(|_| ())?;
        let end = start.checked_add(usize::from(bytes)).ok_or(())?;
        let value = self.bytes.get(start..end).ok_or(())?;
        Ok(value
            .iter()
            .enumerate()
            .fold(0, |word, (index, byte)| word | (u64::from(*byte) << (index * 8))))
    }

    fn reserve_write(&self, address: u64, bytes: u8) -> Result<Self::Reservation, ()> {
        let start = usize::try_from(address).map_err(|_| ())?;
        let end = start.checked_add(usize::from(bytes)).ok_or(())?;
        self.bytes.get(start..end).ok_or(())?;
        Ok((address, bytes))
    }

    fn commit_write(&mut self, reservation: Self::Reservation, value: u64) -> Result<(), ()> {
        let start = usize::try_from(reservation.0).map_err(|_| ())?;
        let end = start + usize::from(reservation.1);
        self.bytes[start..end].copy_from_slice(&value.to_le_bytes()[..usize::from(reservation.1)]);
        self.generation = self.generation.wrapping_add(1);
        Ok(())
    }

    fn reserve_write_batch(&self, writes: &[(u64, u8)]) -> Result<Self::BatchReservation, u64> {
        for (address, bytes) in writes {
            self.reserve_write(*address, *bytes).map_err(|()| *address)?;
        }
        Ok(writes.to_vec())
    }

    fn commit_write_batch(&mut self, reservation: Self::BatchReservation, values: &[u64]) -> Result<(), ()> {
        for (write, value) in reservation.into_iter().zip(values) {
            self.commit_write(write, *value)?;
        }
        Ok(())
    }
}

impl ExclusiveMemory for Memory {
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
        bytes: u8,
        pair: bool,
        _order: MemoryOrder,
    ) -> Result<ExclusiveLoad, ()> {
        Ok(ExclusiveLoad {
            value: AtomicValue {
                low: self.read(address, bytes)?,
                high: if pair {
                    self.read(address + u64::from(bytes), bytes)?
                } else {
                    0
                },
            },
            reservation: ExclusiveReservation::new(address, bytes, pair, MappingGeneration::new(self.generation)),
        })
    }
    fn store_exclusive(
        &mut self,
        reservation: ExclusiveReservation,
        value: AtomicValue,
        _order: MemoryOrder,
    ) -> Result<bool, ()> {
        if reservation.generation().value() != self.generation {
            return Ok(false);
        }
        self.store_ordered(
            reservation.address(),
            reservation.element_bytes(),
            value.low,
            MemoryOrder::Relaxed,
        )?;
        if reservation.pair() {
            self.store_ordered(
                reservation.address() + u64::from(reservation.element_bytes()),
                reservation.element_bytes(),
                value.high,
                MemoryOrder::Relaxed,
            )?;
        }
        Ok(true)
    }
    fn compare_exchange(
        &mut self,
        address: u64,
        bytes: u8,
        pair: bool,
        expected: AtomicValue,
        replacement: AtomicValue,
        _order: MemoryOrder,
    ) -> Result<AtomicValue, ()> {
        let observed = self.load_exclusive(address, bytes, pair, MemoryOrder::Relaxed)?.value;
        if observed == expected {
            let reservation = ExclusiveReservation::new(address, bytes, pair, MappingGeneration::new(self.generation));
            self.store_exclusive(reservation, replacement, MemoryOrder::Relaxed)?;
        }
        Ok(observed)
    }
    fn fetch_update(
        &mut self,
        address: u64,
        bytes: u8,
        operation: AtomicOperation,
        operand: u64,
        _order: MemoryOrder,
    ) -> Result<u64, ()> {
        let old = self.read(address, bytes)?;
        let value = match operation {
            AtomicOperation::Swap => operand,
            AtomicOperation::Add => old.wrapping_add(operand),
            AtomicOperation::Clear => old & !operand,
            AtomicOperation::ExclusiveOr => old ^ operand,
            AtomicOperation::Set => old | operand,
            AtomicOperation::SignedMaximum => (old as i64).max(operand as i64) as u64,
            AtomicOperation::SignedMinimum => (old as i64).min(operand as i64) as u64,
            AtomicOperation::UnsignedMaximum => old.max(operand),
            AtomicOperation::UnsignedMinimum => old.min(operand),
        };
        self.store_ordered(address, bytes, value, MemoryOrder::Relaxed)?;
        Ok(old)
    }
}

impl ExecutionInstructionMemory for Memory {
    fn fetch(&self, address: u64, bytes: &mut [u8]) -> Result<usize, ()> {
        self.fetches.set(self.fetches.get() + 1);
        let start = usize::try_from(address).map_err(|_| ())?;
        if let Some(end) = self.executable_end
            && (start >= end || start + bytes.len() > end)
        {
            return Err(());
        }
        let source = self.bytes.get(start..).ok_or(())?;
        let length = source.len().min(bytes.len());
        bytes[..length].copy_from_slice(&source[..length]);
        Ok(length)
    }

    fn instruction_epoch(&self) -> Option<crate::InstructionEpoch> {
        self.cacheable.then_some(crate::InstructionEpoch {
            incarnation: 1,
            mappings: 1,
            writes: self.generation,
        })
    }

    fn invalidate_instruction(&mut self, address: u64) {
        self.invalidated.push(address);
        if let Some((target, word)) = self.peer_write.take() {
            self.put(target, &word);
        }
    }
}

/// A signal delivery swaps the register file and nothing else, so the blocks already
/// translated for this image must survive it. `replace` is the image-changing swap and
/// must still discard them, which is what makes this test non-vacuous: the two calls
/// differ only in which method is used and they must differ in refetch count.
#[test]
fn a_context_swap_keeps_translations_that_an_image_swap_discards() {
    fn rewound(machine: &ExecutionMachine) -> ExecutionSnapshot {
        machine.freeze().unwrap();
        let mut snapshot = machine.snapshot().unwrap();
        let ExecutionCpuSnapshot::Aarch64(cpu) = &mut snapshot.cpu else {
            panic!("AArch64")
        };
        cpu.pc = 0x1000;
        cpu.registers[1] = 0;
        snapshot
    }

    let mut memory = Memory::new(0x2000);
    for (offset, word) in [0x9100_0400_u32, 0xf100_0421, 0x54ff_ffc1].into_iter().enumerate() {
        memory.put(0x1000 + offset * 4, &word.to_le_bytes());
    }
    let cpu = Aarch64CpuState {
        pc: 0x1000,
        ..Aarch64CpuState::default()
    };
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(cpu));
    assert_eq!(machine.run_slice(1, 30_000, &mut memory), StepOutcome::Yield);
    let warm = memory.fetches.get();
    assert_eq!(warm, 3);

    let snapshot = rewound(&machine);
    machine.replace_context(snapshot).unwrap();
    machine.thaw().unwrap();
    assert_eq!(machine.run_slice(1, 30_000, &mut memory), StepOutcome::Yield);
    assert_eq!(
        memory.fetches.get(),
        warm,
        "a context swap must not discard translations"
    );

    let snapshot = rewound(&machine);
    machine.replace(snapshot).unwrap();
    machine.thaw().unwrap();
    assert_eq!(machine.run_slice(1, 30_000, &mut memory), StepOutcome::Yield);
    assert!(
        memory.fetches.get() > warm,
        "an image swap must still discard translations"
    );
}

/// A replacement carrying a different cache epoch is a new image, and keeping the
/// translated blocks across it would run stale code, so the context swap refuses it.
#[test]
fn a_context_swap_refuses_a_new_cache_epoch() {
    let cpu = Aarch64CpuState::default();
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(cpu));
    machine.freeze().unwrap();
    let mut snapshot = machine.snapshot().unwrap();
    snapshot.cache_epoch += 1;
    assert!(machine.replace_context(snapshot.clone()).is_err());
    snapshot.cache_epoch -= 1;
    assert!(machine.replace_context(snapshot).is_ok());
    machine.thaw().unwrap();
}

#[test]
fn blocks_retain_decode() {
    let mut memory = Memory::new(0x2000);
    // add x0,x0,#1; subs x1,x1,#1; b.ne loop
    for (offset, word) in [0x9100_0400_u32, 0xf100_0421, 0x54ff_ffc1].into_iter().enumerate() {
        memory.put(0x1000 + offset * 4, &word.to_le_bytes());
    }
    let mut cpu = Aarch64CpuState {
        pc: 0x1000,
        ..Aarch64CpuState::default()
    };
    cpu.registers[1] = 10_000;
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(cpu));
    let generation = memory.generation;
    assert_eq!(machine.run_slice(1, 30_000, &mut memory), StepOutcome::Yield);
    assert_eq!(memory.fetches.get(), 3);
    assert_eq!(memory.generation, generation);
    machine.freeze().unwrap();
    let snapshot = machine.snapshot().unwrap();
    let ExecutionCpuSnapshot::Aarch64(cpu) = snapshot.cpu else {
        panic!("AArch64")
    };
    assert_eq!(cpu.registers[0], 10_000);
    assert_eq!(cpu.registers[1], 0);
    assert_eq!(cpu.pc, 0x100c);
}

/// Two publications keep `AArch64` self-modifying code correct and mask each other in the guest
/// corpus: the store path publishes the written executable range, and a guest `ic ivau` publishes
/// the named line, which is the only signal when the bytes were rewritten from another address
/// space. These tests pin each so that removing either is visible on its own.
#[test]
fn a_published_write_epoch_discards_translations_without_cache_maintenance() {
    let mut memory = Memory::new(0x2000);
    // add x0,x0,#1; add x0,x0,#1; b .
    for (offset, word) in [0x9100_0400_u32, 0x9100_0400, 0x17ff_fffe].into_iter().enumerate() {
        memory.put(0x1000 + offset * 4, &word.to_le_bytes());
    }
    let cpu = Aarch64CpuState {
        pc: 0x1000,
        ..Aarch64CpuState::default()
    };
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(cpu));
    assert_eq!(machine.run_slice(1, 3, &mut memory), StepOutcome::Yield);
    // add x0,x0,#4, published only by the write epoch: the guest issues no `ic ivau`.
    memory.put(0x1000, &0x9100_1000_u32.to_le_bytes());
    assert_eq!(machine.run_slice(1, 3, &mut memory), StepOutcome::Yield);
    machine.freeze().unwrap();
    let ExecutionCpuSnapshot::Aarch64(cpu) = machine.snapshot().unwrap().cpu else {
        panic!("AArch64")
    };
    assert_eq!(cpu.registers[0], 7, "the second pass must run the published bytes");
}

#[test]
fn guest_instruction_cache_maintenance_reaches_the_memory_port() {
    let mut memory = Memory::new(0x2000);
    memory.put(0x1000, &0xd50b_7520_u32.to_le_bytes()); // ic ivau, x0
    let mut cpu = Aarch64CpuState {
        pc: 0x1000,
        ..Aarch64CpuState::default()
    };
    cpu.registers[0] = 0x1400;
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(cpu));
    assert_eq!(machine.run_slice(1, 1, &mut memory), StepOutcome::Yield);
    assert_eq!(
        memory.invalidated,
        vec![0x1400],
        "maintenance must publish to peer executors, not only this cache"
    );
}

/// A peer executor's publication that lands after this executor's loop-top epoch check must not
/// be swallowed by the `ic ivau` line discard, which adopts the epoch it samples afterwards.
#[test]
fn a_line_discard_must_not_adopt_a_peer_publication_it_did_not_apply() {
    let mut memory = Memory::new(0x3000);
    memory.put(0x1000, &0xd50b_7520_u32.to_le_bytes()); // ic ivau, x0
    memory.put(0x1004, &0x1400_03ff_u32.to_le_bytes()); // b 0x2000
    memory.put(0x2000, &0x9100_0400_u32.to_le_bytes()); // add x0,x0,#1
    memory.put(0x2004, &0x17ff_fbff_u32.to_le_bytes()); // b 0x1000
    // The peer rewrites the already-translated block at 0x2000 while the `ic ivau` block runs.
    memory.peer_write = Some((0x2000, 0x9100_4000_u32.to_le_bytes())); // add x0,x0,#16
    let mut cpu = Aarch64CpuState {
        pc: 0x2000,
        ..Aarch64CpuState::default()
    };
    cpu.registers[0] = 0;
    cpu.registers[1] = 0x1400;
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(cpu));
    assert_eq!(machine.run_slice(1, 5, &mut memory), StepOutcome::Yield);
    machine.freeze().unwrap();
    let ExecutionCpuSnapshot::Aarch64(cpu) = machine.snapshot().unwrap().cpu else {
        panic!("AArch64")
    };
    assert_eq!(
        cpu.registers[0], 17,
        "the second pass must run the peer's published bytes"
    );
}

#[test]
fn cached_block_respects_instruction_budget() {
    let mut memory = Memory::new(0x2000);
    for (offset, word) in [0x9100_0400_u32, 0x9100_0400, 0x17ff_fffe].into_iter().enumerate() {
        memory.put(0x1000 + offset * 4, &word.to_le_bytes());
    }
    let cpu = Aarch64CpuState {
        pc: 0x1000,
        ..Aarch64CpuState::default()
    };
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(cpu));
    assert_eq!(machine.run_slice(1, 1, &mut memory), StepOutcome::Yield);
    machine.freeze().unwrap();
    let ExecutionCpuSnapshot::Aarch64(cpu) = machine.snapshot().unwrap().cpu else {
        panic!("AArch64")
    };
    assert_eq!(cpu.pc, 0x1004);
    assert_eq!(cpu.registers[0], 1);
}

#[test]
fn cached_memory_fault_preserves_faulting_state() {
    let mut memory = Memory::new(0x3000);
    for (offset, word) in [0x9100_0400_u32, 0xf940_0022, 0xd400_0001].into_iter().enumerate() {
        memory.put(0x1000 + offset * 4, &word.to_le_bytes());
    }
    let mut cpu = Aarch64CpuState {
        pc: 0x1000,
        ..Aarch64CpuState::default()
    };
    cpu.registers[1] = 0x4000;
    cpu.registers[2] = 0xfeed_face;
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(cpu));
    assert_eq!(
        machine.run_slice(1, 8, &mut memory),
        StepOutcome::Fault(ExecutionFault::Operand(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x1004,
                    address: 0x4000,
                    access: AccessKind::Read,
                },
                8,
            )
            .unwrap(),
        ))
    );
    machine.freeze().unwrap();
    let ExecutionCpuSnapshot::Aarch64(cpu) = machine.snapshot().unwrap().cpu else {
        panic!("AArch64")
    };
    assert_eq!(cpu.pc, 0x1004);
    assert_eq!(cpu.registers[0], 1);
    assert_eq!(cpu.registers[2], 0xfeed_face);
}

#[test]
fn x86_partial_instruction_state_survives_fault_snapshot_and_retry() {
    let mut memory = Memory::new(0x3000);
    memory.put(0x1000, &[0xf3, 0xa4, 0x0f, 0x05]);
    memory.put(0x2000, &[0x11, 0x22, 0x33, 0x44]);
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1000,
            ..Default::default()
        },
        ..CpuState::default()
    };
    cpu.registers[1] = 4;
    cpu.registers[6] = 0x2000;
    cpu.registers[7] = 0x2ffe;
    let machine = Memory::machine(ExecutionCpuSnapshot::X86_64(cpu));

    assert_eq!(
        machine.run_slice(1, 16, &mut memory),
        StepOutcome::Fault(ExecutionFault::Operand(
            FaultAccess::new(
                MemoryFault {
                    instruction: 0x1000,
                    address: 0x3000,
                    access: AccessKind::Write
                },
                1,
            )
            .unwrap(),
        ))
    );
    machine.freeze().unwrap();
    let ExecutionCpuSnapshot::X86_64(cpu) = machine.snapshot().unwrap().cpu else {
        panic!("x86-64");
    };
    assert_eq!(
        (cpu.rip, cpu.registers[1], cpu.registers[6], cpu.registers[7]),
        (0x1000, 2, 0x2002, 0x3000)
    );
    assert_eq!(&memory.bytes[0x2ffe..0x3000], &[0x11, 0x22]);
    machine.thaw().unwrap();

    memory.bytes.resize(0x4000, 0);
    memory.put(0x3000, &[0, 0]);
    assert_eq!(
        machine.run_slice(1, 16, &mut memory),
        StepOutcome::Syscall {
            instruction: 0x1002,
            next: 0x1004
        }
    );
    machine.freeze().unwrap();
    let ExecutionCpuSnapshot::X86_64(cpu) = machine.snapshot().unwrap().cpu else {
        panic!("x86-64");
    };
    assert_eq!(
        (cpu.rip, cpu.registers[1], cpu.registers[6], cpu.registers[7]),
        (0x1004, 0, 0x2004, 0x3002)
    );
    assert_eq!(&memory.bytes[0x2ffe..0x3002], &[0x11, 0x22, 0x33, 0x44]);
}

#[test]
fn blocks_observe_epoch() {
    let mut memory = Memory::new(0x2000);
    for (offset, word) in [0x9100_0400_u32, 0x17ff_ffff].into_iter().enumerate() {
        memory.put(0x1000 + offset * 4, &word.to_le_bytes());
    }
    let cpu = Aarch64CpuState {
        pc: 0x1000,
        ..Aarch64CpuState::default()
    };
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(cpu));
    assert_eq!(machine.run_slice(1, 100, &mut memory), StepOutcome::Yield);
    assert_eq!(memory.fetches.get(), 2);
    memory.put(0x1000, &0x9100_0800_u32.to_le_bytes());
    assert_eq!(machine.run_slice(1, 100, &mut memory), StepOutcome::Yield);
    assert_eq!(memory.fetches.get(), 4);
    machine.freeze().unwrap();
    let snapshot = machine.snapshot().unwrap();
    let ExecutionCpuSnapshot::Aarch64(cpu) = snapshot.cpu else {
        panic!("AArch64")
    };
    assert_eq!(cpu.registers[0], 150);
}

#[test]
fn blocks_resynchronize() {
    let mut memory = Memory::new(0x3000);
    for (address, word) in [
        (0x1000, 0xb900_0020_u32),
        (0x1004, 0x1400_03ff),
        (0x2000, 0x9100_0442),
        (0x2004, 0xd400_0001),
    ] {
        memory.put(address, &word.to_le_bytes());
    }
    let cpu = Aarch64CpuState {
        pc: 0x2000,
        ..Aarch64CpuState::default()
    };
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(cpu));
    assert_eq!(
        machine.run_slice(1, 8, &mut memory),
        StepOutcome::Syscall {
            instruction: 0x2004,
            next: 0x2008
        },
    );
    assert_eq!(
        machine.handle_syscall(1, |cpu| {
            let ExecutionCpuSnapshot::Aarch64(cpu) = cpu else {
                panic!("AArch64")
            };
            assert_eq!(cpu.registers[2], 1);
            cpu.pc = 0x1000;
            cpu.registers[0] = 0x9100_0842;
            cpu.registers[1] = 0x2000;
            StepOutcome::Continue
        }),
        StepOutcome::Continue
    );

    assert_eq!(
        machine.run_slice(1, 8, &mut memory),
        StepOutcome::Syscall {
            instruction: 0x2004,
            next: 0x2008
        },
    );
    assert_eq!(
        machine.handle_syscall(1, |cpu| {
            let ExecutionCpuSnapshot::Aarch64(cpu) = cpu else {
                panic!("AArch64")
            };
            assert_eq!(cpu.registers[2], 3);
            StepOutcome::Continue
        }),
        StepOutcome::Continue
    );
}

#[test]
fn aarch64_slice_executes() {
    let mut memory = Memory::new(0x3000);
    for (offset, word) in [0x9100_0400_u32, 0xf900_0020, 0xf940_0022, 0xd400_0001]
        .into_iter()
        .enumerate()
    {
        memory.put(0x1000 + offset * 4, &word.to_le_bytes());
    }
    let mut cpu = Aarch64CpuState {
        pc: 0x1000,
        ..Aarch64CpuState::default()
    };
    cpu.registers[0] = 41;
    cpu.registers[1] = 0x2000;
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(cpu));
    assert_eq!(
        machine.run_slice(1, 8, &mut memory),
        StepOutcome::Syscall {
            instruction: 0x100c,
            next: 0x1010
        },
    );
    assert_eq!(memory.read(0x2000, 8), Ok(42));
}

#[test]
fn ordered_access_executes() {
    let mut memory = Memory::new(0x3000);
    for (offset, word) in [0xc89f_fc20_u32, 0xc8df_fc22, 0xd400_0001].into_iter().enumerate() {
        memory.put(0x1000 + offset * 4, &word.to_le_bytes());
    }
    let mut cpu = Aarch64CpuState {
        pc: 0x1000,
        ..Aarch64CpuState::default()
    };
    cpu.registers[0] = 0x1234_5678_9abc_def0;
    cpu.registers[1] = 0x2000;
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(cpu));
    assert_eq!(
        machine.run_slice(1, 8, &mut memory),
        StepOutcome::Syscall {
            instruction: 0x1008,
            next: 0x100c
        },
    );
    assert_eq!(memory.read(0x2000, 8), Ok(0x1234_5678_9abc_def0));
    machine.freeze().unwrap();
    let snapshot = machine.snapshot().unwrap();
    let ExecutionCpuSnapshot::Aarch64(cpu) = snapshot.cpu else {
        panic!("expected AArch64 snapshot");
    };
    assert_eq!(cpu.registers[2], 0x1234_5678_9abc_def0);
}

#[test]
fn cached_fp_block_preserves_fpsr() {
    let mut memory = Memory::new(0x2000);
    for (offset, word) in [0x9e67_001f_u32, 0x9e66_03e2, 0xd400_0001].into_iter().enumerate() {
        memory.put(0x1000 + offset * 4, &word.to_le_bytes());
    }
    let mut cpu = Aarch64CpuState {
        pc: 0x1000,
        fpsr: 0x80,
        ..Aarch64CpuState::default()
    };
    cpu.registers[0] = 0x7ff8_1234_5678_9abc;
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(cpu));
    assert_eq!(
        machine.run_slice(1, 8, &mut memory),
        StepOutcome::Syscall {
            instruction: 0x1008,
            next: 0x100c
        },
    );
    machine.freeze().unwrap();
    let snapshot = machine.snapshot().unwrap();
    let ExecutionCpuSnapshot::Aarch64(cpu) = snapshot.cpu else {
        panic!("expected AArch64 snapshot");
    };
    assert_eq!(cpu.vectors[31], 0x7ff8_1234_5678_9abc);
    assert_eq!(cpu.registers[2], 0x7ff8_1234_5678_9abc);
    assert_eq!(cpu.fpsr, 0x80);
    assert_eq!(cpu.pc, 0x100c);
    assert_eq!(memory.fetches.get(), 3);
}

#[test]
fn x86_slice_executes() {
    let mut memory = Memory::new(0x3000);
    memory.put(
        0x1000,
        &[0x48, 0x83, 0xc0, 0x01, 0x48, 0x89, 0x03, 0x48, 0x8b, 0x0b, 0x0f, 0x05],
    );
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1000,
            ..Default::default()
        },
        ..CpuState::default()
    };
    cpu.registers[0] = 41;
    cpu.registers[3] = 0x2000;
    let machine = Memory::machine(ExecutionCpuSnapshot::X86_64(cpu));
    assert_eq!(
        machine.run_slice(1, 8, &mut memory),
        StepOutcome::Syscall {
            instruction: 0x100a,
            next: 0x100c
        },
    );
    assert_eq!(memory.read(0x2000, 8), Ok(42));
}

#[test]
fn x86_slice_boundaries() {
    let mut memory = Memory::new(0x2000);
    memory.put(0x1000, &[0x90, 0xeb, 0xfd]);
    let machine = Memory::machine(ExecutionCpuSnapshot::X86_64(CpuState {
        scalar: ScalarState {
            rip: 0x1000,
            ..Default::default()
        },
        ..CpuState::default()
    }));
    assert_eq!(
        machine.run_slice(2, 4, &mut memory),
        StepOutcome::Fault(ExecutionFault::CacheEpoch)
    );
    assert_eq!(machine.run_slice(1, 4, &mut memory), StepOutcome::Yield);
    assert_eq!(
        machine.handle_syscall(1, |_| StepOutcome::Continue),
        StepOutcome::Continue,
        "the bounded slice must release state ownership"
    );
    machine.freeze().unwrap();
    assert_eq!(
        machine.run_slice(1, 4, &mut memory),
        StepOutcome::Fault(ExecutionFault::Frozen)
    );
}

#[test]
fn repeated_x86_interpreter_slices_stop_at_decoder_boundaries() {
    const FIRST: u64 = 0x1000;
    // add rax,1; dec rax; jmp FIRST. These repository-owned bytes mix four-,
    // three-, and two-byte instructions so an interior return is observable.
    const CODE: &[u8] = &[0x48, 0x83, 0xc0, 0x01, 0x48, 0xff, 0xc8, 0xeb, 0xf7];
    let mut memory = Memory::interpreted(0x2000);
    memory.put(FIRST as usize, CODE);
    let machine = Memory::machine(ExecutionCpuSnapshot::X86_64(CpuState {
        scalar: ScalarState {
            rip: FIRST,
            ..Default::default()
        },
        ..CpuState::default()
    }));
    let mut starts = Vec::new();
    let mut offset = 0;
    while offset < CODE.len() {
        starts.push(FIRST + offset as u64);
        let decoded = crate::X86ScalarDecoder::decode(&CODE[offset..], FIRST + offset as u64).unwrap();
        offset += usize::from(decoded.length);
    }

    for budget in [1, 2, 5, 3, 8, 13, 1, 21] {
        assert_eq!(machine.run_slice(1, budget, &mut memory), StepOutcome::Yield);
        machine.freeze().unwrap();
        let ExecutionCpuSnapshot::X86_64(cpu) = machine.snapshot().unwrap().cpu else {
            panic!("x86-64")
        };
        assert!(
            starts.contains(&cpu.rip),
            "budget {budget} returned interior instruction pointer {:#x}",
            cpu.rip,
        );
        machine.thaw().unwrap();
    }
}

#[test]
fn x86_blocks_follow_instruction_epoch() {
    let mut memory = Memory::new(0x2000);
    memory.put(0x1000, &[0x90, 0xeb, 0xfd]);
    let machine = Memory::machine(ExecutionCpuSnapshot::X86_64(CpuState {
        scalar: ScalarState {
            rip: 0x1000,
            ..Default::default()
        },
        ..CpuState::default()
    }));
    assert_eq!(machine.run_slice(1, 32, &mut memory), StepOutcome::Yield);
    let retained = memory.fetches.get();
    assert!(retained > 0);
    assert_eq!(machine.run_slice(1, 32, &mut memory), StepOutcome::Yield);
    assert_eq!(memory.fetches.get(), retained);

    memory.put(0x1000, &[0xcc]);
    assert!(matches!(
        machine.run_slice(1, 1, &mut memory),
        StepOutcome::Fault(ExecutionFault::Signal(crate::SynchronousTrap {
            signal: crate::TrapSignal::Breakpoint,
            instruction: 0x1000,
            ..
        }))
    ));
    assert!(memory.fetches.get() > retained);
}

#[test]
fn failed_locked_cmpxchg_reschedules() {
    let mut memory = Memory::new(0x3000);
    memory.put(0x1000, &[0xf0, 0x0f, 0xb1, 0x0b, 0x0f, 0x05]);
    memory.put(0x2000, &8_u32.to_le_bytes());
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1000,
            ..Default::default()
        },
        ..CpuState::default()
    };
    cpu.registers[0] = 2;
    cpu.registers[1] = 13;
    cpu.registers[3] = 0x2000;
    let machine = Memory::machine(ExecutionCpuSnapshot::X86_64(cpu));
    assert_eq!(machine.run_slice(1, 4096, &mut memory), StepOutcome::Yield);
    machine.freeze().unwrap();
    let ExecutionCpuSnapshot::X86_64(cpu) = machine.snapshot().unwrap().cpu else {
        panic!("expected x86-64 snapshot");
    };
    assert_eq!((cpu.rip, cpu.registers[0]), (0x1004, 8));
    assert!(!cpu.flags.contains(crate::Flag::Zero));
    assert_eq!(memory.read(0x2000, 4), Ok(8));

    memory.put(0x2000, &8_u32.to_le_bytes());
    let mut cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1000,
            ..Default::default()
        },
        ..CpuState::default()
    };
    cpu.registers[0] = 8;
    cpu.registers[1] = 13;
    cpu.registers[3] = 0x2000;
    let machine = Memory::machine(ExecutionCpuSnapshot::X86_64(cpu));
    assert_eq!(
        machine.run_slice(1, 2, &mut memory),
        StepOutcome::Syscall {
            instruction: 0x1004,
            next: 0x1006,
        }
    );
    assert_eq!(memory.read(0x2000, 4), Ok(13));
}

#[test]
fn x86_fault_signals() {
    let mut memory = Memory::new(0x2000);
    memory.put(0x1000, &[0xcc]);
    let machine = Memory::machine(ExecutionCpuSnapshot::X86_64(CpuState {
        scalar: ScalarState {
            rip: 0x1000,
            ..Default::default()
        },
        ..CpuState::default()
    }));
    assert_eq!(
        machine.run_step(1, &mut memory),
        StepOutcome::Fault(crate::ExecutionFault::Signal(crate::SynchronousTrap {
            signal: crate::TrapSignal::Breakpoint,
            code: 128,
            address: 0,
            instruction: 0x1000,
            state: crate::TrapState::Completed { next: 0x1001 },
        })),
    );
    machine.freeze().unwrap();
    let ExecutionCpuSnapshot::X86_64(cpu) = machine.snapshot().unwrap().cpu else {
        panic!()
    };
    assert_eq!(cpu.rip, 0x1001);

    memory.put(0x1000, &[0x48, 0xf7, 0xf1]);
    let machine = Memory::machine(ExecutionCpuSnapshot::X86_64(CpuState {
        scalar: ScalarState {
            rip: 0x1000,
            ..Default::default()
        },
        ..CpuState::default()
    }));
    assert_eq!(
        machine.run_step(1, &mut memory),
        StepOutcome::Fault(crate::ExecutionFault::Signal(crate::SynchronousTrap {
            signal: crate::TrapSignal::Divide,
            code: 1,
            address: 0x1000,
            instruction: 0x1000,
            state: crate::TrapState::Faulting,
        })),
    );
    machine.freeze().unwrap();
    let ExecutionCpuSnapshot::X86_64(cpu) = machine.snapshot().unwrap().cpu else {
        panic!()
    };
    assert_eq!(cpu.rip, 0x1000);
}

#[test]
fn aarch64_breakpoint_signal() {
    let mut memory = Memory::new(0x2000);
    memory.put(0x1000, &0xd420_0000_u32.to_le_bytes());
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(Aarch64CpuState {
        pc: 0x1000,
        ..Aarch64CpuState::default()
    }));
    assert_eq!(
        machine.run_step(1, &mut memory),
        StepOutcome::Fault(crate::ExecutionFault::Signal(crate::SynchronousTrap {
            signal: crate::TrapSignal::Breakpoint,
            code: 1,
            address: 0x1000,
            instruction: 0x1000,
            state: crate::TrapState::Faulting,
        })),
    );
    machine.freeze().unwrap();
    let ExecutionCpuSnapshot::Aarch64(cpu) = machine.snapshot().unwrap().cpu else {
        panic!()
    };
    assert_eq!(cpu.pc, 0x1000);
}

#[test]
fn a64_alignment_access() {
    let mut memory = Memory::new(0x2000);
    memory.put(0x1002, &0xd503_201f_u32.to_le_bytes());
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(Aarch64CpuState {
        pc: 0x1002,
        ..Aarch64CpuState::default()
    }));
    assert_eq!(
        machine.run_step(1, &mut memory),
        StepOutcome::Fault(crate::ExecutionFault::Alignment {
            instruction: 0x1002,
            address: 0x1002,
            access: crate::AccessKind::Execute,
        }),
    );
    machine.freeze().unwrap();
    let ExecutionCpuSnapshot::Aarch64(cpu) = machine.snapshot().unwrap().cpu else {
        panic!()
    };
    assert_eq!(cpu.pc, 0x1002);
}

#[test]
fn illegal_signal_metadata() {
    let mut memory = Memory::new(0x2000);
    memory.put(0x1000, &[0x0f, 0x0b]);
    let machine = Memory::machine(ExecutionCpuSnapshot::X86_64(CpuState {
        scalar: ScalarState {
            rip: 0x1000,
            ..Default::default()
        },
        ..CpuState::default()
    }));
    assert_eq!(
        machine.run_step(1, &mut memory),
        StepOutcome::Fault(crate::ExecutionFault::Signal(crate::SynchronousTrap {
            signal: crate::TrapSignal::Illegal,
            code: 2,
            address: 0x1000,
            instruction: 0x1000,
            state: crate::TrapState::Faulting,
        })),
    );

    memory.put(0x1000, &0_u32.to_le_bytes());
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(Aarch64CpuState {
        pc: 0x1000,
        ..Aarch64CpuState::default()
    }));
    assert_eq!(
        machine.run_step(1, &mut memory),
        StepOutcome::Fault(crate::ExecutionFault::Signal(crate::SynchronousTrap {
            signal: crate::TrapSignal::Illegal,
            code: 1,
            address: 0x1000,
            instruction: 0x1000,
            state: crate::TrapState::Faulting,
        })),
    );
}

#[test]
fn timestamp_counter_reads() {
    let mut memory = Memory::new(0x2000);
    memory.put(0x1000, &[0x0f, 0x31, 0x0f, 0x31]);
    let machine = Memory::machine(ExecutionCpuSnapshot::X86_64(CpuState {
        scalar: ScalarState {
            rip: 0x1000,
            ..Default::default()
        },
        ..CpuState::default()
    }));

    assert_eq!(machine.run_step(1, &mut memory), StepOutcome::Continue);
    assert_eq!(machine.run_step(1, &mut memory), StepOutcome::Continue);
    machine.freeze().unwrap();
    let snapshot = machine.snapshot().unwrap();
    let ExecutionCpuSnapshot::X86_64(cpu) = snapshot.cpu else {
        panic!()
    };
    assert_eq!((cpu.registers[2], cpu.registers[0], cpu.rip), (0, 1, 0x1004));
}

#[test]
fn timestamp_counter_auxiliary() {
    let mut memory = Memory::new(0x2000);
    memory.put(0x1000, &[0x0f, 0x01, 0xf9]);
    let machine = Memory::machine(ExecutionCpuSnapshot::X86_64(CpuState {
        scalar: ScalarState {
            registers: [u64::MAX; 16],
            rip: 0x1000,
            ..Default::default()
        },
        ..CpuState::default()
    }));

    assert_eq!(machine.run_step(1, &mut memory), StepOutcome::Continue);
    machine.freeze().unwrap();
    let ExecutionCpuSnapshot::X86_64(cpu) = machine.snapshot().unwrap().cpu else {
        panic!()
    };
    assert_eq!(
        (cpu.registers[2], cpu.registers[0], cpu.registers[1], cpu.rip),
        (0, 0, 0, 0x1003)
    );
}

#[test]
fn timestamp_counter_fork() {
    let mut memory = Memory::new(0x2000);
    memory.put(0x1000, &[0x0f, 0x31, 0x0f, 0x31]);
    let parent = Memory::machine(ExecutionCpuSnapshot::X86_64(CpuState {
        scalar: ScalarState {
            rip: 0x1000,
            ..Default::default()
        },
        ..CpuState::default()
    }));
    assert_eq!(parent.run_step(1, &mut memory), StepOutcome::Continue);
    parent.freeze().unwrap();
    let child = parent.fork_child().unwrap();
    assert_eq!(child.run_step(2, &mut memory), StepOutcome::Continue);
    child.freeze().unwrap();
    let snapshot = child.snapshot().unwrap();
    let ExecutionCpuSnapshot::X86_64(cpu) = snapshot.cpu else {
        panic!()
    };
    assert_eq!((cpu.registers[2], cpu.registers[0]), (0, 1));
}

#[test]
fn counter_fork_progress() {
    #[derive(Debug)]
    struct Counter(std::sync::atomic::AtomicU64);
    impl crate::ArchitecturalCounter for Counter {
        fn read(&self) -> u64 {
            self.0.fetch_add(100, std::sync::atomic::Ordering::Relaxed)
        }
    }

    let mut memory = Memory::new(0x2000);
    memory.put(0x1000, &0xd53b_e040_u32.to_le_bytes());
    memory.put(0x1004, &0xd53b_e041_u32.to_le_bytes());
    memory.put(0x1008, &0xd53b_e002_u32.to_le_bytes());
    let counter: std::sync::Arc<dyn crate::ArchitecturalCounter> =
        std::sync::Arc::new(Counter(std::sync::atomic::AtomicU64::new(4_000)));
    let parent = ExecutionMachine::new_with_counter(
        ExecutionSnapshot {
            version: EXECUTION_SNAPSHOT_VERSION,
            cpu: ExecutionCpuSnapshot::Aarch64(Aarch64CpuState {
                pc: 0x1000,
                ..Aarch64CpuState::default()
            }),
            cache_epoch: 1,
            fault: None,
        },
        counter,
    )
    .unwrap();

    assert_eq!(parent.run_step(1, &mut memory), StepOutcome::Continue);
    parent.freeze().unwrap();
    let child = parent.fork_child().unwrap();
    assert_eq!(child.run_step(2, &mut memory), StepOutcome::Continue);
    assert_eq!(child.run_step(2, &mut memory), StepOutcome::Continue);
    child.freeze().unwrap();
    let ExecutionCpuSnapshot::Aarch64(cpu) = child.snapshot().unwrap().cpu else {
        panic!()
    };
    assert_eq!((cpu.registers[0], cpu.registers[1]), (4_000, 4_100));
    assert_eq!(cpu.registers[2], 1_000_000_000);
}

/// The guest timebase contract: `cntfrq_el0` is [`crate::GUEST_COUNTER_FREQUENCY_HZ`]
/// and `cntvct_el0` delivers [`crate::ArchitecturalCounter`] nanoseconds unscaled, so
/// a counter difference divided by the frequency is the elapsed seconds the guest saw.
#[test]
fn guest_timebase_is_nanoseconds_against_the_declared_frequency() {
    #[derive(Debug)]
    struct Counter(std::sync::atomic::AtomicU64);
    impl crate::ArchitecturalCounter for Counter {
        fn read(&self) -> u64 {
            self.0.fetch_add(250_000_000, std::sync::atomic::Ordering::Relaxed)
        }
    }

    let mut memory = Memory::new(0x2000);
    memory.put(0x1000, &0xd53b_e040_u32.to_le_bytes()); // mrs x0, cntvct_el0
    memory.put(0x1004, &0xd53b_e041_u32.to_le_bytes()); // mrs x1, cntvct_el0
    memory.put(0x1008, &0xd53b_e002_u32.to_le_bytes()); // mrs x2, cntfrq_el0
    let counter: std::sync::Arc<dyn crate::ArchitecturalCounter> =
        std::sync::Arc::new(Counter(std::sync::atomic::AtomicU64::new(1_000_000_000)));
    let machine = ExecutionMachine::new_with_counter(
        ExecutionSnapshot {
            version: EXECUTION_SNAPSHOT_VERSION,
            cpu: ExecutionCpuSnapshot::Aarch64(Aarch64CpuState {
                pc: 0x1000,
                ..Aarch64CpuState::default()
            }),
            cache_epoch: 1,
            fault: None,
        },
        counter,
    )
    .unwrap();
    for _ in 0..3 {
        assert_eq!(machine.run_step(1, &mut memory), StepOutcome::Continue);
    }
    machine.freeze().unwrap();
    let ExecutionCpuSnapshot::Aarch64(cpu) = machine.snapshot().unwrap().cpu else {
        panic!()
    };
    assert_eq!(cpu.registers[2], crate::GUEST_COUNTER_FREQUENCY_HZ);
    assert_eq!(cpu.registers[1] - cpu.registers[0], 250_000_000);
    let observed_us = (cpu.registers[1] - cpu.registers[0]) * 1_000_000 / cpu.registers[2];
    assert_eq!(observed_us, 250_000);
}

#[test]
fn slice_yield_fetch() {
    let mut memory = Memory::new(0x1001);
    memory.put(0x1000, &[0x90]);
    let machine = Memory::machine(ExecutionCpuSnapshot::X86_64(CpuState {
        scalar: ScalarState {
            rip: 0x1000,
            ..Default::default()
        },
        ..CpuState::default()
    }));
    assert_eq!(machine.run_slice(1, 1, &mut memory), StepOutcome::Yield);
    assert!(matches!(
        machine.run_step(2, &mut memory),
        StepOutcome::Fault(crate::ExecutionFault::CacheEpoch),
    ));
    assert!(matches!(
        machine.run_step(1, &mut memory),
        StepOutcome::Fault(crate::ExecutionFault::Fetch(_)),
    ));
}

/// A store that patches a later instruction of its own block must not let the block's
/// already-decoded tail run. Extending blocks past stores is only sound because the
/// epoch is rechecked after each one; deleting that recheck returns 1 here.
#[test]
fn a_store_into_its_own_block_is_observed_before_the_decoded_tail_runs() {
    let mut memory = Memory::new(0x2000);
    for (offset, word) in [
        0xb900_0001_u32, // str w1, [x0]
        0xd503_201f,     // nop
        0x5280_0020,     // mov w0, #1  <- patched to mov w0, #2 by the store above
        0xd65f_03c0,     // ret
    ]
    .into_iter()
    .enumerate()
    {
        memory.put(0x1000 + offset * 4, &word.to_le_bytes());
    }
    let mut cpu = Aarch64CpuState {
        pc: 0x1000,
        ..Aarch64CpuState::default()
    };
    cpu.registers[0] = 0x1008;
    cpu.registers[1] = 0x5280_0040;
    let machine = Memory::machine(ExecutionCpuSnapshot::Aarch64(cpu));

    assert_eq!(machine.run_slice(1, 3, &mut memory), StepOutcome::Yield);

    machine.freeze().unwrap();
    let ExecutionCpuSnapshot::Aarch64(final_cpu) = machine.snapshot().unwrap().cpu else {
        panic!("AArch64")
    };
    assert_eq!(final_cpu.registers[0], 2);
}

/// An x86 instruction ending flush with the last byte of an executable mapping runs, because the
/// fetch asks only for the bytes up to the page edge; one that really reaches past the edge faults.
/// The aarch64 arm is right by construction — its fetch is exactly the 4 bytes of one instruction.
#[test]
fn instructions_run_at_the_end_of_an_executable_mapping() {
    let mut memory = Memory::new(0x3000);
    memory.executable_end = Some(0x2000);
    memory.put(0x1ff9, &[0xb8, 0x2a, 0x00, 0x00, 0x00, 0x0f, 0x05]);
    let machine = Memory::machine(ExecutionCpuSnapshot::X86_64(CpuState {
        scalar: ScalarState {
            rip: 0x1ff9,
            ..Default::default()
        },
        ..CpuState::default()
    }));
    assert_eq!(
        machine.run_slice(1, 16, &mut memory),
        StepOutcome::Syscall {
            instruction: 0x1ffe,
            next: 0x2000
        }
    );
    assert_eq!(
        machine.handle_syscall(1, |cpu| {
            let ExecutionCpuSnapshot::X86_64(cpu) = cpu else {
                panic!("x86-64")
            };
            assert_eq!(cpu.registers[0], 42);
            StepOutcome::Yield
        }),
        StepOutcome::Yield
    );

    memory.put(0x1ffd, &[0xb8, 0x2a, 0x00]);
    let straddling = Memory::machine(ExecutionCpuSnapshot::X86_64(CpuState {
        scalar: ScalarState {
            rip: 0x1ffd,
            ..Default::default()
        },
        ..CpuState::default()
    }));
    assert_eq!(
        straddling.run_slice(1, 16, &mut memory),
        StepOutcome::Fault(ExecutionFault::Fetch(MemoryFault {
            instruction: 0x1ffd,
            address: 0x1ffd,
            access: AccessKind::Execute,
        }))
    );

    memory.put(0x1ffc, &0xd400_0001_u32.to_le_bytes());
    let aarch64 = Memory::machine(ExecutionCpuSnapshot::Aarch64(Aarch64CpuState {
        pc: 0x1ffc,
        ..Aarch64CpuState::default()
    }));
    assert_eq!(
        aarch64.run_slice(1, 16, &mut memory),
        StepOutcome::Syscall {
            instruction: 0x1ffc,
            next: 0x2000
        }
    );
}

/// The x86 slice must publish its interpreter tally, as the aarch64 slice does. `run_x86_slice`
/// shipped with no tally at all, so `hl-interp: instructions=` read 0 on every amd64 run and the
/// native-coverage ratio built from it was unmeasurable. A lower bound: concurrent tests can only
/// add to these globals, and running this case alone fails deterministically without the tally.
#[test]
fn an_x86_slice_publishes_its_interpreted_instruction_tally() {
    const BUDGET: u64 = 30_000;
    let mut memory = Memory::new(0x2000);
    // inc rax; jmp .-5 -- a tight loop that retires exactly `BUDGET` instructions.
    memory.put(0x1000, &[0x48, 0xff, 0xc0, 0xeb, 0xfb]);
    let cpu = CpuState {
        scalar: ScalarState {
            rip: 0x1000,
            ..Default::default()
        },
        ..CpuState::default()
    };
    let machine = Memory::machine(ExecutionCpuSnapshot::X86_64(cpu));
    let before = crate::INTERPRETED_INSTRUCTIONS.load(std::sync::atomic::Ordering::Relaxed);
    assert_eq!(machine.run_slice(1, BUDGET, &mut memory), StepOutcome::Yield);
    let after = crate::INTERPRETED_INSTRUCTIONS.load(std::sync::atomic::Ordering::Relaxed);
    assert!(
        after - before >= BUDGET,
        "x86 slice retired {BUDGET} instructions but published {} to INTERPRETED_INSTRUCTIONS",
        after - before,
    );
}
