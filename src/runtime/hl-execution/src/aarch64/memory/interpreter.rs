use crate::{
    Aarch64CpuState, Aarch64ExecutionExit, Aarch64Instruction, Aarch64Ir, AccessKind, GuestOperandMemory,
    IndexExtension, LoadExtension, MemoryAddress, MemoryWidth, PcCoordinatePort, Writeback,
};

pub(crate) struct Interpreter;
pub(crate) type Aarch64MemoryInterpreter = Interpreter;

impl Aarch64MemoryInterpreter {
    pub(crate) fn is_memory(instruction: Aarch64Instruction) -> bool {
        matches!(
            instruction,
            Aarch64Instruction::Load { .. }
                | Aarch64Instruction::Store { .. }
                | Aarch64Instruction::VectorLoad { .. }
                | Aarch64Instruction::VectorStore { .. }
                | Aarch64Instruction::VectorLoadPair { .. }
                | Aarch64Instruction::VectorLoadGroup { .. }
                | Aarch64Instruction::VectorStorePair { .. }
                | Aarch64Instruction::VectorStoreGroup { .. }
                | Aarch64Instruction::VectorStructureGroup { .. }
                | Aarch64Instruction::VectorStructureLane { .. }
                | Aarch64Instruction::CacheZero { .. }
                | Aarch64Instruction::LoadPair { .. }
                | Aarch64Instruction::StorePair { .. }
        )
    }

