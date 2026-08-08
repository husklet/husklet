use crate::Aarch64CpuState;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Barrier {
    DataMemory,
    DataSynchronization,
    InstructionSynchronization,
}

pub trait Port {
    fn barrier(&mut self, kind: Barrier, option: u8);

    fn invalidate_instruction(&mut self, _address: u64) {}

    fn counter_frequency(&self) -> u64;

    fn counter_value(&self) -> u64;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Register {
    CacheType,
    ZeroBlock,
    Nzcv,
    InterruptMask,
    Fpcr,
    Fpsr,
    ThreadPointer,
    ReadonlyThreadPointer,
    CounterValue,
    CounterFrequency,
}

impl Register {
    pub(crate) const ZERO_BLOCK_BYTES: u64 = 64;

    /// `None` for the counter registers: they have no value without a [`Port`],
    /// and inventing one here would be a second guest timebase.
    pub(crate) fn read_local(self, cpu: &Aarch64CpuState) -> Option<u64> {
        match self {
            Self::CacheType => Some(0x9444_c004),
            Self::ZeroBlock => Some(Self::ZERO_BLOCK_BYTES.trailing_zeros().saturating_sub(2).into()),
            Self::Nzcv => Some(u64::from(cpu.nzcv.bits())),
            Self::InterruptMask => Some(0),
            Self::Fpcr => Some(cpu.fpcr),
            Self::Fpsr => Some(cpu.fpsr),
            Self::ThreadPointer | Self::ReadonlyThreadPointer => Some(cpu.tls),
            Self::CounterValue | Self::CounterFrequency => None,
        }
    }

    pub(crate) fn write_local<S: crate::aarch64::state::ScalarAccess>(self, cpu: &mut S, value: u64) -> bool {
        match self {
            Self::Nzcv => *cpu.nzcv_mut() = crate::Nzcv::from_bits(value as u32),
            Self::InterruptMask => {}
            Self::Fpcr => cpu.set_fpcr(value & 0x07c8_0000),
            Self::Fpsr => cpu.set_fpsr(value & 0x0800_009f),
            Self::ThreadPointer => cpu.set_tls(value),
            Self::CacheType
            | Self::ZeroBlock
            | Self::ReadonlyThreadPointer
            | Self::CounterValue
            | Self::CounterFrequency => return false,
        }
        true
    }
}

mod decode;
mod execute;

pub(crate) use decode::Decoder;
pub(crate) use execute::Executor;

#[cfg(test)]
mod test;
