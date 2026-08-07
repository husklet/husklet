use crate::x86::real::Conversion;
use crate::{
    AccessKind, CpuState, EffectiveAddress, ExecutionExit, ExtendedClass, ExtendedReal, FloatWidth, GuestOperandMemory,
};

use super::memory::ExtendedMemory;

impl ExtendedMemory {
    pub(crate) fn save<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        load: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let access = if load { AccessKind::Read } else { AccessKind::Write };
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(107)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            };
        }
        if load {
            let mut environment = [0_u64; 7];
            for (index, value) in environment.iter_mut().enumerate() {
                let field = address + (index as u64) * 4;
                *value = match memory.read(field, 4) {
                    Ok(value) => value,
                    Err(()) => return Self::fault(instruction, field, access, 108),
                };
            }
            let mut registers = [ExtendedReal::from_bits(0); 8];
            for (index, value) in registers.iter_mut().enumerate() {
                let field = address + 28 + (index as u64) * 10;
                let Ok(low) = memory.read(field, 8) else { return Self::fault(instruction, field, access, 108) };
                let Ok(high) = memory.read(field + 8, 2) else {
                    return Self::fault(instruction, field + 8, access, 108)
                };
                *value = ExtendedReal::from_bits(u128::from(low) | u128::from(high) << 64);
            }
            let mut staged = cpu.clone();
            staged.x87_control = environment[0] as u16 & 0x1f3f | 0x0040;
            staged.x87_status = environment[1] as u16;
            let tags = environment[2] as u16;
            let top = usize::from((staged.x87_status >> 11) & 7);
            for logical in 0..8 {
                let physical = (top + logical) & 7;
                staged.x87_values[physical] = registers[logical];
                staged.x87_classes[physical] = if tags >> (physical * 2) & 3 == 3 {
                    ExtendedClass::Empty
                } else {
                    registers[logical].class()
                };
            }
            staged.rip = next;
            *cpu = staged;
            return ExecutionExit::Continue;
        }
        let top = usize::from((cpu.x87_status >> 11) & 7);
        let mut tags = 0_u16;
        for (physical, class) in cpu.x87_classes.iter().copied().enumerate() {
            let tag = match class {
                ExtendedClass::Normal => 0,
                ExtendedClass::Zero => 1,
                ExtendedClass::Empty => 3,
                _ => 2,
            };
            tags |= tag << (physical * 2);
        }
        let mut writes = Vec::with_capacity(23);
        let mut values = Vec::with_capacity(23);
        for (index, value) in [
            0xffff_0000 | u64::from(cpu.x87_control),
            0xffff_0000 | u64::from(cpu.x87_status),
            0xffff_0000 | u64::from(tags),
            0,
            0,
            0,
            0xffff_0000,
        ]
        .into_iter()
        .enumerate()
        {
            writes.push((address + (index as u64) * 4, 4));
            values.push(value);
        }
        for logical in 0..8 {
            let bits = cpu.x87_values[(top + logical) & 7].bits();
            let field = address + 28 + (logical as u64) * 10;
            writes.push((field, 8));
            values.push(bits as u64);
            writes.push((field + 8, 2));
            values.push((bits >> 64) as u64);
        }
        let reservation = match memory.reserve_write_batch(&writes) {
            Ok(value) => value,
            Err(fault) => return Self::fault(instruction, fault, access, 108),
        };
        if memory.commit_write_batch(reservation, &values).is_err() {
            return Self::fault(instruction, address, access, 108);
        }
        cpu.x87_control = 0x037f;
        cpu.x87_status = 0;
        cpu.x87_classes.fill(ExtendedClass::Empty);
        cpu.rip = next;
        ExecutionExit::Continue
    }

    // FPREM's round-to-even tie-break is specified as an exact comparison against half the divisor.
    #[allow(clippy::float_cmp)]

    pub(crate) fn environment<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        load: bool,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        let access = if load { AccessKind::Read } else { AccessKind::Write };
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(27)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access,
            };
        }
        if load {
            let mut values = [0_u64; 7];
            for (index, value) in values.iter_mut().enumerate() {
                let field = address + (index as u64) * 4;
                *value = match memory.read(field, 4) {
                    Ok(value) => value,
                    Err(()) => return Self::fault(instruction, field, access, 28),
                };
            }
            let mut staged = cpu.clone();
            staged.x87_control = values[0] as u16 & 0x1f3f | 0x0040;
            staged.x87_status = values[1] as u16;
            let tags = values[2] as u16;
            for physical in 0..8 {
                staged.x87_classes[physical] = if tags >> (physical * 2) & 3 == 3 {
                    ExtendedClass::Empty
                } else {
                    staged.x87_values[physical].class()
                };
            }
            staged.rip = next;
            *cpu = staged;
            return ExecutionExit::Continue;
        }
        let mut tags = 0_u16;
        for (physical, class) in cpu.x87_classes.iter().copied().enumerate() {
            let tag = match class {
                ExtendedClass::Normal => 0,
                ExtendedClass::Zero => 1,
                ExtendedClass::Empty => 3,
                _ => 2,
            };
            tags |= tag << (physical * 2);
        }
        let values = [
            0xffff_0000 | u64::from(cpu.x87_control),
            0xffff_0000 | u64::from(cpu.x87_status),
            0xffff_0000 | u64::from(tags),
            0,
            0,
            0,
            0xffff_0000,
        ];
        let writes: [(u64, u8); 7] = std::array::from_fn(|index| (address + (index as u64) * 4, 4));
        let reservation = match memory.reserve_write_batch(&writes) {
            Ok(value) => value,
            Err(fault) => return Self::fault(instruction, fault, access, 28),
        };
        if memory.commit_write_batch(reservation, &values).is_err() {
            return Self::fault(instruction, address, access, 28);
        }
        cpu.x87_control |= 0x3f;
        cpu.rip = next;
        ExecutionExit::Continue
    }

    pub(crate) fn initialize(cpu: &mut CpuState, next: u64) -> ExecutionExit {
        cpu.x87_control = 0x037f;
        cpu.x87_status = 0;
        cpu.x87_classes.fill(ExtendedClass::Empty);
        cpu.rip = next;
        ExecutionExit::Continue
    }

    pub(crate) fn status(cpu: &mut CpuState, next: u64) -> ExecutionExit {
        let status = u64::from(cpu.x87_status);
        cpu.write_register(crate::ScalarRegister::General(0), crate::ScalarWidth::Word, status);
        cpu.rip = next;
        ExecutionExit::Continue
    }

    pub(crate) fn store_status<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        effective: EffectiveAddress,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
        if !Self::canonical(address) || !Self::canonical(address.wrapping_add(1)) {
            return ExecutionExit::NonCanonical {
                instruction,
                address,
                access: AccessKind::Write,
            };
        }
        let Ok(reservation) = memory.reserve_write(address, 2) else {
            return Self::fault(instruction, address, AccessKind::Write, 2)
        };
        if memory.commit_write(reservation, u64::from(cpu.x87_status)).is_err() {
            return Self::fault(instruction, address, AccessKind::Write, 2);
        }
        cpu.rip = next;
        ExecutionExit::Continue
    }

    pub(crate) fn constant(cpu: &mut CpuState, constant: u8, instruction: u64, next: u64) -> ExecutionExit {
        const BITS: [u64; 7] = [
            0x3ff0_0000_0000_0000,
            0x400a_934f_0979_a371,
            0x3ff7_1547_652b_82fe,
            0x4009_21fb_5444_2d18,
            0x3fd3_4413_509f_79ff,
            0x3fe6_2e42_fefa_39ef,
            0,
        ];
        let (value, class) = Conversion::expand(BITS[usize::from(constant)], FloatWidth::Double);
        let mut staged = cpu.clone();
        let destination = usize::from(((staged.x87_status >> 11) as u8).wrapping_sub(1) & 7);
        if staged.x87_classes[destination] != ExtendedClass::Empty {
            return Self::stack_fault(cpu, staged, destination, instruction, next, true);
        }
        staged.x87_status = (staged.x87_status & !0x3800) | (destination as u16) << 11;
        staged.x87_values[destination] = value;
        staged.x87_classes[destination] = class;
        staged.rip = next;
        *cpu = staged;
        ExecutionExit::Continue
    }
}