    pub(crate) fn execute<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut M,
        coordinates: &dyn PcCoordinatePort,
        ir: Aarch64Ir,
    ) -> Aarch64ExecutionExit {
        match ir.instruction {
            Aarch64Instruction::Load {
                destination,
                width,
                extension,
                address,
            } => Self::load(cpu, memory, coordinates, destination, width, extension, address),
            Aarch64Instruction::Store { source, width, address } => {
                Self::store(cpu, memory, coordinates, source, width, address)
            }
            Aarch64Instruction::VectorLoad {
                destination,
                bytes,
                address,
            } => Self::vector_load(cpu, memory, coordinates, destination, bytes, address),
            Aarch64Instruction::VectorStore { source, bytes, address } => {
                Self::vector_store(cpu, memory, coordinates, source, bytes, address)
            }
            Aarch64Instruction::VectorLoadPair {
                first,
                second,
                bytes,
                address,
            } => Self::vector_load_pair(cpu, memory, coordinates, (first, second), bytes, address),
            Aarch64Instruction::VectorLoadGroup {
                first,
                count,
                bytes,
                address,
            } => Self::vector_load_group(cpu, memory, coordinates, first, count, bytes, address),
            Aarch64Instruction::VectorStorePair {
                first,
                second,
                bytes,
                address,
            } => Self::vector_store_pair(cpu, memory, coordinates, (first, second), bytes, address),
            Aarch64Instruction::VectorStoreGroup {
                first,
                count,
                bytes,
                address,
            } => Self::vector_store_group(cpu, memory, coordinates, first, count, bytes, address),
            Aarch64Instruction::VectorStructureGroup {
                first,
                count,
                lane_bits,
                load,
                wide,
                address,
            } => Self::structure_group(cpu, memory, coordinates, first, count, lane_bits, load, wide, address),
            Aarch64Instruction::VectorStructureLane {
                first,
                count,
                lane_bits,
                lane,
                load,
                replicate,
                wide,
                address,
            } => Self::structure_lane(
                cpu,
                memory,
                coordinates,
                first,
                count,
                lane_bits,
                lane,
                load,
                replicate,
                wide,
                address,
            ),
            Aarch64Instruction::CacheZero { source } => Self::cache_zero(cpu, memory, source),
            Aarch64Instruction::LoadPair {
                first,
                second,
                width,
                sign_extend,
                address,
            } => Self::load_pair(cpu, memory, coordinates, (first, second), width, sign_extend, address),
            Aarch64Instruction::StorePair {
                first,
                second,
                width,
                address,
            } => Self::store_pair(cpu, memory, coordinates, (first, second), width, address),
            _ => Aarch64ExecutionExit::UnsupportedInstruction {
                instruction: cpu.pc,
                word: ir.word,
            },
        }
    }

    fn load<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &M,
        coordinates: &dyn PcCoordinatePort,
        destination: u8,
        width: MemoryWidth,
        extension: LoadExtension,
        address: MemoryAddress,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let resolved = Self::resolve(cpu, coordinates, address);
        let Ok(value) = memory.read(resolved.address, width.bytes()) else {
            return Self::fault(instruction, resolved.address, AccessKind::Read, width.bytes());
        };
        let mut staged = cpu.clone();
        Self::write_load(&mut staged, destination, width, extension, value);
        Self::writeback(&mut staged, resolved);
        staged.pc = instruction.wrapping_add(4);
        cpu.commit_scalar(&staged);
        Aarch64ExecutionExit::Continue
    }

    fn store<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut M,
        coordinates: &dyn PcCoordinatePort,
        source: u8,
        width: MemoryWidth,
        address: MemoryAddress,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let resolved = Self::resolve(cpu, coordinates, address);
        let value = cpu.register(source);
        let Ok(reservation) = memory.reserve_write(resolved.address, width.bytes()) else {
            return Self::fault(instruction, resolved.address, AccessKind::Write, width.bytes());
        };
        if memory.commit_write(reservation, value).is_err() {
            return Self::fault(instruction, resolved.address, AccessKind::Write, width.bytes());
        }
        let mut staged = cpu.clone();
        Self::writeback(&mut staged, resolved);
        staged.pc = instruction.wrapping_add(4);
        cpu.commit_scalar(&staged);
        Aarch64ExecutionExit::Continue
    }

    fn load_pair<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &M,
        coordinates: &dyn PcCoordinatePort,
        registers: (u8, u8),
        width: MemoryWidth,
        sign_extend: bool,
        address: MemoryAddress,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let resolved = Self::resolve(cpu, coordinates, address);
        let second_address = resolved.address.wrapping_add(u64::from(width.bytes()));
        let Ok(first) = memory.read(resolved.address, width.bytes()) else {
            return Self::fault(instruction, resolved.address, AccessKind::Read, width.bytes());
        };
        let Ok(second) = memory.read(second_address, width.bytes()) else {
            return Self::fault(instruction, second_address, AccessKind::Read, width.bytes());
        };
        let mut staged = cpu.clone();
        let extension = if sign_extend {
            LoadExtension::SignTo64
        } else {
            LoadExtension::Zero
        };
        Self::write_load(&mut staged, registers.0, width, extension, first);
        Self::write_load(&mut staged, registers.1, width, extension, second);
        Self::writeback(&mut staged, resolved);
        staged.pc = instruction.wrapping_add(4);
        cpu.commit_scalar(&staged);
        Aarch64ExecutionExit::Continue
    }

    fn store_pair<M: GuestOperandMemory>(
        cpu: &mut Aarch64CpuState,
        memory: &mut M,
        coordinates: &dyn PcCoordinatePort,
        registers: (u8, u8),
        width: MemoryWidth,
        address: MemoryAddress,
    ) -> Aarch64ExecutionExit {
        let instruction = cpu.pc;
        let resolved = Self::resolve(cpu, coordinates, address);
        let second_address = resolved.address.wrapping_add(u64::from(width.bytes()));
        let values = (cpu.register(registers.0), cpu.register(registers.1));
        let writes = [(resolved.address, width.bytes()), (second_address, width.bytes())];
        let reservation = match memory.reserve_write_batch(&writes) {
            Ok(reservation) => reservation,
            Err(address) => return Self::fault(instruction, address, AccessKind::Write, width.bytes()),
        };
        if memory.commit_write_batch(reservation, &[values.0, values.1]).is_err() {
            return Self::fault(
                instruction,
                resolved.address,
                AccessKind::Write,
                u64::from(width.bytes()) * 2,
            );
        }
        let mut staged = cpu.clone();
        Self::writeback(&mut staged, resolved);
        staged.pc = instruction.wrapping_add(4);
        cpu.commit_scalar(&staged);
        Aarch64ExecutionExit::Continue
    }

    pub(super) fn resolve(
        cpu: &Aarch64CpuState,
        coordinates: &dyn PcCoordinatePort,
        address: MemoryAddress,
    ) -> ResolvedAddress {
        match address {
            MemoryAddress::PostRegister { base, index } => ResolvedAddress {
                address: cpu.register_or_sp(base),
                writeback: Some((base, cpu.register_or_sp(base).wrapping_add(cpu.register(index)))),
            },
            MemoryAddress::Literal { displacement } => ResolvedAddress {
                address: coordinates.architectural_pc(cpu.pc).wrapping_add(displacement as u64),
                writeback: None,
            },
            MemoryAddress::Base {
                register,
                displacement,
                writeback,
            } => {
                let base = cpu.register_or_sp(register);
                let adjusted = base.wrapping_add(displacement as u64);
                ResolvedAddress {
                    address: if writeback == Writeback::PostIndex {
                        base
                    } else {
                        adjusted
                    },
                    writeback: match writeback {
                        Writeback::None => None,
                        Writeback::PreIndex | Writeback::PostIndex => Some((register, adjusted)),
                    },
                }
            }
            MemoryAddress::Register {
                base,
                index,
                extension,
                shift,
            } => {
                let index = Self::extend(cpu.register(index), extension) << shift;
                ResolvedAddress {
                    address: cpu.register_or_sp(base).wrapping_add(index),
                    writeback: None,
                }
            }
        }
    }

    fn extend(value: u64, extension: IndexExtension) -> u64 {
        match extension {
            IndexExtension::Unsigned32 => u64::from(value as u32),
            IndexExtension::Unsigned64 => value,
            IndexExtension::Signed32 => (i64::from(value as i32)) as u64,
            IndexExtension::Signed64 => value,
        }
    }

    fn write_load(
        cpu: &mut Aarch64CpuState,
        destination: u8,
        width: MemoryWidth,
        extension: LoadExtension,
        value: u64,
    ) {
        match extension {
            LoadExtension::SignTo64 => {
                cpu.set_register(destination, Self::sign_extend(value, width.bytes() * 8));
            }
            LoadExtension::SignTo32 => {
                cpu.set_narrow_register(destination, Self::sign_extend(value, width.bytes() * 8) as u32);
            }
            LoadExtension::Zero if width == MemoryWidth::Double => {
                cpu.set_register(destination, value);
            }
            LoadExtension::Zero => cpu.set_narrow_register(destination, value as u32),
        }
    }

    pub(super) fn writeback(cpu: &mut Aarch64CpuState, resolved: ResolvedAddress) {
        if let Some((register, value)) = resolved.writeback {
            cpu.set_destination(register, value);
        }
    }

    fn sign_extend(value: u64, bits: u8) -> u64 {
        ((value << (64 - bits)) as i64 >> (64 - bits)) as u64
    }

    pub(super) fn fault(
        instruction: u64,
        address: u64,
        access: AccessKind,
        length: impl Into<u64>,
    ) -> Aarch64ExecutionExit {
        Aarch64ExecutionExit::OperandFault(crate::FaultAccess::operand(instruction, address, access, length.into()))
    }
}

#[derive(Clone, Copy)]
pub(super) struct ResolvedAddress {
    pub(super) address: u64,
    writeback: Option<(u8, u64)>,
}
