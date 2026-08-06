use super::interpreter::Interpreter;
use crate::{Aarch64CpuState, Aarch64ExecutionExit, AccessKind, GuestOperandMemory, MemoryAddress, PcCoordinatePort};

impl Interpreter {
    pub(super) fn structure_group<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut M,
        coordinates: &dyn PcCoordinatePort,
        first: u8,
        count: u8,
        lane_bits: u8,
        load: bool,
        wide: bool,
        address: MemoryAddress,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let resolved = Self::resolve(cpu, coordinates, address);
        let element_bytes = lane_bits / 8;
        let lanes = (if wide { 128 } else { 64 }) / lane_bits;
        let elements = usize::from(count) * usize::from(lanes);
        if load {
            let mut vectors = [0_u128; 4];
            for slot in 0..elements {
                let current = resolved.address.wrapping_add(slot as u64 * u64::from(element_bytes));
                let Ok(value) = memory.read(current, element_bytes) else {
                    return Self::fault(instruction, current, AccessKind::Read, element_bytes)
                };
                let register = slot % usize::from(count);
                let lane = slot / usize::from(count);
                vectors[register] |= u128::from(value) << (lane * usize::from(lane_bits));
            }
            let mut staged = cpu.clone();
            for index in 0..count {
                staged.set_vector(first.wrapping_add(index) & 31, vectors[index as usize]);
            }
            Self::writeback(&mut staged, resolved);
            staged.pc = instruction.wrapping_add(4);
            *cpu = staged;
            return Aarch64ExecutionExit::Continue;
        }

        let mut writes = [(0_u64, 0_u8); 64];
        let mut values = [0_u64; 64];
        for slot in 0..elements {
            writes[slot] = (
                resolved.address.wrapping_add(slot as u64 * u64::from(element_bytes)),
                element_bytes,
            );
            let register = first.wrapping_add((slot % usize::from(count)) as u8) & 31;
            let lane = (slot / usize::from(count)) as u8;
            values[slot] = cpu.vector_lane(register, lane_bits, lane);
        }
        let reservation = match memory.reserve_write_batch(&writes[..elements]) {
            Ok(value) => value,
            Err(address) => return Self::fault(instruction, address, AccessKind::Write, element_bytes),
        };
        if memory.commit_write_batch(reservation, &values[..elements]).is_err() {
            return Self::fault(
                instruction,
                resolved.address,
                AccessKind::Write,
                u64::from(element_bytes) * elements as u64,
            );
        }
        let mut staged = cpu.clone();
        Self::writeback(&mut staged, resolved);
        staged.pc = instruction.wrapping_add(4);
        *cpu = staged;
        Aarch64ExecutionExit::Continue
    }

    pub(super) fn structure_lane<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut M,
        coordinates: &dyn PcCoordinatePort,
        first: u8,
        count: u8,
        lane_bits: u8,
        lane: u8,
        load: bool,
        replicate: bool,
        wide: bool,
        address: MemoryAddress,
    ) -> Aarch64ExecutionExit {
        if load {
            Self::structure_load(
                cpu,
                memory,
                coordinates,
                first,
                count,
                lane_bits,
                lane,
                replicate,
                wide,
                address,
            )
        } else {
            Self::structure_store(cpu, memory, coordinates, first, count, lane_bits, lane, address)
        }
    }

    fn structure_load<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &M,
        coordinates: &dyn PcCoordinatePort,
        first: u8,
        count: u8,
        lane_bits: u8,
        lane: u8,
        replicate: bool,
        wide: bool,
        address: MemoryAddress,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let resolved = Self::resolve(cpu, coordinates, address);
        let bytes = lane_bits / 8;
        let mut values = [0_u64; 4];
        for index in 0..count {
            let current = resolved.address.wrapping_add(u64::from(index * bytes));
            values[index as usize] = match memory.read(current, bytes) {
                Ok(value) => value,
                Err(()) => return Self::fault(instruction, current, AccessKind::Read, bytes),
            };
        }
        let mut staged = cpu.clone();
        for index in 0..count {
            let register = first.wrapping_add(index) & 31;
            Self::write_structure(
                &mut staged,
                register,
                lane_bits,
                lane,
                values[index as usize],
                replicate,
                wide,
            );
        }
        Self::writeback(&mut staged, resolved);
        staged.pc = instruction.wrapping_add(4);
        *cpu = staged;
        Aarch64ExecutionExit::Continue
    }

    fn write_structure(
        cpu: &mut Aarch64CpuState,
        register: u8,
        lane_bits: u8,
        lane: u8,
        value: u64,
        replicate: bool,
        wide: bool,
    ) {
        if !replicate {
            cpu.set_vector_lane(register, lane_bits, lane, value);
            return;
        }
        let lanes = if wide { 128 } else { 64 } / lane_bits;
        let repeated = (0..lanes).fold(0_u128, |all, lane| all | u128::from(value) << (lane * lane_bits));
        cpu.set_vector(register, repeated);
    }

    fn structure_store<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut M,
        coordinates: &dyn PcCoordinatePort,
        first: u8,
        count: u8,
        lane_bits: u8,
        lane: u8,
        address: MemoryAddress,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let resolved = Self::resolve(cpu, coordinates, address);
        let bytes = lane_bits / 8;
        let mut writes = [(0_u64, 0_u8); 4];
        let mut values = [0_u64; 4];
        for index in 0..count {
            writes[index as usize] = (resolved.address.wrapping_add(u64::from(index * bytes)), bytes);
            values[index as usize] = cpu.vector_lane(first.wrapping_add(index) & 31, lane_bits, lane);
        }
        let reservation = match memory.reserve_write_batch(&writes[..count as usize]) {
            Ok(value) => value,
            Err(address) => return Self::fault(instruction, address, AccessKind::Write, bytes),
        };
        if memory
            .commit_write_batch(reservation, &values[..count as usize])
            .is_err()
        {
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
}
