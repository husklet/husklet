use super::interpreter::Aarch64MemoryInterpreter;
use crate::{
    Aarch64CpuState, Aarch64ExecutionExit, AccessKind, GuestOperandMemory, MemoryAddress, PcCoordinatePort,
    SystemRegister,
};

impl Aarch64MemoryInterpreter {
    pub(super) fn vector_load<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &M,
        coordinates: &dyn PcCoordinatePort,
        destination: u8,
        bytes: u8,
        address: MemoryAddress,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let resolved = Self::resolve(cpu, coordinates, address);
        let low_bytes = bytes.min(8);
        let low = match memory.read(resolved.address, low_bytes) {
            Ok(value) => value,
            Err(()) => return Self::fault(instruction, resolved.address, AccessKind::Read, low_bytes),
        };
        let high = if bytes == 16 {
            match memory.read(resolved.address.wrapping_add(8), 8) {
                Ok(value) => value,
                Err(()) => {
                    return Self::fault(instruction, resolved.address.wrapping_add(8), AccessKind::Read, 8_u8);
                }
            }
        } else {
            0
        };
        let mut staged = cpu.clone();
        staged.set_vector(destination, u128::from(low) | u128::from(high) << 64);
        Self::writeback(&mut staged, resolved);
        staged.pc = instruction.wrapping_add(4);
        *cpu = staged;
        Aarch64ExecutionExit::Continue
    }

    pub(super) fn vector_store<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut M,
        coordinates: &dyn PcCoordinatePort,
        source: u8,
        bytes: u8,
        address: MemoryAddress,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let resolved = Self::resolve(cpu, coordinates, address);
        let value = cpu.vector(source);
        if bytes == 16 {
            let second = resolved.address.wrapping_add(8);
            let writes = [(resolved.address, 8), (second, 8)];
            let reservation = match memory.reserve_write_batch(&writes) {
                Ok(reservation) => reservation,
                Err(address) => return Self::fault(instruction, address, AccessKind::Write, 8_u8),
            };
            let values = [value as u64, (value >> 64) as u64];
            if memory.commit_write_batch(reservation, &values).is_err() {
                return Self::fault(instruction, resolved.address, AccessKind::Write, 16_u8);
            }
        } else {
            let reservation = match memory.reserve_write(resolved.address, bytes) {
                Ok(reservation) => reservation,
                Err(()) => return Self::fault(instruction, resolved.address, AccessKind::Write, bytes),
            };
            if memory.commit_write(reservation, value as u64).is_err() {
                return Self::fault(instruction, resolved.address, AccessKind::Write, bytes);
            }
        }
        let mut staged = cpu.clone();
        Self::writeback(&mut staged, resolved);
        staged.pc = instruction.wrapping_add(4);
        *cpu = staged;
        Aarch64ExecutionExit::Continue
    }

    pub(super) fn vector_load_pair<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &M,
        coordinates: &dyn PcCoordinatePort,
        registers: (u8, u8),
        bytes: u8,
        address: MemoryAddress,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let resolved = Self::resolve(cpu, coordinates, address);
        let first = match Self::read_vector(memory, resolved.address, bytes) {
            Ok(value) => value,
            Err(address) => return Self::fault(instruction, address, AccessKind::Read, bytes.min(8)),
        };
        let second_address = resolved.address.wrapping_add(u64::from(bytes));
        let second = match Self::read_vector(memory, second_address, bytes) {
            Ok(value) => value,
            Err(address) => return Self::fault(instruction, address, AccessKind::Read, bytes.min(8)),
        };
        let mut staged = cpu.clone();
        staged.set_vector(registers.0, first);
        staged.set_vector(registers.1, second);
        Self::writeback(&mut staged, resolved);
        staged.pc = instruction.wrapping_add(4);
        *cpu = staged;
        Aarch64ExecutionExit::Continue
    }

    pub(super) fn vector_load_group<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &M,
        coordinates: &dyn PcCoordinatePort,
        first: u8,
        count: u8,
        bytes: u8,
        address: MemoryAddress,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let resolved = Self::resolve(cpu, coordinates, address);
        let mut values = [0; 4];
        for index in 0..count {
            let address = resolved.address.wrapping_add(u64::from(bytes) * u64::from(index));
            values[index as usize] = match Self::read_vector(memory, address, bytes) {
                Ok(value) => value,
                Err(address) => return Self::fault(instruction, address, AccessKind::Read, bytes.min(8)),
            };
        }
        let mut staged = cpu.clone();
        for index in 0..count {
            staged.set_vector(first.wrapping_add(index) & 31, values[index as usize]);
        }
        Self::writeback(&mut staged, resolved);
        staged.pc = instruction.wrapping_add(4);
        *cpu = staged;
        Aarch64ExecutionExit::Continue
    }

    pub(super) fn vector_store_pair<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut M,
        coordinates: &dyn PcCoordinatePort,
        registers: (u8, u8),
        bytes: u8,
        address: MemoryAddress,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let resolved = Self::resolve(cpu, coordinates, address);
        let values128 = [cpu.vector(registers.0), cpu.vector(registers.1)];
        let mut writes = [(0, 0); 4];
        let mut values = [0; 4];
        let chunks = if bytes == 16 { 4 } else { 2 };
        for register in 0..2 {
            let base = resolved.address.wrapping_add(u64::from(bytes) * register as u64);
            let index = register * if bytes == 16 { 2 } else { 1 };
            writes[index] = (base, bytes.min(8));
            values[index] = values128[register] as u64;
            if bytes == 16 {
                writes[index + 1] = (base.wrapping_add(8), 8);
                values[index + 1] = (values128[register] >> 64) as u64;
            }
        }
        let reservation = match memory.reserve_write_batch(&writes[..chunks]) {
            Ok(reservation) => reservation,
            Err(address) => return Self::fault(instruction, address, AccessKind::Write, 8_u8),
        };
        if memory.commit_write_batch(reservation, &values[..chunks]).is_err() {
            return Self::fault(instruction, resolved.address, AccessKind::Write, u64::from(bytes) * 2);
        }
        let mut staged = cpu.clone();
        Self::writeback(&mut staged, resolved);
        staged.pc = instruction.wrapping_add(4);
        *cpu = staged;
        Aarch64ExecutionExit::Continue
    }

    pub(super) fn vector_store_group<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut M,
        coordinates: &dyn PcCoordinatePort,
        first: u8,
        count: u8,
        bytes: u8,
        address: MemoryAddress,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let resolved = Self::resolve(cpu, coordinates, address);
        let mut writes = [(0, 0); 8];
        let mut values = [0; 8];
        let chunks = usize::from(count) * if bytes == 16 { 2 } else { 1 };
        for register in 0..count {
            let value = cpu.vector(first.wrapping_add(register) & 31);
            let index = usize::from(register) * if bytes == 16 { 2 } else { 1 };
            let base = resolved.address.wrapping_add(u64::from(bytes) * u64::from(register));
            writes[index] = (base, bytes.min(8));
            values[index] = value as u64;
            if bytes == 16 {
                writes[index + 1] = (base.wrapping_add(8), 8);
                values[index + 1] = (value >> 64) as u64;
            }
        }
        let reservation = match memory.reserve_write_batch(&writes[..chunks]) {
            Ok(reservation) => reservation,
            Err(address) => return Self::fault(instruction, address, AccessKind::Write, 8_u8),
        };
        if memory.commit_write_batch(reservation, &values[..chunks]).is_err() {
            return Self::fault(
                instruction,
                resolved.address,
                AccessKind::Write,
                u64::from(bytes) * u64::from(count),
            );
        }
        let mut staged = cpu.clone();
        Self::writeback(&mut staged, resolved);
        staged.pc = instruction.wrapping_add(4);
        *cpu = staged;
        Aarch64ExecutionExit::Continue
    }

    pub(super) fn read_vector<M: GuestOperandMemory>(memory: &M, address: u64, bytes: u8) -> Result<u128, u64> {
        let low = memory.read(address, bytes.min(8)).map_err(|()| address)?;
        let high = if bytes == 16 {
            memory
                .read(address.wrapping_add(8), 8)
                .map_err(|()| address.wrapping_add(8))?
        } else {
            0
        };
        Ok(u128::from(low) | u128::from(high) << 64)
    }

    pub(super) fn cache_zero<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut M,
        source: u8,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let bytes = SystemRegister::ZERO_BLOCK_BYTES;
        let address = cpu.register(source) & !(bytes - 1);
        let mut writes = [(0, 8); 8];
        for (index, write) in writes.iter_mut().enumerate() {
            write.0 = address.wrapping_add((index as u64) * 8);
        }
        let reservation = match memory.reserve_write_batch(&writes) {
            Ok(reservation) => reservation,
            Err(address) => return Self::fault(instruction, address, AccessKind::Write, 8_u8),
        };
        if memory.commit_write_batch(reservation, &[0; 8]).is_err() {
            return Self::fault(instruction, address, AccessKind::Write, bytes);
        }
        cpu.pc = instruction.wrapping_add(4);
        Aarch64ExecutionExit::Continue
    }
}
