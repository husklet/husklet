use crate::{
    AccessKind, AtomicOperation, BitAction, BitPlan, CpuState, ExclusiveMemory, ExecutionExit, Flag,
    GuestOperandMemory, MemoryOrder, ScalarOperand, ScalarRegister, ScalarWidth,
};

pub(crate) struct BitExecutor;

impl BitExecutor {
    pub(crate) fn execute<M: GuestOperandMemory + ExclusiveMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        action: BitAction,
        destination: ScalarOperand,
        index: ScalarOperand,
        locked: bool,
        width: ScalarWidth,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let index = Self::index(cpu, index, width);
        match destination {
            ScalarOperand::Register(register) => Self::register(cpu, action, register, index, width, next),
            ScalarOperand::Memory(effective) => {
                Self::memory(cpu, memory, action, effective, index, locked, width, instruction, next)
            }
            ScalarOperand::Immediate(_) => unreachable!(),
        }
    }

    fn register(
        cpu: &mut CpuState,
        action: BitAction,
        register: ScalarRegister,
        index: i64,
        width: ScalarWidth,
        next: u64,
    ) -> ExecutionExit {
        let value = cpu.read_register(register, width);
        let bit = (index as u64 & (u64::from(Self::bits(width)) - 1)) as u8;
        let mask = 1_u64 << bit;
        let result = match action {
            BitAction::Test => value,
            BitAction::Set => value | mask,
            BitAction::Reset => value & !mask,
            BitAction::Complement => value ^ mask,
        };
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.flags = staged.flags.with(Flag::Carry, value & mask != 0);
        if action != BitAction::Test {
            staged.write_register(register, width, result);
        }
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn memory<M: GuestOperandMemory + ExclusiveMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        action: BitAction,
        effective: crate::EffectiveAddress,
        index: i64,
        locked: bool,
        width: ScalarWidth,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let base = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let address = base.wrapping_add_signed(index >> 3);
        if !Self::canonical(address) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access: AccessKind::Read,
            };
        }
        let bit = (index & 7) as u8;
        let prior = if locked {
            Self::locked(memory, action, address, bit, width, instruction)
        } else {
            Self::unlocked(memory, action, address, index, width, instruction)
        };
        let prior = match prior {
            Ok(value) => value,
            Err(exit) => return exit,
        };
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.flags = staged.flags.with(Flag::Carry, prior);
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn locked<M: ExclusiveMemory>(
        memory: &mut M,
        action: BitAction,
        address: u64,
        bit: u8,
        width: ScalarWidth,
        instruction: u64,
    ) -> Result<bool, ExecutionExit> {
        let operation = match action {
            BitAction::Set => AtomicOperation::Set,
            BitAction::Reset => AtomicOperation::Clear,
            BitAction::Complement => AtomicOperation::ExclusiveOr,
            BitAction::Test => unreachable!(),
        };
        memory
            .fetch_update(address, 1, operation, 1 << bit, MemoryOrder::SequentiallyConsistent)
            .map(|old| old & (1 << bit) != 0)
            .map_err(|()| {
                ExecutionExit::OperandFault(crate::FaultAccess::operand(
                    instruction,
                    address,
                    AccessKind::Write,
                    Self::bytes(width),
                ))
            })
    }

    fn unlocked<M: GuestOperandMemory>(
        memory: &mut M,
        action: BitAction,
        address: u64,
        index: i64,
        width: ScalarWidth,
        instruction: u64,
    ) -> Result<bool, ExecutionExit> {
        let byte = memory.read(address, 1).map_err(|()| {
            ExecutionExit::OperandFault(crate::FaultAccess::operand(
                instruction,
                address,
                AccessKind::Read,
                Self::bytes(width),
            ))
        })? as u8;
        let plan = BitPlan::memory(index, byte, action);
        if action == BitAction::Test {
            return Ok(plan.prior);
        }
        let reservation = memory.reserve_write(address, 1).map_err(|()| {
            ExecutionExit::OperandFault(crate::FaultAccess::operand(
                instruction,
                address,
                AccessKind::Write,
                Self::bytes(width),
            ))
        })?;
        memory
            .commit_write(reservation, u64::from(plan.proposed))
            .map_err(|()| {
                ExecutionExit::OperandFault(crate::FaultAccess::operand(
                    instruction,
                    address,
                    AccessKind::Write,
                    Self::bytes(width),
                ))
            })?;
        Ok(plan.prior)
    }

    fn index(cpu: &CpuState, operand: ScalarOperand, width: ScalarWidth) -> i64 {
        match operand {
            ScalarOperand::Register(register) => match width {
                ScalarWidth::Word => i64::from(cpu.read_register(register, width) as u16 as i16),
                ScalarWidth::Dword => cpu.read_register(register, width) as i32 as i64,
                ScalarWidth::Qword => cpu.read_register(register, width) as i64,
                ScalarWidth::Byte => unreachable!(),
            },
            ScalarOperand::Immediate(value) => value,
            ScalarOperand::Memory(_) => unreachable!(),
        }
    }

    const fn bytes(width: ScalarWidth) -> u64 {
        match width {
            ScalarWidth::Byte => 1,
            ScalarWidth::Word => 2,
            ScalarWidth::Dword => 4,
            ScalarWidth::Qword => 8,
        }
    }

    const fn bits(width: ScalarWidth) -> u8 {
        match width {
            ScalarWidth::Word => 16,
            ScalarWidth::Dword => 32,
            ScalarWidth::Qword => 64,
            ScalarWidth::Byte => 8,
        }
    }

    const fn canonical(address: u64) -> bool {
        (((address << 16) as i64) >> 16) as u64 == address
    }
}
