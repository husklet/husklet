use super::ScalarInterpreter;
use crate::{
    ControlFlag, CpuState, ExecutionExit, Flag, FlagState, GuestOperandMemory, ScalarOperand, ScalarState, ScalarWidth,
    Staged,
};

impl ScalarInterpreter {
    pub(super) fn push_flags<M: GuestOperandMemory>(
        staged: &mut ScalarState,
        cpu: &CpuState,
        memory: &M,
        width: ScalarWidth,
        next: u64,
        instruction: u64,
    ) -> Result<Staged<M::Reservation, M::BatchReservation>, ExecutionExit> {
        let mut value = u64::from(cpu.flags.bits()) | 0x202;
        value |= u64::from(cpu.direction) << 10;
        value |= u64::from(cpu.alignment_check) << 18;
        value |= u64::from(cpu.id_flag) << 21;
        let bytes = Self::bytes(width);
        staged.registers[4] = staged.registers[4].wrapping_sub(u64::from(bytes));
        let stack = Self::stack(staged.registers[4]);
        Self::write(
            staged,
            memory,
            ScalarOperand::Memory(stack),
            width,
            value,
            next,
            instruction,
        )
    }

    pub(super) fn pop_flags<M: GuestOperandMemory>(
        staged: &mut ScalarState,
        cpu: &CpuState,
        memory: &M,
        width: ScalarWidth,
        instruction: u64,
    ) -> Result<Staged<M::Reservation, M::BatchReservation>, ExecutionExit> {
        let bytes = Self::bytes(width);
        let value = Self::memory_read(memory, cpu.registers[4], bytes, instruction)?;
        staged.registers[4] = staged.registers[4].wrapping_add(u64::from(bytes));
        Self::restore_flags(staged, value, width);
        Ok(Staged::Cpu)
    }

    pub(super) fn iret<M: GuestOperandMemory>(
        staged: &mut ScalarState,
        cpu: &CpuState,
        memory: &M,
        instruction: u64,
    ) -> Result<Staged<M::Reservation, M::BatchReservation>, ExecutionExit> {
        let mut frame = [0_u64; 5];
        for (offset, value) in [0_u64, 8, 16, 24, 32].into_iter().zip(&mut frame) {
            let address = cpu.registers[4]
                .checked_add(offset)
                .ok_or(ExecutionExit::NonCanonical {
                    instruction,
                    address: cpu.registers[4],
                    access: crate::AccessKind::Read,
                })?;
            *value = Self::memory_read(memory, address, 8, instruction)?;
        }
        Self::canonical(frame[0], 1, instruction, crate::AccessKind::Execute)?;
        staged.rip = frame[0];
        staged.registers[4] = frame[3];
        Self::restore_flags(staged, frame[2], ScalarWidth::Qword);
        Ok(Staged::Cpu)
    }

    fn restore_flags(cpu: &mut ScalarState, value: u64, width: ScalarWidth) {
        cpu.flags = FlagState::from_bits(value as u16 & 0x8d5);
        cpu.direction = value & (1 << 10) != 0;
        if width == ScalarWidth::Qword {
            cpu.alignment_check = value & (1 << 18) != 0;
            cpu.id_flag = value & (1 << 21) != 0;
        }
    }

    pub(super) fn control(staged: &mut ScalarState, cpu: &CpuState, operation: ControlFlag) {
        match operation {
            ControlFlag::ComplementCarry => {
                staged.flags = staged.flags.with(Flag::Carry, !cpu.flags.contains(Flag::Carry));
            }
            ControlFlag::ClearCarry => staged.flags = staged.flags.with(Flag::Carry, false),
            ControlFlag::SetCarry => staged.flags = staged.flags.with(Flag::Carry, true),
            ControlFlag::ClearDirection => staged.direction = false,
            ControlFlag::SetDirection => staged.direction = true,
        }
    }
}
