use crate::{
    AccessKind, CpuState, DecodedInstruction, ExecutionExit, FloatWidth, GuestOperandMemory, ScalarInstruction,
    ScalarIrError, VectorSource,
};

pub(crate) struct Transport;

impl Transport {
    pub(crate) fn decode(decoded: &DecodedInstruction) -> Result<ScalarInstruction, ScalarIrError> {
        Ok(ScalarInstruction::VectorScalarMove {
            destination: decoded.register.ok_or(ScalarIrError::Invalid)?,
            operand: super::Decoder::vector_source(decoded)?,
            store: decoded.opcode == 0x11,
            format: super::Decoder::float_format(decoded)?,
        })
    }

    pub(crate) fn execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        destination: u8,
        operand: VectorSource,
        store: bool,
        format: FloatWidth,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let bytes = match format {
            FloatWidth::Single => 4,
            FloatWidth::Double => 8,
        };
        if let VectorSource::Register(source) = operand {
            let (target, value) = if store {
                (source, cpu.vectors[usize::from(destination)])
            } else {
                (destination, cpu.vectors[usize::from(source)])
            };
            let mask = if format == FloatWidth::Single {
                u128::from(u32::MAX)
            } else {
                u128::from(u64::MAX)
            };
            let mut staged = cpu.clone();
            staged.rip = next;
            staged.vectors[usize::from(target)] = staged.vectors[usize::from(target)] & !mask | value & mask;
            *cpu = staged;
            return ExecutionExit::Continue;
        }
        let VectorSource::Memory(effective) = operand else {
            unreachable!()
        };
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let access = if store { AccessKind::Write } else { AccessKind::Read };
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(u64::from(bytes - 1))) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            };
        }
        if store {
            let reservation = match memory.reserve_write(address, bytes) {
                Ok(value) => value,
                Err(()) => return Self::fault(instruction, address, access, bytes),
            };
            if memory
                .commit_write(reservation, cpu.vectors[usize::from(destination)] as u64)
                .is_err()
            {
                return Self::fault(instruction, address, access, bytes);
            }
            cpu.rip = next;
            return ExecutionExit::Continue;
        }
        let value = match memory.read(address, bytes) {
            Ok(value) => value,
            Err(()) => return Self::fault(instruction, address, access, bytes),
        };
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.vectors[usize::from(destination)] = u128::from(value);
        *cpu = staged;
        ExecutionExit::Continue
    }

    const fn canonical(address: u64) -> bool {
        (((address << 16) as i64) >> 16) as u64 == address
    }

    fn fault(instruction: u64, address: u64, access: AccessKind, bytes: u8) -> ExecutionExit {
        ExecutionExit::OperandFault(crate::FaultAccess::operand(
            instruction,
            address,
            access,
            u64::from(bytes),
        ))
    }
}
