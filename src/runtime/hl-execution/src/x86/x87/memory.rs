use crate::x86::real::Conversion;
use crate::{
    AccessKind, CpuState, EffectiveAddress, ExecutionExit, ExtendedClass, ExtendedReal, FloatWidth, GuestOperandMemory,
};

pub(crate) struct ExtendedMemory;

impl ExtendedMemory {
    pub(crate) fn float<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        format: FloatWidth,
        store: bool,
        pop: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let bytes = match format {
            FloatWidth::Single => 4,
            FloatWidth::Double => 8,
        };
        let access = if store { AccessKind::Write } else { AccessKind::Read };
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(bytes - 1)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            };
        }
        if store {
            Self::store_float(cpu, memory, address, format, pop, instruction, next)
        } else {
            Self::load_float(cpu, memory, address, format, instruction, next)
        }
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
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(9)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            };
        }
        if load {
            Self::load(cpu, memory, address, instruction, next)
        } else {
            Self::store(cpu, memory, address, instruction, next)
        }
    }

    fn load<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        address: u64,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let Ok(low) = memory.read(address, 8) else {
            return Self::fault(instruction, address, AccessKind::Read, 10);
        };
        let Ok(high) = memory.read(address + 8, 2) else {
            return Self::fault(instruction, address + 8, AccessKind::Read, 10);
        };
        let source = ExtendedReal::from_bits(u128::from(low) | u128::from(high) << 64);
        let mut staged = cpu.clone();
        let index = usize::from(((staged.x87_status >> 11) as u8).wrapping_sub(1) & 7);
        if staged.x87_classes[index] != ExtendedClass::Empty {
            return Self::stack_fault(cpu, staged, index, instruction, next, true);
        }
        let (value, class, exception) = match source.class() {
            ExtendedClass::Denormal => (source, ExtendedClass::Denormal, Some(1_u16 << 1)),
            ExtendedClass::SignalingNan => {
                let quiet = ExtendedReal::from_bits(source.bits() | (1_u128 << 62));
                (quiet, ExtendedClass::QuietNan, Some(1))
            }
            ExtendedClass::Unsupported => (ExtendedReal::INDEFINITE, ExtendedClass::QuietNan, Some(1)),
            class => (source, class, None),
        };
        if let Some(flag) = exception {
            staged.x87_status |= flag;
            if staged.x87_control & flag == 0 {
                staged.x87_status |= (1 << 7) | (1 << 15);
                *cpu = staged;
                return ExecutionExit::UndefinedInstruction { instruction };
            }
        }
        staged.x87_status = (staged.x87_status & !0x3800) | (index as u16) << 11;
        staged.x87_values[index] = value;
        staged.x87_classes[index] = class;
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn store<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        address: u64,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let index = usize::from((cpu.x87_status >> 11) & 7);
        let (value, empty) = if cpu.x87_classes[index] == ExtendedClass::Empty {
            (ExtendedReal::INDEFINITE, true)
        } else {
            (cpu.x87_values[index], false)
        };
        if empty && cpu.x87_control & 1 == 0 {
            let mut staged = cpu.clone();
            Self::raise_stack(&mut staged, false);
            *cpu = staged;
            return ExecutionExit::UndefinedInstruction { instruction };
        }
        let writes = [(address, 8), (address + 8, 2)];
        let reservation = match memory.reserve_write_batch(&writes) {
            Ok(value) => value,
            Err(fault) => return Self::fault(instruction, fault, AccessKind::Write, 10),
        };
        let bits = value.bits();
        if memory
            .commit_write_batch(reservation, &[bits as u64, (bits >> 64) as u64])
            .is_err()
        {
            return Self::fault(instruction, address, AccessKind::Write, 10);
        }
        let mut staged = cpu.clone();
        if empty {
            Self::raise_stack(&mut staged, false);
        }
        staged.x87_classes[index] = ExtendedClass::Empty;
        staged.x87_status = (staged.x87_status & !0x3800) | (((index + 1) & 7) as u16) << 11;
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn load_float<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        address: u64,
        format: FloatWidth,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let bytes = match format {
            FloatWidth::Single => 4,
            FloatWidth::Double => 8,
        };
        let Ok(bits) = memory.read(address, bytes) else {
            return Self::fault(instruction, address, AccessKind::Read, bytes);
        };
        let (mut value, mut class) = Conversion::expand(bits, format);
        let mut staged = cpu.clone();
        let index = usize::from(((staged.x87_status >> 11) as u8).wrapping_sub(1) & 7);
        if staged.x87_classes[index] != ExtendedClass::Empty {
            return Self::stack_fault(cpu, staged, index, instruction, next, true);
        }
        let flag = match class {
            ExtendedClass::Denormal => 1 << 1,
            ExtendedClass::SignalingNan | ExtendedClass::Unsupported => 1,
            _ => 0,
        };
        if class == ExtendedClass::SignalingNan {
            value = ExtendedReal::from_bits(value.bits() | (1_u128 << 62));
            class = ExtendedClass::QuietNan;
        } else if class == ExtendedClass::Unsupported {
            value = ExtendedReal::INDEFINITE;
            class = ExtendedClass::QuietNan;
        }
        if flag != 0 && Self::raise(&mut staged, flag) {
            *cpu = staged;
            return ExecutionExit::UndefinedInstruction { instruction };
        }
        staged.x87_status = (staged.x87_status & !0x3800) | (index as u16) << 11;
        staged.x87_values[index] = value;
        staged.x87_classes[index] = class;
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }

    fn store_float<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        address: u64,
        format: FloatWidth,
        pop: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let index = usize::from((cpu.x87_status >> 11) & 7);
        let empty = cpu.x87_classes[index] == ExtendedClass::Empty;
        let bytes = match format {
            FloatWidth::Single => 4,
            FloatWidth::Double => 8,
        };
        let Ok(reservation) = memory.reserve_write(address, bytes) else {
            return Self::fault(instruction, address, AccessKind::Write, bytes);
        };
        let mut staged = cpu.clone();
        let converted = if empty {
            if Self::raise_stack(&mut staged, false) {
                *cpu = staged;
                return ExecutionExit::UndefinedInstruction { instruction };
            }
            Conversion::indefinite(format)
        } else {
            let result = Conversion::narrow(
                cpu.x87_values[index],
                cpu.x87_classes[index],
                format,
                (cpu.x87_control >> 10) & 3,
            );
            if result.flags != 0 && Self::raise(&mut staged, result.flags) {
                *cpu = staged;
                return ExecutionExit::UndefinedInstruction { instruction };
            }
            result.bits
        };
        if memory.commit_write(reservation, converted).is_err() {
            return Self::fault(instruction, address, AccessKind::Write, bytes);
        }
        if pop {
            staged.x87_classes[index] = ExtendedClass::Empty;
            staged.x87_status = (staged.x87_status & !0x3800) | (((index + 1) & 7) as u16) << 11;
        }
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }
}
