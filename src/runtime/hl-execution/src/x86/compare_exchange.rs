use crate::{
    AccessKind, AluOperation, Arithmetic, AtomicOperation, AtomicValue, CpuState, EffectiveAddress, ExclusiveMemory,
    ExecutionExit, Flag, FlagState, GuestOperandMemory, IntegerWidth, MemoryOrder, ScalarOperand, ScalarRegister,
    ScalarWidth, Staged,
};

pub(crate) struct CompareExchange;

impl CompareExchange {
    pub(crate) fn wide<M: GuestOperandMemory + ExclusiveMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        wide: bool,
        locked: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let bytes = if wide { 8 } else { 4 };
        let total = bytes * 2;
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        if let Err(exit) = Self::validate(address, total, instruction, AccessKind::Write) {
            return exit;
        }
        let mask = if wide { u64::MAX } else { 0xffff_ffff };
        let expected = AtomicValue {
            low: cpu.registers[0] & mask,
            high: cpu.registers[2] & mask,
        };
        let replacement = AtomicValue {
            low: cpu.registers[3] & mask,
            high: cpu.registers[1] & mask,
        };
        let observed = if wide || locked {
            match memory.compare_exchange(
                address,
                bytes,
                true,
                expected,
                replacement,
                MemoryOrder::SequentiallyConsistent,
            ) {
                Ok(value) => value,
                Err(()) => return Self::wide_fault(instruction, address, total, AccessKind::Write),
            }
        } else {
            let writes = [(address, bytes), (address + u64::from(bytes), bytes)];
            let reservation = match memory.reserve_write_batch(&writes) {
                Ok(value) => value,
                Err(fault) => return Self::wide_fault(instruction, fault, bytes, AccessKind::Write),
            };
            let low = match memory.read(address, bytes) {
                Ok(value) => value & mask,
                Err(()) => return Self::wide_fault(instruction, address, bytes, AccessKind::Read),
            };
            let high = match memory.read(address + u64::from(bytes), bytes) {
                Ok(value) => value & mask,
                Err(()) => {
                    return Self::wide_fault(instruction, address + u64::from(bytes), bytes, AccessKind::Read);
                }
            };
            let value = AtomicValue { low, high };
            if value == expected
                && memory
                    .commit_write_batch(reservation, &[replacement.low, replacement.high])
                    .is_err()
                {
                    return Self::wide_fault(instruction, address, total, AccessKind::Write);
                }
            value
        };
        let equal = observed == expected;
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.flags = staged.flags.with(Flag::Zero, equal);
        if !equal {
            let width = if wide { ScalarWidth::Qword } else { ScalarWidth::Dword };
            staged.write_register(ScalarRegister::General(0), width, observed.low);
            staged.write_register(ScalarRegister::General(2), width, observed.high);
        }
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn wide_fault(instruction: u64, address: u64, bytes: u8, access: AccessKind) -> ExecutionExit {
        ExecutionExit::OperandFault(crate::FaultAccess::operand(
            instruction,
            address,
            access,
            u64::from(bytes),
        ))
    }

