use hl_isa::GuestArchitecture;
use hl_task::{AlternateStack, SignalAction, SignalDisposition, SignalInfo, SignalMask, SignalNumber};
use hl_time::Timespec;

use crate::{Errno, GuestAccess, GuestMarshaller, GuestMemory, MarshalError};

const SIGNAL_SET_SIZE: usize = 8;
const SIGNAL_ACTION_SIZE: usize = 32;
const SIGNAL_INFO_SIZE: usize = 128;
const ALTERNATE_STACK_SIZE: usize = 24;
const ACTION_FLAGS: u64 = 0xdc00_0c07;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AbiError {
    Fault,
    Invalid,
    NoMemory,
    Overflow,
}

impl AbiError {
    #[must_use]
    pub const fn errno(self) -> Errno {
        match self {
            Self::Fault => Errno::EFAULT,
            Self::Invalid => Errno::EINVAL,
            Self::NoMemory => Errno::ENOMEM,
            Self::Overflow => Errno::EOVERFLOW,
        }
    }
}

impl From<MarshalError> for AbiError {
    fn from(value: MarshalError) -> Self {
        match value {
            MarshalError::Fault(_) => Self::Fault,
            MarshalError::Overflow | MarshalError::TooBig => Self::Overflow,
            MarshalError::Invalid => Self::Invalid,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MaskOperation {
    Block,
    Unblock,
    Replace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Target {
    Process(i32),
    Thread(i32),
    ProcessThread { process: i32, thread: i32 },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WaitPlan {
    pub selected: SignalMask,
    pub information: u64,
    pub timeout: Option<Timespec>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct QueuedSignal {
    pub target: i32,
    pub code: i32,
    pub info: Option<SignalInfo>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedSignalCopyout {
    destination: u64,
    bytes: Vec<u8>,
}

impl StagedSignalCopyout {
    pub fn commit<M: GuestMemory>(self, marshaller: &GuestMarshaller<'_, M>) -> Result<(), AbiError> {
        let progress = marshaller.copy_to(self.destination, &self.bytes);
        progress.fault.map_or(Ok(()), |_| Err(AbiError::Fault))
    }
}

pub struct Abi<'a, M: GuestMemory> {
    marshaller: GuestMarshaller<'a, M>,
}

impl<'a, M: GuestMemory> Abi<'a, M> {
    #[must_use]
    pub const fn new(memory: &'a M, architecture: GuestArchitecture) -> Self {
        Self {
            marshaller: GuestMarshaller::new(memory, architecture),
        }
    }

    pub fn action(
        &self,
        signal: u32,
        address: u64,
        set_size: usize,
    ) -> Result<(SignalNumber, Option<SignalAction>), AbiError> {
        Self::set_size(set_size)?;
        let signal = Self::signal(signal)?;
        if matches!(signal.get(), 9 | 19) {
            return Err(AbiError::Invalid);
        }
        if address == 0 {
            return Ok((signal, None));
        }
        let bytes = self.marshaller.copy_struct_from::<SIGNAL_ACTION_SIZE>(address)?;
        let handler = Self::word(&bytes, 0);
        let flags = Self::word(&bytes, 8);
        if flags & !ACTION_FLAGS != 0 {
            return Err(AbiError::Invalid);
        }
        let disposition = match handler {
            0 => SignalDisposition::Default,
            1 => SignalDisposition::Ignore,
            value => SignalDisposition::Handler(value),
        };
        Ok((
            signal,
            Some(SignalAction {
                disposition,
                flags,
                restorer: Self::word(&bytes, 16),
                mask: SignalMask::from_bits(Self::word(&bytes, 24)),
            }),
        ))
    }

    pub fn stage_action(&self, destination: u64, action: SignalAction) -> Result<StagedSignalCopyout, AbiError> {
        let handler = match action.disposition {
            SignalDisposition::Default => 0,
            SignalDisposition::Ignore => 1,
            SignalDisposition::Handler(value) => value,
        };
        let mut bytes = Vec::with_capacity(SIGNAL_ACTION_SIZE);
        for word in [handler, action.flags, action.restorer, action.mask.bits()] {
            bytes.extend_from_slice(&word.to_le_bytes());
        }
        self.stage(destination, bytes)
    }

    pub fn mask(
        &self,
        operation: u32,
        address: u64,
        set_size: usize,
    ) -> Result<(MaskOperation, Option<SignalMask>), AbiError> {
        Self::set_size(set_size)?;
        let operation = match operation {
            0 => MaskOperation::Block,
            1 => MaskOperation::Unblock,
            2 => MaskOperation::Replace,
            _ => return Err(AbiError::Invalid),
        };
        let mask = (address != 0).then(|| self.read_mask(address)).transpose()?;
        Ok((operation, mask))
    }

    pub fn pending(&self, destination: u64, set_size: usize) -> Result<StagedSignalCopyout, AbiError> {
        Self::set_size(set_size)?;
        self.stage(destination, vec![0; SIGNAL_SET_SIZE])
    }

    pub fn stage_mask(&self, destination: u64, mask: SignalMask) -> Result<StagedSignalCopyout, AbiError> {
        self.stage(destination, mask.bits().to_le_bytes().to_vec())
    }

    pub fn timed_wait(&self, set: u64, information: u64, timeout: u64, set_size: usize) -> Result<WaitPlan, AbiError> {
        Self::set_size(set_size)?;
        let selected = self.read_mask(set)?;
        let timeout = (timeout != 0).then(|| self.timespec(timeout)).transpose()?;
        Ok(WaitPlan {
            selected,
            information,
            timeout,
        })
    }

    pub fn suspend(&self, set: u64, set_size: usize) -> Result<SignalMask, AbiError> {
        Self::set_size(set_size)?;
        self.read_mask(set)
    }

    pub fn queued_info(&self, target: i32, signal: u32, information: u64) -> Result<QueuedSignal, AbiError> {
        let bytes = self.marshaller.copy_struct_from::<SIGNAL_INFO_SIZE>(information)?;
        let signal = Self::optional_signal(signal)?;
        let code = Self::signed(&bytes, 8);
        Ok(QueuedSignal {
            target,
            code,
            info: signal.map(|signal| SignalInfo {
                signal,
                error: Self::signed(&bytes, 4),
                code,
                sender_process: Self::unsigned(&bytes, 16),
                sender_user: Self::unsigned(&bytes, 20),
                value: Self::word(&bytes, 24),
                address: 0,
                source_tag: 0,
            }),
        })
    }

    pub fn kill(&self, process: i32, signal: u32) -> Result<(Target, Option<SignalNumber>), AbiError> {
        Ok((Target::Process(process), Self::optional_signal(signal)?))
    }

    pub fn tkill(&self, thread: i32, signal: u32) -> Result<(Target, Option<SignalNumber>), AbiError> {
        Ok((Target::Thread(thread), Self::optional_signal(signal)?))
    }

    pub fn tgkill(&self, process: i32, thread: i32, signal: u32) -> Result<(Target, Option<SignalNumber>), AbiError> {
        if process <= 0 || thread <= 0 {
            return Err(AbiError::Invalid);
        }
        Ok((
            Target::ProcessThread { process, thread },
            Self::optional_signal(signal)?,
        ))
    }

    pub fn alternate_stack(&self, address: u64) -> Result<Option<AlternateStack>, AbiError> {
        if address == 0 {
            return Ok(None);
        }
        let bytes = self.marshaller.copy_struct_from::<ALTERNATE_STACK_SIZE>(address)?;
        let pointer = Self::word(&bytes, 0);
        let flags = Self::unsigned(&bytes, 8);
        let size = Self::word(&bytes, 16);
        match flags {
            0 if size < 2048 => Err(AbiError::NoMemory),
            0 => Ok(Some(AlternateStack::Enabled { pointer, size })),
            0x8000_0000 if size < 2048 => Err(AbiError::NoMemory),
            0x8000_0000 => Ok(Some(AlternateStack::Autodisarm { pointer, size })),
            2 => Ok(Some(AlternateStack::Disabled)),
            _ => Err(AbiError::Invalid),
        }
    }

    pub fn stage_alternate_stack(
        &self,
        destination: u64,
        stack: AlternateStack,
    ) -> Result<StagedSignalCopyout, AbiError> {
        let (pointer, flags, size) = match stack {
            AlternateStack::Disabled => (0, 2_u32, 0),
            AlternateStack::Enabled { pointer, size } => (pointer, 0, size),
            AlternateStack::Autodisarm { pointer, size } => (pointer, 0x8000_0000, size),
            AlternateStack::Active { pointer, size } => (pointer, 1, size),
        };
        let mut bytes = vec![0; ALTERNATE_STACK_SIZE];
        bytes[..8].copy_from_slice(&pointer.to_le_bytes());
        bytes[8..12].copy_from_slice(&flags.to_le_bytes());
        bytes[16..24].copy_from_slice(&size.to_le_bytes());
        self.stage(destination, bytes)
    }

    pub fn stage_info(&self, destination: u64, info: SignalInfo) -> Result<StagedSignalCopyout, AbiError> {
        let mut bytes = vec![0; SIGNAL_INFO_SIZE];
        bytes[..4].copy_from_slice(&(info.signal.get() as i32).to_le_bytes());
        bytes[4..8].copy_from_slice(&info.error.to_le_bytes());
        bytes[8..12].copy_from_slice(&info.code.to_le_bytes());
        if info.signal.get() == 31 && info.code == 1 {
            bytes[16..24].copy_from_slice(&info.address.to_le_bytes());
            bytes[24..28].copy_from_slice(&(info.value as u32).to_le_bytes());
            bytes[28..32].copy_from_slice(&info.source_tag.to_le_bytes());
            return self.stage(destination, bytes);
        }
        bytes[16..20].copy_from_slice(&info.sender_process.to_le_bytes());
        bytes[20..24].copy_from_slice(&info.sender_user.to_le_bytes());
        bytes[24..32].copy_from_slice(&info.value.to_le_bytes());
        self.stage(destination, bytes)
    }

    fn timespec(&self, address: u64) -> Result<Timespec, AbiError> {
        let bytes = self.marshaller.copy_struct_from::<16>(address)?;
        let seconds = i64::from_le_bytes(bytes[..8].try_into().expect("seconds"));
        let nanoseconds = i64::from_le_bytes(bytes[8..].try_into().expect("nanoseconds"));
        if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
            return Err(AbiError::Invalid);
        }
        Timespec::new(seconds as u64, nanoseconds as u32).ok_or(AbiError::Invalid)
    }

    fn read_mask(&self, address: u64) -> Result<SignalMask, AbiError> {
        let bytes = self.marshaller.copy_struct_from::<SIGNAL_SET_SIZE>(address)?;
        Ok(SignalMask::from_bits(u64::from_le_bytes(bytes)))
    }

    fn stage(&self, destination: u64, bytes: Vec<u8>) -> Result<StagedSignalCopyout, AbiError> {
        let available = self.marshaller.probe(destination, bytes.len(), GuestAccess::Write)?;
        if available != bytes.len() {
            return Err(AbiError::Fault);
        }
        Ok(StagedSignalCopyout { destination, bytes })
    }

    const fn set_size(size: usize) -> Result<(), AbiError> {
        if size == SIGNAL_SET_SIZE {
            Ok(())
        } else {
            Err(AbiError::Invalid)
        }
    }

    fn signal(raw: u32) -> Result<SignalNumber, AbiError> {
        u8::try_from(raw)
            .ok()
            .and_then(|value| SignalNumber::new(value).ok())
            .ok_or(AbiError::Invalid)
    }

    fn optional_signal(raw: u32) -> Result<Option<SignalNumber>, AbiError> {
        if raw == 0 {
            Ok(None)
        } else {
            Self::signal(raw).map(Some)
        }
    }

    fn word(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("word"))
    }

    fn signed(bytes: &[u8], offset: usize) -> i32 {
        i32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("signed"))
    }

    fn unsigned(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("unsigned"))
    }
}
