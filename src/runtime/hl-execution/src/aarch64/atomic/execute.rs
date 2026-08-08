use crate::{
    Aarch64CpuState, Aarch64ExecutionExit, Aarch64Instruction, Aarch64Ir, AccessKind, AtomicOperation, AtomicValue,
    ExclusiveMemory, MemoryWidth,
};

pub(crate) struct Executor;

impl Executor {
    pub(crate) fn handles(instruction: &Aarch64Instruction) -> bool {
        matches!(
            *instruction,
            Aarch64Instruction::ExclusiveLoad { .. }
                | Aarch64Instruction::OrderedAccess { .. }
                | Aarch64Instruction::ExclusiveStore { .. }
                | Aarch64Instruction::AtomicCompareExchange { .. }
                | Aarch64Instruction::AtomicUpdate { .. }
                | Aarch64Instruction::ClearExclusive
        )
    }

    pub(crate) fn execute<E: ExclusiveMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut E,
        ir: &Aarch64Ir,
    ) -> Aarch64ExecutionExit {
        match ir.instruction {
            Aarch64Instruction::OrderedAccess {
                load,
                base,
                transfer,
                width,
                order,
            } => Self::ordered(cpu, memory, load, base, transfer, width, order),
            Aarch64Instruction::ExclusiveLoad {
                base,
                first,
                second,
                width,
                order,
            } => Self::load_exclusive(cpu, memory, base, (first, second), width, order),
            Aarch64Instruction::ExclusiveStore {
                base,
                status,
                first,
                second,
                width,
                order,
            } => Self::store_exclusive(cpu, memory, base, status, (first, second), width, order),
            Aarch64Instruction::AtomicCompareExchange {
                base,
                expected,
                replacement,
                width,
                pair,
                order,
            } => Self::compare_exchange(cpu, memory, base, expected, replacement, width, pair, order),
            Aarch64Instruction::AtomicUpdate {
                base,
                source,
                destination,
                width,
                operation,
                order,
            } => Self::update(cpu, memory, base, (source, destination), width, operation, order),
            Aarch64Instruction::ClearExclusive => {
                if let Some(reservation) = cpu.exclusive.take() {
                    memory.discard_exclusive(reservation);
                }
                Self::advance(cpu)
            }
            _ => Aarch64ExecutionExit::UnsupportedInstruction {
                instruction: cpu.pc,
                word: ir.word,
            },
        }
    }

    fn ordered<E: ExclusiveMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut E,
        load: bool,
        base: u8,
        transfer: u8,
        width: MemoryWidth,
        order: crate::MemoryOrder,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let address = cpu.register_or_sp(base);
        if address & (u64::from(width.bytes()) - 1) != 0 {
            return Self::alignment(
                instruction,
                address,
                if load { AccessKind::Read } else { AccessKind::Write },
            );
        }
        if load {
            let Ok(value) = memory.load_ordered(address, width.bytes(), order) else {
                return Self::fault(instruction, address, AccessKind::Read, width.bytes());
            };
            let mut staged = cpu.clone();
            Self::write_width(&mut staged, transfer, width, value);
            staged.pc = instruction.wrapping_add(4);
            cpu.commit_scalar(&staged);
        } else if memory
            .store_ordered(address, width.bytes(), cpu.register(transfer), order)
            .is_err()
        {
            return Self::fault(instruction, address, AccessKind::Write, width.bytes());
        } else {
            cpu.pc = instruction.wrapping_add(4);
        }
        Aarch64ExecutionExit::Continue
    }

    fn load_exclusive<E: ExclusiveMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut E,
        base: u8,
        registers: (u8, Option<u8>),
        width: MemoryWidth,
        order: crate::MemoryOrder,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let address = cpu.register_or_sp(base);
        if address & (u64::from(width.bytes()) - 1) != 0 {
            return Self::alignment(instruction, address, AccessKind::Read);
        }
        let expected_bytes = width.bytes() * if registers.1.is_some() { 2 } else { 1 };
        let Ok(loaded) = memory.load_exclusive(address, width.bytes(), registers.1.is_some(), order) else {
            return Self::fault(instruction, address, AccessKind::Read, expected_bytes);
        };
        if loaded.reservation.address() != address
            || loaded.reservation.bytes() != expected_bytes
            || loaded.reservation.pair() != registers.1.is_some()
        {
            return Self::fault(instruction, address, AccessKind::Read, expected_bytes);
        }
        let mut staged = cpu.clone();
        Self::write_width(&mut staged, registers.0, width, loaded.value.low);
        if let Some(second) = registers.1 {
            Self::write_width(&mut staged, second, width, loaded.value.high);
        }
        staged.exclusive = Some(loaded.reservation);
        staged.pc = instruction.wrapping_add(4);
        cpu.commit_scalar(&staged);
        Aarch64ExecutionExit::Continue
    }

    fn store_exclusive<E: ExclusiveMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut E,
        base: u8,
        status: u8,
        registers: (u8, Option<u8>),
        width: MemoryWidth,
        order: crate::MemoryOrder,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let address = cpu.register_or_sp(base);
        if address & (u64::from(width.bytes()) - 1) != 0 {
            return Self::alignment(instruction, address, AccessKind::Write);
        }
        let Some(reservation) = cpu.exclusive else {
            let mut staged = cpu.clone();
            staged.set_narrow_register(status, 1);
            staged.exclusive = None;
            staged.pc = instruction.wrapping_add(4);
            cpu.commit_scalar(&staged);
            return Aarch64ExecutionExit::Continue;
        };
        let pair = registers.1.is_some();
        if reservation.address() != address
            || reservation.element_bytes() != width.bytes()
            || reservation.pair() != pair
        {
            memory.discard_exclusive(reservation);
            let mut staged = cpu.clone();
            staged.set_narrow_register(status, 1);
            staged.exclusive = None;
            staged.pc = instruction.wrapping_add(4);
            cpu.commit_scalar(&staged);
            return Aarch64ExecutionExit::Continue;
        }
        let replacement = AtomicValue {
            low: cpu.register(registers.0),
            high: registers.1.map_or(0, |register| cpu.register(register)),
        };
        let result = memory.store_exclusive(reservation, replacement, order);
        cpu.exclusive = None;
        let Ok(success) = result else {
            return Self::fault(instruction, address, AccessKind::Write, reservation.bytes());
        };
        let mut staged = cpu.clone();
        staged.set_narrow_register(status, u32::from(!success));
        staged.pc = instruction.wrapping_add(4);
        cpu.commit_scalar(&staged);
        Aarch64ExecutionExit::Continue
    }

    fn compare_exchange<E: ExclusiveMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut E,
        base: u8,
        expected: u8,
        replacement: u8,
        width: MemoryWidth,
        pair: bool,
        order: crate::MemoryOrder,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let address = cpu.register_or_sp(base);
        let total = u64::from(width.bytes()) * if pair { 2 } else { 1 };
        if address & (total - 1) != 0 {
            return Self::alignment(instruction, address, AccessKind::Write);
        }
        let expected_value = AtomicValue {
            low: cpu.register(expected),
            high: if pair { cpu.register(expected + 1) } else { 0 },
        };
        let replacement_value = AtomicValue {
            low: cpu.register(replacement),
            high: if pair { cpu.register(replacement + 1) } else { 0 },
        };
        let Ok(observed) =
            memory.compare_exchange(address, width.bytes(), pair, expected_value, replacement_value, order)
        else {
            return Self::fault(instruction, address, AccessKind::Write, total);
        };
        let mut staged = cpu.clone();
        Self::write_width(&mut staged, expected, width, observed.low);
        if pair {
            Self::write_width(&mut staged, expected + 1, width, observed.high);
        }
        staged.pc = instruction.wrapping_add(4);
        cpu.commit_scalar(&staged);
        Aarch64ExecutionExit::Continue
    }

    fn update<E: ExclusiveMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut E,
        base: u8,
        registers: (u8, u8),
        width: MemoryWidth,
        operation: AtomicOperation,
        order: crate::MemoryOrder,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let address = cpu.register_or_sp(base);
        if address & (u64::from(width.bytes()) - 1) != 0 {
            return Self::alignment(instruction, address, AccessKind::Write);
        }
        let Ok(old) = memory.fetch_update(address, width.bytes(), operation, cpu.register(registers.0), order) else {
            return Self::fault(instruction, address, AccessKind::Write, width.bytes());
        };
        let mut staged = cpu.clone();
        Self::write_width(&mut staged, registers.1, width, old);
        staged.pc = instruction.wrapping_add(4);
        cpu.commit_scalar(&staged);
        Aarch64ExecutionExit::Continue
    }

    fn write_width(cpu: &mut Aarch64CpuState, register: u8, width: MemoryWidth, value: u64) {
        if width == MemoryWidth::Double {
            cpu.set_register(register, value);
        } else {
            cpu.set_narrow_register(register, value as u32);
        }
    }

    fn advance(cpu: &mut Aarch64CpuState) -> Aarch64ExecutionExit {
        cpu.pc = cpu.pc.wrapping_add(4);
        Aarch64ExecutionExit::Continue
    }

    fn alignment(instruction: u64, address: u64, access: AccessKind) -> Aarch64ExecutionExit {
        Aarch64ExecutionExit::AlignmentFault {
            instruction,
            target: address,
            access,
        }
    }

    fn fault(instruction: u64, address: u64, access: AccessKind, length: impl Into<u64>) -> Aarch64ExecutionExit {
        Aarch64ExecutionExit::OperandFault(crate::FaultAccess::operand(instruction, address, access, length.into()))
    }
}
