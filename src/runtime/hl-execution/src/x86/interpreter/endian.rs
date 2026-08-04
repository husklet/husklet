use super::ScalarInterpreter;
use crate::{CpuState, ExecutionExit, GuestOperandMemory, ScalarOperand, ScalarRegister, ScalarWidth, Staged};

impl ScalarInterpreter {
    pub(super) fn byte_swap(value: u64, width: ScalarWidth) -> u64 {
        match width {
            ScalarWidth::Dword => u64::from((value as u32).swap_bytes()),
            ScalarWidth::Qword => value.swap_bytes(),
            ScalarWidth::Byte | ScalarWidth::Word => unreachable!(),
        }
    }

    pub(super) fn endian<M: GuestOperandMemory>(
        mut staged: CpuState,
        cpu: &CpuState,
        memory: &M,
        register: ScalarRegister,
        address: crate::EffectiveAddress,
        store: bool,
        width: ScalarWidth,
        next: u64,
        instruction: u64,
    ) -> Result<Staged<M::Reservation, M::BatchReservation>, ExecutionExit> {
        let operand = ScalarOperand::Memory(address);
        if store {
            let value = cpu.read_register(register, width);
            Self::write(
                staged,
                memory,
                operand,
                width,
                Self::swap(value, width),
                next,
                instruction,
            )
        } else {
            let value = Self::read(cpu, memory, operand, width, next, instruction)?;
            staged.write_register(register, width, Self::swap(value, width));
            Ok(Staged::Cpu(staged))
        }
    }

    fn swap(value: u64, width: ScalarWidth) -> u64 {
        match width {
            ScalarWidth::Word => u64::from((value as u16).swap_bytes()),
            ScalarWidth::Dword => u64::from((value as u32).swap_bytes()),
            ScalarWidth::Qword => value.swap_bytes(),
            ScalarWidth::Byte => unreachable!(),
        }
    }
}
