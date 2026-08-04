use crate::{
    AccessKind, Arithmetic, CpuState, ExecutionExit, Flag, GuestOperandMemory, IntegerWidth, RepeatCondition,
    ScalarWidth, Segment, StringInstruction, StringOperation,
};

const SOURCE: usize = 6;
const DESTINATION: usize = 7;
const COUNT: usize = 1;

pub(crate) struct StringInterpreter;

impl StringInterpreter {
    pub(crate) fn execute<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        length: u8,
        width: ScalarWidth,
        instruction: StringInstruction,
        budget: u64,
    ) -> ExecutionExit {
        let origin = cpu.rip;
        let repeated = instruction.repeat != RepeatCondition::None;
        if repeated && Self::count(cpu, instruction.address_32) == 0 {
            cpu.rip = origin.wrapping_add(u64::from(length));
            return ExecutionExit::Continue;
        }
        if budget == 0 {
            return ExecutionExit::Yield {
                instruction: origin,
                completed: 0,
            };
        }
        let mut completed = 0;
        loop {
            let outcome = Self::iteration(cpu, memory, width, instruction, origin);
            if let Err(exit) = outcome {
                return exit;
            }
            completed += 1;
            if repeated {
                Self::decrement_count(cpu, instruction.address_32);
            }
            let compare_stopped = match instruction.repeat {
                RepeatCondition::WhileEqual => !cpu.flags.contains(Flag::Zero),
                RepeatCondition::WhileNotEqual => cpu.flags.contains(Flag::Zero),
                RepeatCondition::None | RepeatCondition::Count => false,
            };
            let complete = !repeated || Self::count(cpu, instruction.address_32) == 0 || compare_stopped;
            if complete {
                cpu.rip = origin.wrapping_add(u64::from(length));
                return ExecutionExit::Continue;
            }
            if completed == budget {
                return ExecutionExit::Yield {
                    instruction: origin,
                    completed,
                };
            }
        }
    }

    fn iteration<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        width: ScalarWidth,
        instruction: StringInstruction,
        origin: u64,
    ) -> Result<(), ExecutionExit> {
        let bytes = Self::bytes(width);
        let source = Self::source_address(cpu, instruction);
        let destination = Self::index(cpu, DESTINATION, instruction.address_32);
        match instruction.operation {
            StringOperation::Move => {
                let value = Self::read(memory, source, bytes, origin)?;
                let reservation = Self::reserve(memory, destination, bytes, origin)?;
                memory.commit_write(reservation, value).map_err(|_| {
                    ExecutionExit::OperandFault(crate::FaultAccess::operand(
                        origin,
                        destination,
                        AccessKind::Write,
                        u64::from(bytes),
                    ))
                })?;
                Self::advance(cpu, SOURCE, bytes, instruction.address_32);
                Self::advance(cpu, DESTINATION, bytes, instruction.address_32);
            }
            StringOperation::Store => {
                let reservation = Self::reserve(memory, destination, bytes, origin)?;
                memory
                    .commit_write(reservation, cpu.registers[0] & Self::mask(width))
                    .map_err(|_| {
                        ExecutionExit::OperandFault(crate::FaultAccess::operand(
                            origin,
                            destination,
                            AccessKind::Write,
                            u64::from(bytes),
                        ))
                    })?;
                Self::advance(cpu, DESTINATION, bytes, instruction.address_32);
            }
            StringOperation::Load => {
                let value = Self::read(memory, source, bytes, origin)?;
                Self::write_accumulator(cpu, width, value);
                Self::advance(cpu, SOURCE, bytes, instruction.address_32);
            }
            StringOperation::Compare => {
                let left = Self::read(memory, source, bytes, origin)?;
                let right = Self::read(memory, destination, bytes, origin)?;
                cpu.flags = cpu
                    .flags
                    .apply(Arithmetic::sub(Self::integer_width(width), left, right, false).flags);
                Self::advance(cpu, SOURCE, bytes, instruction.address_32);
                Self::advance(cpu, DESTINATION, bytes, instruction.address_32);
            }
            StringOperation::Scan => {
                let right = Self::read(memory, destination, bytes, origin)?;
                cpu.flags = cpu.flags.apply(
                    Arithmetic::sub(
                        Self::integer_width(width),
                        cpu.registers[0] & Self::mask(width),
                        right,
                        false,
                    )
                    .flags,
                );
                Self::advance(cpu, DESTINATION, bytes, instruction.address_32);
            }
        }
        Ok(())
    }

    fn source_address(cpu: &CpuState, instruction: StringInstruction) -> u64 {
        let source = Self::index(cpu, SOURCE, instruction.address_32);
        source.wrapping_add(match instruction.source_segment {
            Some(Segment::Fs) => cpu.fs_base,
            Some(Segment::Gs) => cpu.gs_base,
            None => 0,
        })
    }

    fn index(cpu: &CpuState, register: usize, address_32: bool) -> u64 {
        if address_32 {
            u64::from(cpu.registers[register] as u32)
        } else {
            cpu.registers[register]
        }
    }

    fn count(cpu: &CpuState, address_32: bool) -> u64 {
        Self::index(cpu, COUNT, address_32)
    }

    fn advance(cpu: &mut CpuState, register: usize, bytes: u8, address_32: bool) {
        if address_32 {
            let value = cpu.registers[register] as u32;
            cpu.registers[register] = u64::from(if cpu.direction {
                value.wrapping_sub(u32::from(bytes))
            } else {
                value.wrapping_add(u32::from(bytes))
            });
        } else if cpu.direction {
            cpu.registers[register] = cpu.registers[register].wrapping_sub(u64::from(bytes));
        } else {
            cpu.registers[register] = cpu.registers[register].wrapping_add(u64::from(bytes));
        }
    }

    fn decrement_count(cpu: &mut CpuState, address_32: bool) {
        if address_32 {
            cpu.registers[COUNT] = u64::from((cpu.registers[COUNT] as u32).wrapping_sub(1));
        } else {
            cpu.registers[COUNT] = cpu.registers[COUNT].wrapping_sub(1);
        }
    }

    fn read<M: GuestOperandMemory>(
        memory: &M,
        address: u64,
        bytes: u8,
        instruction: u64,
    ) -> Result<u64, ExecutionExit> {
        Self::canonical(address, bytes, instruction, AccessKind::Read)?;
        memory.read(address, bytes).map_err(|_| {
            ExecutionExit::OperandFault(crate::FaultAccess::operand(
                instruction,
                address,
                AccessKind::Read,
                u64::from(bytes),
            ))
        })
    }

    fn reserve<M: GuestOperandMemory>(
        memory: &M,
        address: u64,
        bytes: u8,
        instruction: u64,
    ) -> Result<M::Reservation, ExecutionExit> {
        Self::canonical(address, bytes, instruction, AccessKind::Write)?;
        memory.reserve_write(address, bytes).map_err(|_| {
            ExecutionExit::OperandFault(crate::FaultAccess::operand(
                instruction,
                address,
                AccessKind::Write,
                u64::from(bytes),
            ))
        })
    }

    fn canonical(address: u64, bytes: u8, instruction: u64, access: AccessKind) -> Result<(), ExecutionExit> {
        let end = address
            .checked_add(u64::from(bytes) - 1)
            .ok_or(ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            })?;
        if !Self::is_canonical(address) || !Self::is_canonical(end) {
            return Err(ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            });
        }
        Ok(())
    }

    const fn is_canonical(address: u64) -> bool {
        (((address << 16) as i64) >> 16) as u64 == address
    }

    fn write_accumulator(cpu: &mut CpuState, width: ScalarWidth, value: u64) {
        cpu.registers[0] = match width {
            ScalarWidth::Byte => (cpu.registers[0] & !0xff) | (value & 0xff),
            ScalarWidth::Word => (cpu.registers[0] & !0xffff) | (value & 0xffff),
            ScalarWidth::Dword => value & 0xffff_ffff,
            ScalarWidth::Qword => value,
        };
    }

    const fn bytes(width: ScalarWidth) -> u8 {
        match width {
            ScalarWidth::Byte => 1,
            ScalarWidth::Word => 2,
            ScalarWidth::Dword => 4,
            ScalarWidth::Qword => 8,
        }
    }

    const fn mask(width: ScalarWidth) -> u64 {
        match width {
            ScalarWidth::Byte => 0xff,
            ScalarWidth::Word => 0xffff,
            ScalarWidth::Dword => 0xffff_ffff,
            ScalarWidth::Qword => u64::MAX,
        }
    }

    const fn integer_width(width: ScalarWidth) -> IntegerWidth {
        match width {
            ScalarWidth::Byte => IntegerWidth::Byte,
            ScalarWidth::Word => IntegerWidth::Word,
            ScalarWidth::Dword => IntegerWidth::Dword,
            ScalarWidth::Qword => IntegerWidth::Qword,
        }
    }
}
