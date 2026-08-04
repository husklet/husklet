use crate::{
    AccessKind, CpuState, DecodedInstruction, EffectiveAddress, ExecutionExit, GuestOperandMemory, ScalarInstruction,
    ScalarIrError,
};

pub(crate) struct MxcsrControl;

impl MxcsrControl {
    pub(crate) fn decode(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if decoded.raw_reg == Some(0) {
            return crate::x86::fxsave::Fxsave::decode(decoded);
        }
        if decoded.raw_mod == Some(3) || decoded.prefixes.operand_16 {
            return Err(ScalarIrError::Invalid);
        }
        Ok(ScalarInstruction::MxcsrControl {
            address: decoded.address.ok_or(ScalarIrError::Invalid)?,
            load: decoded.raw_reg == Some(2),
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
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(3)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            };
        }
        if load {
            let value = match memory.read(address, 4) {
                Ok(value) => value as u32,
                Err(()) => return Self::fault(instruction, address, access),
            };
            if value & !0xffff != 0 {
                return ExecutionExit::UndefinedInstruction { instruction };
            }
            let mut staged = cpu.clone();
            staged.rip = next;
            staged.mxcsr = value;
            *cpu = staged;
            return ExecutionExit::Continue;
        }
        let reservation = match memory.reserve_write(address, 4) {
            Ok(value) => value,
            Err(()) => return Self::fault(instruction, address, access),
        };
        if memory.commit_write(reservation, u64::from(cpu.mxcsr)).is_err() {
            return Self::fault(instruction, address, access);
        }
        cpu.rip = next;
        ExecutionExit::Continue
    }

    const fn canonical(address: u64) -> bool {
        (((address << 16) as i64) >> 16) as u64 == address
    }
    fn fault(instruction: u64, address: u64, access: AccessKind) -> ExecutionExit {
        ExecutionExit::OperandFault(crate::FaultAccess::operand(instruction, address, access, 4))
    }
}