    pub(crate) fn locked_alu<M: ExclusiveMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        source: u64,
        operation: AluOperation,
        width: ScalarWidth,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let bytes = Self::byte_count(width);
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        if let Err(exit) = Self::validate(address, bytes, instruction, AccessKind::Write) {
            return exit;
        }
        let carry = u64::from(cpu.flags.contains(Flag::Carry));
        let (atomic, operand) = match operation {
            AluOperation::Add => (AtomicOperation::Add, source),
            AluOperation::Or => (AtomicOperation::Set, source),
            AluOperation::Adc => (AtomicOperation::Add, source.wrapping_add(carry)),
            AluOperation::Sbb => (AtomicOperation::Add, source.wrapping_add(carry).wrapping_neg()),
            AluOperation::And => (AtomicOperation::Clear, !source),
            AluOperation::Sub => (AtomicOperation::Add, source.wrapping_neg()),
            AluOperation::Xor => (AtomicOperation::ExclusiveOr, source),
            AluOperation::Compare | AluOperation::Test => unreachable!(),
        };
        let old = match memory.fetch_update(address, bytes, atomic, operand, MemoryOrder::SequentiallyConsistent) {
            Ok(value) => value & Self::width_mask(width),
            Err(()) => {
                return ExecutionExit::OperandFault(crate::FaultAccess::operand(
                    instruction,
                    address,
                    AccessKind::Write,
                    u64::from(bytes),
                ));
            }
        };
        let arithmetic = Self::arithmetic(operation, width, old, source, cpu.flags);
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.flags = staged.flags.apply(arithmetic.flags);
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn add<M: GuestOperandMemory>(
        staged: &mut CpuState,
        cpu: &CpuState,
        memory: &M,
        destination: ScalarOperand,
        source: ScalarRegister,
        width: ScalarWidth,
        next: u64,
        instruction: u64,
    ) -> Result<Staged<M::Reservation, M::BatchReservation>, ExecutionExit> {
        let source_value = cpu.read_register(source, width);
        let (old, address) = match destination {
            ScalarOperand::Register(register) => (cpu.read_register(register, width), None),
            ScalarOperand::Memory(effective) => {
                let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                Self::validate(address, Self::byte_count(width), instruction, AccessKind::Read)?;
                let old = memory.read(address, Self::byte_count(width)).map_err(|()| {
                    ExecutionExit::OperandFault(crate::FaultAccess::operand(
                        instruction,
                        address,
                        AccessKind::Read,
                        u64::from(Self::byte_count(width)),
                    ))
                })?;
                (old, Some(address))
            }
            ScalarOperand::Immediate(_) => unreachable!(),
        };
        let operation = Arithmetic::add(Self::integer_width(width), old, source_value, false);
        staged.flags = staged.flags.apply(operation.flags);
        staged.write_register(source, width, old);
        match (destination, address) {
            (ScalarOperand::Register(register), None) => {
                staged.write_register(register, width, operation.result);
                Ok(Staged::Cpu)
            }
            (ScalarOperand::Memory(_), Some(address)) => {
                Self::validate(address, Self::byte_count(width), instruction, AccessKind::Write)?;
                let reservation = memory.reserve_write(address, Self::byte_count(width)).map_err(|()| {
                    ExecutionExit::OperandFault(crate::FaultAccess::operand(
                        instruction,
                        address,
                        AccessKind::Write,
                        u64::from(Self::byte_count(width)),
                    ))
                })?;
                Ok(Staged::Write(
                    reservation,
                    operation.result,
                    address,
                    Self::byte_count(width),
                ))
            }
            _ => unreachable!(),
        }
    }

    pub(crate) fn locked_add<M: GuestOperandMemory + ExclusiveMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        source: ScalarRegister,
        width: ScalarWidth,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let bytes = Self::byte_count(width);
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        if let Err(exit) = Self::validate(address, bytes, instruction, AccessKind::Write) {
            return exit;
        }
        let source_value = cpu.read_register(source, width);
        let old = match memory.fetch_update(
            address,
            bytes,
            AtomicOperation::Add,
            source_value,
            MemoryOrder::SequentiallyConsistent,
        ) {
            Ok(value) => value & Self::width_mask(width),
            Err(()) => {
                return ExecutionExit::OperandFault(crate::FaultAccess::operand(
                    instruction,
                    address,
                    AccessKind::Write,
                    u64::from(bytes),
                ));
            }
        };
        let operation = Arithmetic::add(Self::integer_width(width), old, source_value, false);
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.flags = staged.flags.apply(operation.flags);
        staged.write_register(source, width, old);
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn register(
        staged: &mut CpuState,
        cpu: &CpuState,
        destination: ScalarRegister,
        source: ScalarRegister,
        width: ScalarWidth,
    ) {
        let left = cpu.read_register(destination, width);
        let right = cpu.read_register(source, width);
        staged.write_register(destination, width, right);
        staged.write_register(source, width, left);
    }

