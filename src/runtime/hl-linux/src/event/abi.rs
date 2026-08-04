use hl_event::{
    EpollEvent, EpollInterest, EventFdFlags, InotifyMask, SignalFdFlags, SignalMask, TimerFdClock, TimerFdCreateFlags,
    TimerFdSetFlags, TimerSetting,
};
use hl_descriptor::Readiness;
use hl_isa::GuestArchitecture;
use hl_time::Duration;

use crate::{GuestAccess, GuestMarshaller, GuestMemory, MarshalError};

const CLOSE_ON_EXEC: u32 = 0x8_0000;
const NONBLOCKING: u32 = 0x800;
const EPOLL_X86_SIZE: usize = 12;
const EPOLL_AARCH_SIZE: usize = 16;
const EPOLL_MAXIMUM: usize = 256;
const SIGNAL_SET_SIZE: usize = 8;
const TIMESPEC_SIZE: usize = 16;
const ITIMERSPEC_SIZE: usize = 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Marshal(MarshalError),
    Invalid,
    Overflow,
}

impl From<MarshalError> for Error {
    fn from(error: MarshalError) -> Self {
        Self::Marshal(error)
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CreationFlags {
    pub close_on_exec: bool,
    pub nonblocking: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EpollOperation {
    Add,
    Delete,
    Modify,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EpollControlPlan {
    pub operation: EpollOperation,
    pub descriptor: i32,
    pub interests: Option<EpollInterest>,
    pub data: Option<u64>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EpollWaitPlan {
    pub output: u64,
    pub maximum: usize,
    pub timeout_nanoseconds: Option<u64>,
    pub signal_mask: Option<SignalMask>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerSetPlan {
    pub flags: TimerFdSetFlags,
    pub setting: TimerSetting,
    pub old_value: Option<u64>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InotifyWatchPlan {
    pub path: Vec<u8>,
    pub mask: InotifyMask,
}

#[derive(Clone, Debug, Eq, PartialEq)]
struct CopyoutEntry {
    address: u64,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedEventCopyout {
    writes: Vec<CopyoutEntry>,
}

impl StagedEventCopyout {
    pub fn commit<M: GuestMemory>(self, marshaller: &GuestMarshaller<'_, M>) -> Result<(), Error> {
        for write in self.writes {
            let progress = marshaller.copy_to(write.address, &write.bytes);
            if let Some(fault) = progress.fault {
                return Err(MarshalError::Fault(fault).into());
            }
        }
        Ok(())
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

    pub fn epoll_create1(flags: u32) -> Result<CreationFlags, Error> {
        if flags & !CLOSE_ON_EXEC != 0 {
            return Err(Error::Invalid);
        }
        Ok(CreationFlags {
            close_on_exec: flags & CLOSE_ON_EXEC != 0,
            nonblocking: false,
        })
    }

    pub fn eventfd2(initial: u32, flags: u32) -> Result<(u64, EventFdFlags, CreationFlags), Error> {
        let allowed = 1 | NONBLOCKING | CLOSE_ON_EXEC;
        if flags & !allowed != 0 {
            return Err(Error::Invalid);
        }
        let neutral = (if flags & 1 != 0 { EventFdFlags::SEMAPHORE } else { 0 })
            | (if flags & NONBLOCKING != 0 {
                EventFdFlags::NONBLOCKING
            } else {
                0
            });
        Ok((
            initial as u64,
            EventFdFlags::from_bits(neutral),
            CreationFlags {
                close_on_exec: flags & CLOSE_ON_EXEC != 0,
                nonblocking: flags & NONBLOCKING != 0,
            },
        ))
    }

    pub fn epoll_control(
        &self,
        operation: i32,
        descriptor: i32,
        event_pointer: u64,
    ) -> Result<EpollControlPlan, Error> {
        let operation = match operation {
            1 => EpollOperation::Add,
            2 => EpollOperation::Delete,
            3 => EpollOperation::Modify,
            _ => return Err(Error::Invalid),
        };
        if operation == EpollOperation::Delete {
            return Ok(EpollControlPlan {
                operation,
                descriptor,
                interests: None,
                data: None,
            });
        }
        let (interest_bits, data) = match self.marshaller.architecture() {
            GuestArchitecture::Aarch64 => {
                let bytes = self.marshaller.copy_struct_from::<EPOLL_AARCH_SIZE>(event_pointer)?;
                (
                    u32::from_le_bytes(bytes[..4].try_into().unwrap()),
                    u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
                )
            }
            GuestArchitecture::X86_64 => {
                let bytes = self.marshaller.copy_struct_from::<EPOLL_X86_SIZE>(event_pointer)?;
                (
                    u32::from_le_bytes(bytes[..4].try_into().unwrap()),
                    u64::from_le_bytes(bytes[4..12].try_into().unwrap()),
                )
            }
        };
        if interest_bits & !0xd000_201f != 0 {
            return Err(Error::Invalid);
        }
        Ok(EpollControlPlan {
            operation,
            descriptor,
            interests: Some(EpollInterest::from_bits(Self::epoll_interests(interest_bits))),
            data: Some(data),
        })
    }

    pub fn epoll_wait(
        &self,
        output: u64,
        maximum: i32,
        timeout_milliseconds: i32,
        signal_mask: u64,
        signal_set_size: usize,
    ) -> Result<EpollWaitPlan, Error> {
        if maximum <= 0 {
            return Err(Error::Invalid);
        }
        let maximum = (maximum as usize).min(EPOLL_MAXIMUM);
        let mask = self.optional_signal_mask(signal_mask, signal_set_size)?;
        self.probe_event_output(output, maximum)?;
        let timeout_nanoseconds = if timeout_milliseconds < 0 {
            None
        } else {
            Some((timeout_milliseconds as u64) * 1_000_000)
        };
        Ok(EpollWaitPlan {
            output,
            maximum,
            timeout_nanoseconds,
            signal_mask: mask,
        })
    }

    pub fn epoll_pwait2(
        &self,
        output: u64,
        maximum: i32,
        timeout_pointer: u64,
        signal_mask: u64,
        signal_set_size: usize,
    ) -> Result<EpollWaitPlan, Error> {
        if maximum <= 0 {
            return Err(Error::Invalid);
        }
        let maximum = (maximum as usize).min(EPOLL_MAXIMUM);
        let timeout_nanoseconds = if timeout_pointer == 0 {
            None
        } else {
            Some(self.timespec(timeout_pointer)?.nanoseconds())
        };
        let mask = self.optional_signal_mask(signal_mask, signal_set_size)?;
        self.probe_event_output(output, maximum)?;
        Ok(EpollWaitPlan {
            output,
            maximum,
            timeout_nanoseconds,
            signal_mask: mask,
        })
    }

    fn optional_signal_mask(&self, pointer: u64, size: usize) -> Result<Option<SignalMask>, Error> {
        if pointer == 0 {
            return Ok(None);
        }
        if size != SIGNAL_SET_SIZE {
            return Err(Error::Invalid);
        }
        let bytes = self.marshaller.copy_struct_from::<SIGNAL_SET_SIZE>(pointer)?;
        Ok(Some(SignalMask::from_bits(u64::from_le_bytes(bytes))))
    }

    fn probe_event_output(&self, output: u64, maximum: usize) -> Result<(), Error> {
        let length = maximum.checked_mul(self.epoll_size()).ok_or(Error::Overflow)?;
        let available = self.marshaller.probe(output, length, GuestAccess::Write)?;
        if available != length {
            return Err(Error::Invalid);
        }
        Ok(())
    }

    pub fn stage_epoll_events(&self, plan: &EpollWaitPlan, events: &[EpollEvent]) -> Result<StagedEventCopyout, Error> {
        if events.len() > plan.maximum {
            return Err(Error::Invalid);
        }
        let mut bytes = Vec::with_capacity(events.len() * self.epoll_size());
        for event in events {
            bytes.extend_from_slice(&Self::linux_epoll_events(event.readiness).to_le_bytes());
            if self.marshaller.architecture() == GuestArchitecture::Aarch64 {
                bytes.extend_from_slice(&[0; 4]);
            }
            bytes.extend_from_slice(&event.data.to_le_bytes());
        }
        Ok(StagedEventCopyout {
            writes: vec![CopyoutEntry {
                address: plan.output,
                bytes,
            }],
        })
    }

    fn epoll_interests(linux: u32) -> u32 {
        (linux & 0x1)
            | (if linux & 0x4 != 0 { EpollInterest::WRITE } else { 0 })
            | (if linux & 0x2 != 0 { EpollInterest::PRIORITY } else { 0 })
            | (linux & (0x8 | 0x10 | 0x1000_0000 | 0x4000_0000 | 0x8000_0000))
            | (if linux & 0x2000 != 0 { EpollInterest::READ_HANGUP } else { 0 })
    }

    fn linux_epoll_events(readiness: hl_descriptor::Readiness) -> u32 {
        (readiness.bits() & Readiness::READ)
            | (if readiness.contains(Readiness::WRITE) { 0x4 } else { 0 })
            | (if readiness.contains(Readiness::PRIORITY) { 0x2 } else { 0 })
            | (readiness.bits() & (Readiness::ERROR | Readiness::HANGUP))
            | (if readiness.contains(Readiness::READ_HANGUP) { 0x2000 } else { 0 })
    }

    fn epoll_size(&self) -> usize {
        match self.marshaller.architecture() {
            GuestArchitecture::Aarch64 => EPOLL_AARCH_SIZE,
            GuestArchitecture::X86_64 => EPOLL_X86_SIZE,
        }
    }

    pub fn signalfd4(
        &self,
        descriptor: i32,
        mask_pointer: u64,
        mask_size: usize,
        flags: u32,
    ) -> Result<(i32, SignalMask, SignalFdFlags, CreationFlags), Error> {
        if flags & !(NONBLOCKING | CLOSE_ON_EXEC) != 0 || mask_size != SIGNAL_SET_SIZE {
            return Err(Error::Invalid);
        }
        let bytes = self.marshaller.copy_struct_from::<SIGNAL_SET_SIZE>(mask_pointer)?;
        let mask = SignalMask::from_bits(u64::from_le_bytes(bytes));
        Ok((
            descriptor,
            mask,
            SignalFdFlags::from_bits(flags),
            Self::creation_flags(flags),
        ))
    }

    pub fn timerfd_create(clock: i32, flags: u32) -> Result<(TimerFdClock, TimerFdCreateFlags, CreationFlags), Error> {
        let clock = TimerFdClock::from_linux_id(clock).ok_or(Error::Invalid)?;
        if flags & !(NONBLOCKING | CLOSE_ON_EXEC) != 0 {
            return Err(Error::Invalid);
        }
        Ok((clock, TimerFdCreateFlags::from_bits(flags), Self::creation_flags(flags)))
    }

    pub fn timerfd_settime(&self, flags: u32, new_value: u64, old_value: u64) -> Result<TimerSetPlan, Error> {
        let bytes = self.marshaller.copy_struct_from::<ITIMERSPEC_SIZE>(new_value)?;
        if flags & !3 != 0 {
            return Err(Error::Invalid);
        }
        Ok(TimerSetPlan {
            flags: TimerFdSetFlags::from_bits(flags),
            setting: Self::decode_timer_setting(&bytes)?,
            old_value: (old_value != 0).then_some(old_value),
        })
    }

    pub fn timerfd_gettime_copyout(&self, output: u64, setting: TimerSetting) -> Result<StagedEventCopyout, Error> {
        let bytes = Self::encode_timer_setting(setting);
        let available = self.marshaller.probe(output, bytes.len(), GuestAccess::Write)?;
        if available != bytes.len() {
            return Err(Error::Invalid);
        }
        Ok(StagedEventCopyout {
            writes: vec![CopyoutEntry { address: output, bytes }],
        })
    }

    fn decode_timer_setting(bytes: &[u8; ITIMERSPEC_SIZE]) -> Result<TimerSetting, Error> {
        Ok(TimerSetting {
            interval: Self::duration(&bytes[..16])?,
            value: Self::duration(&bytes[16..])?,
        })
    }

    fn encode_timer_setting(setting: TimerSetting) -> Vec<u8> {
        let mut bytes = Vec::with_capacity(ITIMERSPEC_SIZE);
        for duration in [setting.interval, setting.value] {
            let timespec = duration.timespec();
            bytes.extend_from_slice(&(timespec.seconds() as i64).to_le_bytes());
            bytes.extend_from_slice(&(timespec.subsecond_nanoseconds() as i64).to_le_bytes());
        }
        bytes
    }

    fn timespec(&self, pointer: u64) -> Result<Duration, Error> {
        let bytes = self.marshaller.copy_struct_from::<TIMESPEC_SIZE>(pointer)?;
        Self::duration(&bytes)
    }

    fn duration(bytes: &[u8]) -> Result<Duration, Error> {
        let seconds = i64::from_le_bytes(bytes[..8].try_into().unwrap());
        let nanoseconds = i64::from_le_bytes(bytes[8..16].try_into().unwrap());
        if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
            return Err(Error::Invalid);
        }
        let total = (seconds as u64)
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(nanoseconds as u64))
            .ok_or(Error::Overflow)?;
        Ok(Duration::from_nanoseconds(total))
    }

    pub fn inotify_init1(flags: u32) -> Result<CreationFlags, Error> {
        if flags & !(NONBLOCKING | CLOSE_ON_EXEC) != 0 {
            return Err(Error::Invalid);
        }
        Ok(Self::creation_flags(flags))
    }

    pub fn inotify_add_watch(&self, path: u64, mask: u32) -> Result<InotifyWatchPlan, Error> {
        let path = self.marshaller.c_string(path, 4096)?;
        let mask = InotifyMask::from_bits(mask);
        if mask.bits() & InotifyMask::EVENT_BITS == 0 || mask.bits() & !InotifyMask::ALLOWED_WATCH_BITS != 0 {
            return Err(Error::Invalid);
        }
        Ok(InotifyWatchPlan { path, mask })
    }

    pub fn inotify_remove_watch(descriptor: i32) -> Result<i32, Error> {
        if descriptor < 0 {
            return Err(Error::Invalid);
        }
        Ok(descriptor)
    }

    fn creation_flags(flags: u32) -> CreationFlags {
        CreationFlags {
            close_on_exec: flags & CLOSE_ON_EXEC != 0,
            nonblocking: flags & NONBLOCKING != 0,
        }
    }
}
