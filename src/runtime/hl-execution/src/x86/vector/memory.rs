use crate::{AccessKind, CpuState, ExecutionExit, GuestOperandMemory, Staged, VectorLane, VectorSource};

pub enum Transfer<B> {
    Cpu,
    Batch(B, [u64; 4], u64),
}

pub struct Memory;

impl Memory {
    pub fn duplicate<M: GuestOperandMemory>(
        staged: &mut CpuState,
        cpu: &CpuState,
        memory: &M,
        destination: u8,
        source: VectorSource,
        lane: u8,
        high: bool,
        next: u64,
        instruction: u64,
    ) -> Result<Staged<M::Reservation, M::BatchReservation>, ExecutionExit> {
        let value = match source {
            VectorSource::Register(source) => cpu.vectors[usize::from(source)],
            VectorSource::Memory(address) if lane == 8 => {
                let address = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                Self::canonical_range(address, 8, instruction, AccessKind::Read)?;
                u128::from(memory.read(address, 8).map_err(|()| {
                    ExecutionExit::OperandFault(crate::FaultAccess::operand(instruction, address, AccessKind::Read, 8))
                })?)
            }
            VectorSource::Memory(_) => VectorLane::read(cpu, memory, source, next, instruction)?,
        };
        staged.vectors[usize::from(destination)] = if lane == 8 {
            let low = value & u128::from(u64::MAX);
            low | (low << 64)
        } else {
            let first = if high { value >> 32 } else { value } & u128::from(u32::MAX);
            let second = if high { value >> 96 } else { value >> 64 } & u128::from(u32::MAX);
            first | (first << 32) | (second << 64) | (second << 96)
        };
        Ok(Staged::Cpu)
    }

    pub fn staged_transfer<M: GuestOperandMemory>(
        staged: &mut CpuState,
        cpu: &CpuState,
        memory: &M,
        vector: u8,
        operand: VectorSource,
        store: bool,
        aligned: bool,
        next: u64,
        instruction: u64,
    ) -> Result<Staged<M::Reservation, M::BatchReservation>, ExecutionExit> {
        match Self::transfer(staged, cpu, memory, vector, operand, store, aligned, next, instruction)? {
            Transfer::Cpu => Ok(Staged::Cpu),
            Transfer::Batch(reservation, values, address) => Ok(Staged::Batch(reservation, values, address, 16)),
        }
    }

    pub fn half<M: GuestOperandMemory>(
        staged: &mut CpuState,
        cpu: &CpuState,
        memory: &M,
        vector: u8,
        source: VectorSource,
        store: bool,
        high: bool,
        next: u64,
        instruction: u64,
    ) -> Result<Staged<M::Reservation, M::BatchReservation>, ExecutionExit> {
        if let VectorSource::Register(source) = source {
            let prior = cpu.vectors[usize::from(vector)];
            let source = cpu.vectors[usize::from(source)];
            staged.vectors[usize::from(vector)] = if high {
                (prior & u128::from(u64::MAX)) | ((source & u128::from(u64::MAX)) << 64)
            } else {
                (prior & (u128::from(u64::MAX) << 64)) | u128::from((source >> 64) as u64)
            };
            return Ok(Staged::Cpu);
        }
        let VectorSource::Memory(address) = source else {
            unreachable!()
        };
        let address = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let access = if store { AccessKind::Write } else { AccessKind::Read };
        Self::canonical_range(address, 8, instruction, access)?;
        if store {
            let reservation = memory.reserve_write(address, 8).map_err(|()| {
                ExecutionExit::OperandFault(crate::FaultAccess::operand(instruction, address, access, 8))
            })?;
            let vector = cpu.vectors[usize::from(vector)];
            let value = if high { (vector >> 64) as u64 } else { vector as u64 };
            return Ok(Staged::Write(reservation, value, address, 8));
        }
        let value = memory
            .read(address, 8)
            .map_err(|()| ExecutionExit::OperandFault(crate::FaultAccess::operand(instruction, address, access, 8)))?;
        let prior = cpu.vectors[usize::from(vector)];
        staged.vectors[usize::from(vector)] = if high {
            (prior & u128::from(u64::MAX)) | (u128::from(value) << 64)
        } else {
            (prior & (u128::from(u64::MAX) << 64)) | u128::from(value)
        };
        Ok(Staged::Cpu)
    }

    pub fn transfer<M: GuestOperandMemory>(
        staged: &mut CpuState,
        cpu: &CpuState,
        memory: &M,
        vector: u8,
        operand: VectorSource,
        store: bool,
        aligned: bool,
        next: u64,
        instruction: u64,
    ) -> Result<Transfer<M::BatchReservation>, ExecutionExit> {
        if let VectorSource::Register(index) = operand {
            let value = cpu.vectors[usize::from(if store { vector } else { index })];
            staged.vectors[usize::from(if store { index } else { vector })] = value;
            return Ok(Transfer::Cpu);
        }
        let VectorSource::Memory(address) = operand else {
            unreachable!()
        };
        let address = address.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let access = if store { AccessKind::Write } else { AccessKind::Read };
        Self::canonical(address, instruction, access)?;
        if aligned && address & 15 != 0 {
            return Err(ExecutionExit::AlignmentFault {
                instruction,
                address,
                access,
            });
        }
        if !store {
            staged.vectors[usize::from(vector)] = VectorLane::read(cpu, memory, operand, next, instruction)?;
            return Ok(Transfer::Cpu);
        }
        let value = cpu.vectors[usize::from(vector)];
        let writes = [(address, 8), (address + 8, 8)];
        let reservation = memory.reserve_write_batch(&writes).map_err(|fault| {
            ExecutionExit::OperandFault(crate::FaultAccess::operand(instruction, fault, AccessKind::Write, 16))
        })?;
        Ok(Transfer::Batch(
            reservation,
            [value as u64, (value >> 64) as u64, 0, 0],
            address,
        ))
    }

    fn canonical(address: u64, instruction: u64, access: AccessKind) -> Result<(), ExecutionExit> {
        Self::canonical_range(address, 16, instruction, access)
    }

    fn canonical_range(address: u64, size: u64, instruction: u64, access: AccessKind) -> Result<(), ExecutionExit> {
        let end = address.checked_add(size - 1).ok_or(ExecutionExit::NonCanonical {
            instruction,
            address,
            access,
        })?;
        if Self::is_canonical(address) && Self::is_canonical(end) {
            Ok(())
        } else {
            Err(ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            })
        }
    }

    const fn is_canonical(address: u64) -> bool {
        let upper = address >> 48;
        upper == 0 || upper == 0xffff
    }
}
