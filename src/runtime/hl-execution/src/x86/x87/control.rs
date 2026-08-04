use crate::{
    AccessKind, CpuState, DecodedInstruction, EffectiveAddress, ExecutionExit, GuestOperandMemory, ScalarInstruction,
    ScalarIrError,
};

pub(crate) struct Control;

impl Control {
    pub(crate) fn decode(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if decoded.opcode == 0x9b {
            return Ok(ScalarInstruction::Nop);
        }
        if !matches!(decoded.raw_reg, Some(5 | 7)) {
            return Err(ScalarIrError::Unsupported);
        }
        if decoded.raw_mod == Some(3) {
            return Err(ScalarIrError::Unsupported);
        }
        Ok(ScalarInstruction::X87Control {
            address: decoded.address.ok_or(ScalarIrError::Invalid)?,
            load: decoded.raw_reg == Some(5),
        })
    }

    pub(crate) fn execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        load: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let access = if load { AccessKind::Read } else { AccessKind::Write };
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(1)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            };
        }
        if load {
            let value = match memory.read(address, 2) {
                Ok(value) => value as u16,
                Err(()) => return Self::fault(instruction, address, access),
            };
            let mut staged = cpu.clone();
            staged.rip = next;
            staged.x87_control = value & 0x1f3f | 0x0040;
            *cpu = staged;
            return ExecutionExit::Continue;
        }
        let reservation = match memory.reserve_write(address, 2) {
            Ok(value) => value,
            Err(()) => return Self::fault(instruction, address, access),
        };
        if memory.commit_write(reservation, u64::from(cpu.x87_control)).is_err() {
            return Self::fault(instruction, address, access);
        }
        cpu.rip = next;
        ExecutionExit::Continue
    }

    const fn canonical(address: u64) -> bool {
        (((address << 16) as i64) >> 16) as u64 == address
    }
    fn fault(instruction: u64, address: u64, access: AccessKind) -> ExecutionExit {
        ExecutionExit::OperandFault(crate::FaultAccess::operand(instruction, address, access, 2))
    }
}
