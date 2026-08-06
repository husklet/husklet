use super::ScalarInterpreter;
use crate::{
    AccessKind, CpuState, ExecutionExit, GuestOperandMemory, IntegerWidth, ScalarOperand, ScalarRegister, ScalarWidth,
    Staged,
};

impl ScalarInterpreter {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn vector_move<M: GuestOperandMemory>(
        mut staged: CpuState,
        cpu: &CpuState,
        memory: &M,
        vector: u8,
        scalar: ScalarOperand,
        to_vector: bool,
        width: ScalarWidth,
        next: u64,
        instruction: u64,
    ) -> Result<Staged<M::Reservation, M::BatchReservation>, ExecutionExit> {
        if to_vector {
            let value = Self::read(cpu, memory, scalar, width, next, instruction)?;
            staged.vectors[usize::from(vector)] = u128::from(value);
            Ok(Staged::Cpu(staged))
        } else {
            let value = staged.vectors[usize::from(vector)] as u64;
            Self::write(staged, memory, scalar, width, value, next, instruction)
        }
    }
    pub(super) fn leave<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        length: u8,
        width: ScalarWidth,
        address_32: bool,
    ) -> ExecutionExit {
        let instruction = cpu.rip;
        cpu.registers[4] = if address_32 {
            cpu.registers[5] & 0xffff_ffff
        } else {
            cpu.registers[5]
        };
        let bytes = Self::bytes(width);
        let value = match Self::memory_read(memory, cpu.registers[4], bytes, instruction) {
            Ok(value) => value,
            Err(exit) => return exit,
        };
        cpu.registers[4] = if address_32 {
            cpu.registers[4].wrapping_add(u64::from(bytes)) & 0xffff_ffff
        } else {
            cpu.registers[4].wrapping_add(u64::from(bytes))
        };
        cpu.write_register(ScalarRegister::General(5), width, value);
        cpu.rip = instruction.wrapping_add(u64::from(length));
        ExecutionExit::Continue
    }
    pub(super) fn call<M: GuestOperandMemory>(
        mut staged: CpuState,
        memory: &M,
        width: ScalarWidth,
        next: u64,
        target: u64,
        instruction: u64,
    ) -> Result<Staged<M::Reservation, M::BatchReservation>, ExecutionExit> {
        let bytes = Self::bytes(width);
        staged.registers[4] = staged.registers[4].wrapping_sub(u64::from(bytes));
        Self::canonical(target, 1, instruction, AccessKind::Execute)?;
        staged.rip = target;
        let stack = Self::stack(staged.registers[4]);
        Self::write(
            staged,
            memory,
            ScalarOperand::Memory(stack),
            width,
            next,
            next,
            instruction,
        )
    }
    pub(crate) fn read<M: GuestOperandMemory>(
        cpu: &CpuState,
        memory: &M,
        operand: ScalarOperand,
        width: ScalarWidth,
        next: u64,
        instruction: u64,
    ) -> Result<u64, ExecutionExit> {
        match operand {
            ScalarOperand::Immediate(value) => Ok(value as u64),
            ScalarOperand::Register(register) => Ok(cpu.read_register(register, width)),
            ScalarOperand::Memory(address) => {
                let address = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                Self::memory_read(memory, address, Self::bytes(width), instruction)
            }
        }
    }
    pub(crate) fn write<M: GuestOperandMemory>(
        mut staged: CpuState,
        memory: &M,
        operand: ScalarOperand,
        width: ScalarWidth,
        value: u64,
        next: u64,
        instruction: u64,
    ) -> Result<Staged<M::Reservation, M::BatchReservation>, ExecutionExit> {
        match operand {
            ScalarOperand::Register(register) => {
                staged.write_register(register, width, value);
                Ok(Staged::Cpu(staged))
            }
            ScalarOperand::Memory(address) => {
                let address = address.resolve(&staged.registers, next, staged.fs_base, staged.gs_base);
                let bytes = Self::bytes(width);
                Self::canonical(address, bytes, instruction, AccessKind::Write)?;
                let reservation = memory.reserve_write(address, bytes).map_err(|()| {
                    ExecutionExit::OperandFault(crate::FaultAccess::operand(
                        instruction,
                        address,
                        AccessKind::Write,
                        u64::from(bytes),
                    ))
                })?;
                Ok(Staged::Write(staged, reservation, value, address, bytes))
            }
            ScalarOperand::Immediate(_) => Err(ExecutionExit::UndefinedInstruction { instruction }),
        }
    }
    pub(super) fn memory_read<M: GuestOperandMemory>(
        memory: &M,
        address: u64,
        bytes: u8,
        instruction: u64,
    ) -> Result<u64, ExecutionExit> {
        Self::canonical(address, bytes, instruction, AccessKind::Read)?;
        memory.read(address, bytes).map_err(|()| {
            ExecutionExit::OperandFault(crate::FaultAccess::operand(
                instruction,
                address,
                AccessKind::Read,
                u64::from(bytes),
            ))
        })
    }
    pub(super) fn canonical(
        address: u64,
        bytes: u8,
        instruction: u64,
        access: AccessKind,
    ) -> Result<(), ExecutionExit> {
        let end = address
            .checked_add(u64::from(bytes) - 1)
            .ok_or(ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            })?;
        if !Self::is_canonical(address) || !Self::is_canonical(end) {
            return Err(ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            });
        }
        Ok(())
    }
    const fn is_canonical(address: u64) -> bool {
        (((address << 16) as i64) >> 16) as u64 == address
    }
    pub(super) const fn bytes(width: ScalarWidth) -> u8 {
        match width {
            ScalarWidth::Byte => 1,
            ScalarWidth::Word => 2,
            ScalarWidth::Dword => 4,
            ScalarWidth::Qword => 8,
        }
    }
    pub(super) const fn mask(width: ScalarWidth) -> u64 {
        match width {
            ScalarWidth::Byte => 0xff,
            ScalarWidth::Word => 0xffff,
            ScalarWidth::Dword => 0xffff_ffff,
            ScalarWidth::Qword => u64::MAX,
        }
    }
    pub(super) const fn integer_width(width: ScalarWidth) -> IntegerWidth {
        match width {
            ScalarWidth::Byte => IntegerWidth::Byte,
            ScalarWidth::Word => IntegerWidth::Word,
            ScalarWidth::Dword => IntegerWidth::Dword,
            ScalarWidth::Qword => IntegerWidth::Qword,
        }
    }
    pub(super) fn stack(address: u64) -> crate::EffectiveAddress {
        crate::EffectiveAddress {
            displacement: address as i64,
            ..crate::EffectiveAddress::default()
        }
    }
}
