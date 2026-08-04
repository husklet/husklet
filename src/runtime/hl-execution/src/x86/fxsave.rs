use crate::{
    AccessKind, CpuState, DecodedInstruction, EffectiveAddress, ExecutionExit, GuestOperandMemory, ScalarInstruction,
    ScalarIrError,
};

pub(crate) struct Fxsave;

impl Fxsave {
    pub(crate) fn decode(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        if matches!(decoded.raw_reg, Some(2 | 3)) {
            return crate::x86::mxcsr_control::MxcsrControl::decode(decoded);
        }
        if decoded.raw_mod == Some(3) || decoded.prefixes.operand_16 || decoded.prefixes.rep || decoded.prefixes.repne {
            return Err(ScalarIrError::Invalid);
        }
        let address = decoded.address.ok_or(ScalarIrError::Invalid)?;
        Ok(if decoded.raw_reg == Some(1) {
            ScalarInstruction::Fxrstor { address }
        } else {
            ScalarInstruction::Fxsave { address }
        })
    }

    pub(crate) fn execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(511)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access: AccessKind::Write,
            };
        }
        if address & 15 != 0 {
            return ExecutionExit::AlignmentFault {
                instruction,
                address,
                access: AccessKind::Write,
            };
        }
        let mut values = [0_u64; 64];
        let top = usize::from((cpu.x87_status >> 11) & 7);
        let mut tags = 0_u8;
        for logical in 0..8 {
            let physical = (top + logical) & 7;
            if cpu.x87_classes[physical] != crate::ExtendedClass::Empty {
                tags |= 1 << physical;
            }
            let bits = cpu.x87_values[physical].bits();
            values[4 + logical * 2] = bits as u64;
            values[5 + logical * 2] = (bits >> 64) as u64;
        }
        values[0] = u64::from(cpu.x87_control) | (u64::from(cpu.x87_status) << 16) | (u64::from(tags) << 32);
        // MXCSR_MASK advertises exactly the 16 architectural bits accepted by
        // the paired LDMXCSR implementation.
        values[3] = u64::from(cpu.mxcsr) | (0x0000_ffff_u64 << 32);
        for (index, vector) in cpu.vectors.iter().enumerate() {
            values[20 + index * 2] = *vector as u64;
            values[21 + index * 2] = (*vector >> 64) as u64;
        }
        let writes: [(u64, u8); 64] = std::array::from_fn(|index| (address + (index as u64) * 8, 8));
        let reservation = match memory.reserve_write_batch(&writes) {
            Ok(value) => value,
            Err(fault) => return Self::fault(instruction, fault),
        };
        if memory.commit_write_batch(reservation, &values).is_err() {
            return Self::fault(instruction, address);
        }
        cpu.rip = next;
        ExecutionExit::Continue
    }

    pub(crate) fn restore<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(511)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access: AccessKind::Read,
            };
        }
        if address & 15 != 0 {
            return ExecutionExit::AlignmentFault {
                instruction,
                address,
                access: AccessKind::Read,
            };
        }
        let mut values = [0_u64; 64];
        for (index, value) in values.iter_mut().enumerate() {
            let current = address + (index as u64) * 8;
            *value = match memory.read(current, 8) {
                Ok(value) => value,
                Err(()) => return Self::read_fault(instruction, current),
            };
        }
        let mxcsr = values[3] as u32;
        if mxcsr & !0xffff != 0 {
            return ExecutionExit::UndefinedInstruction { instruction };
        }
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.x87_control = values[0] as u16 & 0x1f3f | 0x0040;
        staged.x87_status = (values[0] >> 16) as u16;
        let tags = (values[0] >> 32) as u8;
        let top = usize::from((staged.x87_status >> 11) & 7);
        for logical in 0..8 {
            let physical = (top + logical) & 7;
            let value = crate::ExtendedReal::from_bits(
                u128::from(values[4 + logical * 2]) | u128::from(values[5 + logical * 2] & 0xffff) << 64,
            );
            staged.x87_values[physical] = value;
            staged.x87_classes[physical] = if tags & (1 << physical) == 0 {
                crate::ExtendedClass::Empty
            } else {
                value.class()
            };
        }
        staged.mxcsr = mxcsr;
        for index in 0..16 {
            staged.vectors[index] = u128::from(values[20 + index * 2]) | (u128::from(values[21 + index * 2]) << 64);
        }
        *cpu = staged;
        ExecutionExit::Continue
    }

    const fn canonical(address: u64) -> bool {
        (((address << 16) as i64) >> 16) as u64 == address
    }
    fn fault(instruction: u64, address: u64) -> ExecutionExit {
        ExecutionExit::OperandFault(crate::FaultAccess::operand(
            instruction,
            address,
            AccessKind::Write,
            512,
        ))
    }
    fn read_fault(instruction: u64, address: u64) -> ExecutionExit {
        ExecutionExit::OperandFault(crate::FaultAccess::operand(instruction, address, AccessKind::Read, 512))
    }
}
