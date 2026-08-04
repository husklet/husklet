use crate::{CpuState, ExecutionExit, Flag, FlagState, GuestOperandMemory, ScalarInstruction, Staged, VectorLane};

pub struct Executor;

impl Executor {
    #[allow(clippy::too_many_arguments)]
    pub fn stage<M: GuestOperandMemory>(
        mut staged: CpuState,
        cpu: &CpuState,
        memory: &M,
        operation: ScalarInstruction,
        next: u64,
        instruction: u64,
    ) -> Result<Staged<M::Reservation, M::BatchReservation>, ExecutionExit> {
        match operation {
            ScalarInstruction::CarrylessMultiply {
                destination,
                source,
                control,
            } => {
                let right = VectorLane::read(cpu, memory, source, next, instruction)?;
                let left = cpu.vectors[usize::from(destination)];
                let left = (left >> (u32::from(control & 1) * 64)) as u64;
                let right = (right >> (u32::from((control >> 4) & 1) * 64)) as u64;
                staged.vectors[usize::from(destination)] = VectorLane::carryless_multiply(left, right);
            }
            ScalarInstruction::Aes {
                operation,
                destination,
                source,
            } => {
                let source = VectorLane::read(cpu, memory, source, next, instruction)?;
                let state = cpu.vectors[usize::from(destination)];
                staged.vectors[usize::from(destination)] = super::Aes::execute(state, source, operation);
            }
            ScalarInstruction::Sha {
                operation,
                destination,
                source,
            } => {
                let source = VectorLane::read(cpu, memory, source, next, instruction)?;
                let state = cpu.vectors[usize::from(destination)];
                staged.vectors[usize::from(destination)] =
                    super::Sha::execute(state, source, cpu.vectors[0], operation);
            }
            ScalarInstruction::VectorTest { left, right } => {
                let right = VectorLane::read(cpu, memory, right, next, instruction)?;
                let left = cpu.vectors[usize::from(left)];
                staged.flags = FlagState::default()
                    .with(Flag::Zero, left & right == 0)
                    .with(Flag::Carry, !left & right == 0);
            }
            ScalarInstruction::VectorExtend {
                destination,
                source,
                source_lane,
                destination_lane,
                signed,
            } => {
                let source = VectorLane::read(cpu, memory, source, next, instruction)?;
                staged.vectors[usize::from(destination)] =
                    VectorLane::extend(source, source_lane, destination_lane, signed);
            }
            ScalarInstruction::VectorBlend {
                destination,
                source,
                lane,
                selectors,
                implicit,
            } => {
                let right = VectorLane::read(cpu, memory, source, next, instruction)?;
                let left = cpu.vectors[usize::from(destination)];
                let selectors = if implicit {
                    cpu.vectors[0]
                } else {
                    u128::from(selectors)
                };
                staged.vectors[usize::from(destination)] = VectorLane::blend(left, right, selectors, lane, implicit);
            }
            ScalarInstruction::VectorHorizontalMinimum { destination, source } => {
                let source = VectorLane::read(cpu, memory, source, next, instruction)?;
                staged.vectors[usize::from(destination)] = VectorLane::horizontal_minimum(source);
            }
            ScalarInstruction::VectorSad {
                destination,
                source,
                control,
            } => {
                let right = VectorLane::read(cpu, memory, source, next, instruction)?;
                let left = cpu.vectors[usize::from(destination)];
                staged.vectors[usize::from(destination)] = VectorLane::sad(left, right, control);
            }
            ScalarInstruction::VectorDot {
                destination,
                source,
                control,
                format,
            } => {
                let right = VectorLane::read(cpu, memory, source, next, instruction)?;
                let left = cpu.vectors[usize::from(destination)];
                let (result, exceptions) = VectorLane::dot(left, right, control, format, cpu.mxcsr);
                staged.vectors[usize::from(destination)] = result;
                staged.mxcsr |= exceptions;
            }
            ScalarInstruction::VectorUnpack {
                destination,
                source,
                lane,
                high,
            } => {
                let right = VectorLane::read(cpu, memory, source, next, instruction)?;
                staged.vectors[usize::from(destination)] =
                    VectorLane::unpack(cpu.vectors[usize::from(destination)], right, lane, high);
            }
            ScalarInstruction::VectorBitwise {
                operation,
                destination,
                source,
            } => {
                let right = VectorLane::read(cpu, memory, source, next, instruction)?;
                let left = cpu.vectors[usize::from(destination)];
                staged.vectors[usize::from(destination)] = VectorLane::bitwise(left, right, operation);
            }
            ScalarInstruction::VectorByteShift { vector, left, count } => {
                staged.vectors[usize::from(vector)] =
                    VectorLane::shift_bytes(cpu.vectors[usize::from(vector)], count, left);
            }
            ScalarInstruction::VectorLaneShift {
                vector,
                lane,
                kind,
                count,
            } => {
                staged.vectors[usize::from(vector)] =
                    VectorLane::shift(cpu.vectors[usize::from(vector)], lane, count, kind);
            }
            ScalarInstruction::VectorVariableShift {
                vector,
                count,
                lane,
                kind,
            } => {
                let raw = VectorLane::read(cpu, memory, count, next, instruction)? as u64;
                staged.vectors[usize::from(vector)] = VectorLane::shift(
                    cpu.vectors[usize::from(vector)],
                    lane,
                    u8::try_from(raw.min(255)).unwrap(),
                    kind,
                );
            }
            ScalarInstruction::Ssse3 {
                operation,
                lane,
                destination,
                source,
            } => {
                let right = VectorLane::read(cpu, memory, source, next, instruction)?;
                staged.vectors[usize::from(destination)] =
                    VectorLane::ssse3(cpu.vectors[usize::from(destination)], right, lane, operation);
            }
            ScalarInstruction::VectorAlign {
                destination,
                source,
                count,
            } => {
                let right = VectorLane::read(cpu, memory, source, next, instruction)?;
                staged.vectors[usize::from(destination)] = if count == 0 {
                    right
                } else if count < 16 {
                    let bits = u32::from(count) * 8;
                    right >> bits | cpu.vectors[usize::from(destination)] << (128 - bits)
                } else if count < 32 {
                    cpu.vectors[usize::from(destination)] >> (u32::from(count - 16) * 8)
                } else {
                    0
                };
            }
            ScalarInstruction::VectorInteger {
                operation,
                destination,
                source,
                lane,
            } => {
                let right = VectorLane::read(cpu, memory, source, next, instruction)?;
                let left = cpu.vectors[usize::from(destination)];
                staged.vectors[usize::from(destination)] = VectorLane::integer(left, right, lane, operation);
            }
            ScalarInstruction::VectorShuffle {
                mode,
                destination,
                source,
                selectors,
            } => {
                let right = VectorLane::read(cpu, memory, source, next, instruction)?;
                let left = cpu.vectors[usize::from(destination)];
                staged.vectors[usize::from(destination)] = VectorLane::shuffle(left, right, selectors, mode);
            }
            ScalarInstruction::VectorByteShuffle { destination, control } => {
                let indexes = VectorLane::read(cpu, memory, control, next, instruction)?;
                staged.vectors[usize::from(destination)] =
                    VectorLane::shuffle_bytes(cpu.vectors[usize::from(destination)], indexes);
            }
            ScalarInstruction::VectorCompare {
                comparison,
                destination,
                source,
                lane,
            } => {
                let right = VectorLane::read(cpu, memory, source, next, instruction)?;
                let left = cpu.vectors[usize::from(destination)];
                staged.vectors[usize::from(destination)] = VectorLane::compare(left, right, lane, comparison);
            }
            ScalarInstruction::VectorMask {
                destination,
                source,
                lane,
            } => {
                staged = VectorLane::write_mask(staged, destination, source, lane);
            }
            ScalarInstruction::VectorInsertWord {
                destination,
                source,
                lane,
            } => {
                return VectorLane::insert_word(staged, cpu, memory, destination, source, lane, next, instruction);
            }
            _ => unreachable!(),
        }
        Ok(Staged::Cpu(staged))
    }
}