    pub(crate) fn exchange<M: GuestOperandMemory + ExclusiveMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        address: EffectiveAddress,
        source: ScalarRegister,
        width: ScalarWidth,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let bytes = Self::byte_count(width);
        let address = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let end = address.checked_add(u64::from(bytes) - 1);
        if !Self::is_canonical(address) || end.is_none_or(|value| !Self::is_canonical(value)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access: AccessKind::Write,
            };
        }
        let replacement = cpu.read_register(source, width);
        let observed = match memory.fetch_update(
            address,
            bytes,
            AtomicOperation::Swap,
            replacement,
            MemoryOrder::SequentiallyConsistent,
        ) {
            Ok(value) => value & Self::width_mask(width),
            Err(()) => {
                return ExecutionExit::OperandFault(crate::FaultAccess::operand(
                    instruction,
                    address,
                    AccessKind::Write,
                    u64::from(bytes),
                ));
            }
        };
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.write_register(source, width, observed);
        *cpu = staged;
        ExecutionExit::Continue
    }

    pub(crate) fn arithmetic(
        operation: AluOperation,
        width: ScalarWidth,
        left: u64,
        right: u64,
        flags: FlagState,
    ) -> Arithmetic {
        let width = Self::integer_width(width);
        match operation {
            AluOperation::Add => Arithmetic::add(width, left, right, false),
            AluOperation::Or => Arithmetic::logic(width, left | right),
            AluOperation::Adc => Arithmetic::add(width, left, right, flags.contains(Flag::Carry)),
            AluOperation::Sbb => Arithmetic::sub(width, left, right, flags.contains(Flag::Carry)),
            AluOperation::And | AluOperation::Test => Arithmetic::logic(width, left & right),
            AluOperation::Sub | AluOperation::Compare => Arithmetic::sub(width, left, right, false),
            AluOperation::Xor => Arithmetic::logic(width, left ^ right),
        }
    }

    pub(crate) fn locked<M: GuestOperandMemory + ExclusiveMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        address: EffectiveAddress,
        source: ScalarRegister,
        width: ScalarWidth,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let bytes = Self::byte_count(width);
        let address = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let end = address.checked_add(u64::from(bytes) - 1);
        if !Self::is_canonical(address) || end.is_none_or(|value| !Self::is_canonical(value)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access: AccessKind::Write,
            };
        }
        let accumulator = cpu.read_register(ScalarRegister::General(0), width);
        let replacement = cpu.read_register(source, width);
        let observed = match memory.compare_exchange(
            address,
            bytes,
            false,
            AtomicValue {
                low: accumulator,
                high: 0,
            },
            AtomicValue {
                low: replacement,
                high: 0,
            },
            MemoryOrder::SequentiallyConsistent,
        ) {
            Ok(value) => value.low & Self::width_mask(width),
            Err(()) => {
                return ExecutionExit::OperandFault(crate::FaultAccess::operand(
                    instruction,
                    address,
                    AccessKind::Write,
                    u64::from(bytes),
                ));
            }
        };
        let arithmetic = Arithmetic::sub(Self::integer_width(width), accumulator, observed, false);
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.flags = staged.flags.apply(arithmetic.flags);
        if accumulator != observed {
            staged.write_register(ScalarRegister::General(0), width, observed);
        }
        *cpu = staged;
        if accumulator == observed {
            ExecutionExit::Continue
        } else {
            ExecutionExit::Yield {
                instruction,
                completed: 1,
            }
        }
    }

    const fn byte_count(width: ScalarWidth) -> u8 {
        match width {
            ScalarWidth::Byte => 1,
            ScalarWidth::Word => 2,
            ScalarWidth::Dword => 4,
            ScalarWidth::Qword => 8,
        }
    }

    const fn width_mask(width: ScalarWidth) -> u64 {
        match width {
            ScalarWidth::Byte => 0xff,
            ScalarWidth::Word => 0xffff,
            ScalarWidth::Dword => 0xffff_ffff,
            ScalarWidth::Qword => u64::MAX,
        }
    }

    const fn integer_width(width: ScalarWidth) -> IntegerWidth {
        match width {
            ScalarWidth::Byte => IntegerWidth::Byte,
            ScalarWidth::Word => IntegerWidth::Word,
            ScalarWidth::Dword => IntegerWidth::Dword,
            ScalarWidth::Qword => IntegerWidth::Qword,
        }
    }

    const fn is_canonical(address: u64) -> bool {
        (((address << 16) as i64) >> 16) as u64 == address
    }

    fn validate(address: u64, bytes: u8, instruction: u64, access: AccessKind) -> Result<(), ExecutionExit> {
        let end = address.checked_add(u64::from(bytes) - 1);
        if Self::is_canonical(address) && end.is_some_and(Self::is_canonical) {
            Ok(())
        } else {
            Err(ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            })
        }
    }
}
