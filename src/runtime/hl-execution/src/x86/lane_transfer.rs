use crate::{
    AccessKind, CpuState, ExecutionExit, GuestOperandMemory, ScalarOperand, ScalarRegister, ScalarWidth, VectorSource,
};

pub(crate) struct LaneTransfer;

impl LaneTransfer {
    pub(crate) fn vex_insert_single<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        destination: u8,
        first: u8,
        second: VectorSource,
        control: u8,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let value = match second {
            VectorSource::Register(register) => {
                let lane = u32::from(control >> 6 & 3) * 32;
                (cpu.vectors[usize::from(register)] >> lane) as u32
            }
            VectorSource::Memory(effective) => {
                let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                match memory.read(address, 4) {
                    Ok(value) => value as u32,
                    Err(()) => return Self::fault(instruction, address, AccessKind::Read, 4),
                }
            }
        };
        let mut result = cpu.vectors[usize::from(first)];
        let destination_lane = u32::from(control >> 4 & 3) * 32;
        result = (result & !(u128::from(u32::MAX) << destination_lane)) | (u128::from(value) << destination_lane);
        for lane in 0..4_u32 {
            if control & (1 << lane) != 0 {
                result &= !(u128::from(u32::MAX) << (lane * 32));
            }
        }
        cpu.vectors[usize::from(destination)] = result;
        cpu.vector_upper[usize::from(destination)] = 0;
        cpu.rip = next;
        ExecutionExit::Continue
    }

    pub(crate) fn insert_single<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        destination: u8,
        source: VectorSource,
        control: u8,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let value = match source {
            VectorSource::Register(register) => {
                let lane = u32::from(control >> 6 & 3) * 32;
                (cpu.vectors[usize::from(register)] >> lane) as u32
            }
            VectorSource::Memory(effective) => {
                let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                match memory.read(address, 4) {
                    Ok(value) => value as u32,
                    Err(()) => return Self::fault(instruction, address, AccessKind::Read, 4),
                }
            }
        };
        let mut result = cpu.vectors[usize::from(destination)];
        let destination_lane = u32::from(control >> 4 & 3) * 32;
        result = (result & !(u128::from(u32::MAX) << destination_lane)) | (u128::from(value) << destination_lane);
        for lane in 0..4_u32 {
            if control & (1 << lane) != 0 {
                result &= !(u128::from(u32::MAX) << (lane * 32));
            }
        }
        cpu.vectors[usize::from(destination)] = result;
        cpu.rip = next;
        ExecutionExit::Continue
    }

    pub(crate) fn insert<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &M,
        destination: u8,
        source: ScalarOperand,
        bytes: u8,
        lane: u8,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let value = match source {
            ScalarOperand::Register(register) => cpu.read_register(register, Self::width(bytes)),
            ScalarOperand::Memory(effective) => {
                let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                match memory.read(address, bytes) {
                    Ok(value) => value,
                    Err(()) => return Self::fault(instruction, address, AccessKind::Read, bytes),
                }
            }
            ScalarOperand::Immediate(_) => unreachable!(),
        };
        let shift = u32::from(lane) * u32::from(bytes) * 8;
        let mask = u128::from(Self::mask(bytes)) << shift;
        cpu.vectors[usize::from(destination)] =
            (cpu.vectors[usize::from(destination)] & !mask) | (u128::from(value & Self::mask(bytes)) << shift);
        cpu.rip = next;
        ExecutionExit::Continue
    }

    pub(crate) fn extract<M: GuestOperandMemory>(
        cpu: &mut CpuState,
        memory: &mut M,
        source: u8,
        destination: ScalarOperand,
        bytes: u8,
        lane: u8,
        instruction: u64,
        next: u64,
    ) -> ExecutionExit {
        let shift = u32::from(lane) * u32::from(bytes) * 8;
        let value = (cpu.vectors[usize::from(source)] >> shift) as u64 & Self::mask(bytes);
        match destination {
            ScalarOperand::Register(ScalarRegister::General(register)) => {
                cpu.write_register(ScalarRegister::General(register), Self::width(bytes.max(4)), value);
                cpu.rip = next;
            }
            ScalarOperand::Memory(effective) => {
                let address = effective.resolve(&cpu.registers, next, cpu.fs_base, cpu.gs_base);
                let Ok(reservation) = memory.reserve_write(address, bytes) else {
                    return Self::fault(instruction, address, AccessKind::Write, bytes)
                };
                if memory.commit_write(reservation, value).is_err() {
                    return Self::fault(instruction, address, AccessKind::Write, bytes);
                }
                cpu.rip = next;
            }
            _ => unreachable!(),
        }
        ExecutionExit::Continue
    }

    const fn width(bytes: u8) -> ScalarWidth {
        match bytes {
            1 => ScalarWidth::Byte,
            2 => ScalarWidth::Word,
            4 => ScalarWidth::Dword,
            8 => ScalarWidth::Qword,
            _ => unreachable!(),
        }
    }
    const fn mask(bytes: u8) -> u64 {
        match bytes {
            1 => 0xff,
            2 => 0xffff,
            4 => 0xffff_ffff,
            8 => u64::MAX,
            _ => unreachable!(),
        }
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
