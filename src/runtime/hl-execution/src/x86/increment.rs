use crate::{
    AccessKind, Arithmetic, AtomicOperation, CpuState, ExclusiveMemory, ExecutionExit, Flag, GuestOperandMemory,
    IntegerWidth, MemoryOrder, ScalarOperand, ScalarWidth,
};

pub(crate) struct Increment;

impl Increment {
    pub(crate) fn execute<M: GuestOperandMemory + ExclusiveMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        operand: ScalarOperand,
        decrement: bool,
        locked: bool,
        width: ScalarWidth,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        match operand {
            ScalarOperand::Register(register) => {
                let old = cpu.read_register(register, width);
                let (result, flags) = Self::result(cpu, old, decrement, width);
                let mut staged = cpu.clone();
                staged.rip = next;
                staged.flags = flags;
                staged.write_register(register, width, result);
                *cpu = staged;
                ExecutionExit::Continue
            }
            ScalarOperand::Memory(effective) => {
                let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                if locked {
                    return Self::locked(cpu, memory, address, decrement, width, instruction, next);
                }
                let bytes = Self::bytes(width);
                if let Err(exit) = Self::validate(address, bytes, instruction, AccessKind::Read) {
                    return exit;
                }
                let old = match memory.read(address, bytes) {
                    Ok(value) => value,
                    Err(()) => return Self::fault(instruction, address, AccessKind::Read, bytes),
                };
                let (result, flags) = Self::result(cpu, old, decrement, width);
                let reservation = match memory.reserve_write(address, bytes) {
                    Ok(value) => value,
                    Err(()) => return Self::fault(instruction, address, AccessKind::Write, bytes),
                };
                if memory.commit_write(reservation, result).is_err() {
                    return Self::fault(instruction, address, AccessKind::Write, bytes);
                }
                let mut staged = cpu.clone();
                staged.rip = next;
                staged.flags = flags;
                *cpu = staged;
                ExecutionExit::Continue
            }
            ScalarOperand::Immediate(_) => unreachable!(),
        }
    }

    fn locked<M: ExclusiveMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        address: u64,
        decrement: bool,
        width: ScalarWidth,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let bytes = Self::bytes(width);
        if let Err(exit) = Self::validate(address, bytes, instruction, AccessKind::Write) {
            return exit;
        }
        let operand = if decrement { Self::mask(width) } else { 1 };
        let old = match memory.fetch_update(
            address,
            bytes,
            AtomicOperation::Add,
            operand,
            MemoryOrder::SequentiallyConsistent,
        ) {
            Ok(value) => value & Self::mask(width),
            Err(()) => return Self::fault(instruction, address, AccessKind::Write, bytes),
        };
        let (_, flags) = Self::result(cpu, old, decrement, width);
        let mut staged = cpu.clone();
        staged.rip = next;
        staged.flags = flags;
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn result(cpu: &CpuState, old: u64, decrement: bool, width: ScalarWidth) -> (u64, crate::FlagState) {
        let arithmetic = if decrement {
            Arithmetic::sub(Self::integer_width(width), old, 1, false)
        } else {
            Arithmetic::add(Self::integer_width(width), old, 1, false)
        };
        let carry = cpu.flags.contains(Flag::Carry);
        (
            arithmetic.result,
            cpu.flags.apply(arithmetic.flags).with(Flag::Carry, carry),
        )
    }

    fn validate(address: u64, bytes: u8, instruction: u64, access: AccessKind) -> Result<(), ExecutionExit> {
        let end = address.checked_add(u64::from(bytes) - 1);
        if Self::canonical(address) && end.is_some_and(Self::canonical) {
            Ok(())
        } else {
            Err(ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            })
        }
    }

    const fn canonical(address: u64) -> bool {
        (((address << 16) as i64) >> 16) as u64 == address
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
    fn fault(instruction: u64, address: u64, access: AccessKind, length: u8) -> ExecutionExit {
        ExecutionExit::OperandFault(crate::FaultAccess::operand(
            instruction,
            address,
            access,
            u64::from(length),
        ))
    }
}
