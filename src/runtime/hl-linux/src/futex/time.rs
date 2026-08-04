use hl_isa::GuestArchitecture;
use hl_time::Timespec;

use crate::{Errno, GuestAccess, GuestMarshaller, GuestMemory, MarshalError};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    Fault,
    Invalid,
    Unsupported,
    Overflow,
}

impl Error {
    #[must_use]
    pub const fn errno(self) -> Errno {
        match self {
            Self::Fault => Errno::EFAULT,
            Self::Invalid => Errno::EINVAL,
            Self::Unsupported => Errno::EOPNOTSUPP,
            Self::Overflow => Errno::EOVERFLOW,
        }
    }
}

impl From<MarshalError> for Error {
    fn from(value: MarshalError) -> Self {
        match value {
            MarshalError::Fault(_) => Self::Fault,
            MarshalError::Invalid | MarshalError::TooBig => Self::Invalid,
            MarshalError::Overflow => Self::Overflow,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ClockIdentity {
    Realtime,
    Monotonic,
    ProcessCpu,
    ThreadCpu,
    MonotonicRaw,
    RealtimeCoarse,
    MonotonicCoarse,
    BootTime,
    RealtimeAlarm,
    BootTimeAlarm,
    Cycle,
    Tai,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimerPlan {
    Create {
        clock: ClockIdentity,
        event: Option<TimerEvent>,
        output: u64,
    },
    Set {
        timer: i32,
        absolute: bool,
        value: Timespec,
        interval: Timespec,
        old: u64,
    },
    Get {
        timer: i32,
        output: u64,
    },
    Delete {
        timer: i32,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerEvent {
    pub value: u64,
    pub signal: i32,
    pub notification: i32,
    pub target_thread: Option<i32>,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IntervalTimer {
    pub interval: Timespec,
    pub value: Timespec,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TimeQueryPlan {
    GetTimeOfDay { time: u64, timezone: u64 },
    ProcessTimes { output: u64 },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StagedTimeCopyout {
    destination: u64,
    bytes: Vec<u8>,
}

impl StagedTimeCopyout {
    pub fn commit<M: GuestMemory>(self, marshaller: &GuestMarshaller<'_, M>) -> Result<(), Error> {
        let progress = marshaller.copy_to(self.destination, &self.bytes);
        progress.fault.map_or(Ok(()), |_| Err(Error::Fault))
    }
}

pub struct TimeFutexAbi<'a, M: GuestMemory> {
    pub(super) marshaller: GuestMarshaller<'a, M>,
}

impl<'a, M: GuestMemory> TimeFutexAbi<'a, M> {
    #[must_use]
    pub const fn new(memory: &'a M, architecture: GuestArchitecture) -> Self {
        Self {
            marshaller: GuestMarshaller::new(memory, architecture),
        }
    }

    pub fn nanosleep(&self, request: u64, remainder: u64) -> Result<(Timespec, u64), Error> {
        Ok((self.timespec(request)?, remainder))
    }

    pub fn clock_nanosleep(
        &self,
        clock: i32,
        flags: u32,
        request: u64,
        remainder: u64,
    ) -> Result<(ClockIdentity, bool, Timespec, u64), Error> {
        if flags & !1 != 0 {
            return Err(Error::Invalid);
        }
        let clock = Self::clock(clock)?;
        if matches!(
            clock,
            ClockIdentity::ThreadCpu
                | ClockIdentity::MonotonicRaw
                | ClockIdentity::RealtimeCoarse
                | ClockIdentity::MonotonicCoarse
        ) {
            return Err(Error::Unsupported);
        }
        let requested = self.timespec(request)?;
        let absolute = flags == 1;
        if !absolute && remainder != 0 {
            let available = self.marshaller.probe(remainder, 16, crate::GuestAccess::Write)?;
            if available != 16 {
                return Err(Error::Fault);
            }
        }
        Ok((clock, absolute, requested, remainder))
    }

    pub fn clock_read(&self, clock: i32, destination: u64) -> Result<(ClockIdentity, u64), Error> {
        Ok((Self::read_clock(clock)?, destination))
    }

    pub fn clock_set(&self, clock: i32, source: u64) -> Result<(ClockIdentity, Timespec), Error> {
        let clock = Self::clock(clock)?;
        if clock != ClockIdentity::Realtime {
            return Err(Error::Invalid);
        }
        Ok((clock, self.timespec(source)?))
    }

    pub fn timer_create(&self, clock: i32, event: u64, output: u64) -> Result<TimerPlan, Error> {
        let clock = Self::read_clock(clock)?;
        let event = if event == 0 {
            None
        } else {
            // Linux consumes the fixed sigev_value/signo/notify prefix first.
            // SIGEV_THREAD_ID is the only admitted notification whose union
            // payload is interpreted by the kernel, as a 32-bit tid at +16.
            let bytes = self.marshaller.copy_struct_from::<16>(event)?;
            let signal = Self::signed(&bytes, 8);
            let notification = Self::signed(&bytes, 12);
            if !matches!(notification, 0..=2 | 4) || (notification != 1 && !(1..=64).contains(&signal)) {
                return Err(Error::Invalid);
            }
            let target_thread = if notification == 4 {
                let thread = self
                    .marshaller
                    .copy_struct_from::<4>(event.checked_add(16).ok_or(Error::Overflow)?)?;
                Some(Self::signed(&thread, 0))
            } else {
                None
            };
            Some(TimerEvent {
                value: Self::word(&bytes, 0),
                signal,
                notification,
                target_thread,
            })
        };
        Ok(TimerPlan::Create { clock, event, output })
    }

    pub fn interval_timer(&self, source: u64) -> Result<IntervalTimer, Error> {
        let bytes = self.marshaller.copy_struct_from::<32>(source)?;
        Ok(IntervalTimer {
            interval: Self::decode_timeval(&bytes[..16])?,
            value: Self::decode_timeval(&bytes[16..])?,
        })
    }

    pub fn stage_interval(&self, destination: u64, timer: IntervalTimer) -> Result<StagedTimeCopyout, Error> {
        let mut bytes = Vec::with_capacity(32);
        Self::encode_timeval(&mut bytes, timer.interval);
        Self::encode_timeval(&mut bytes, timer.value);
        self.stage(destination, bytes)
    }

    pub fn timer_set(&self, timer: i32, flags: u32, source: u64, old: u64) -> Result<TimerPlan, Error> {
        if flags & !1 != 0 {
            return Err(Error::Invalid);
        }
        if source == 0 {
            return Err(Error::Invalid);
        }
        let bytes = self.marshaller.copy_struct_from::<32>(source)?;
        let interval = Self::decode_timespec(&bytes[..16])?;
        let value = Self::decode_timespec(&bytes[16..])?;
        Ok(TimerPlan::Set {
            timer,
            absolute: flags == 1,
            value,
            interval,
            old,
        })
    }

    #[must_use]
    pub const fn timer_get(&self, timer: i32, output: u64) -> TimerPlan {
        TimerPlan::Get { timer, output }
    }

    #[must_use]
    pub const fn timer_delete(&self, timer: i32) -> TimerPlan {
        TimerPlan::Delete { timer }
    }

    pub fn stage_timer_id(&self, destination: u64, timer: i32) -> Result<StagedTimeCopyout, Error> {
        self.stage(destination, timer.to_ne_bytes().to_vec())
    }

    #[must_use]
    pub const fn gettimeofday(&self, time: u64, timezone: u64) -> TimeQueryPlan {
        TimeQueryPlan::GetTimeOfDay { time, timezone }
    }

    #[must_use]
    pub const fn times(&self, output: u64) -> TimeQueryPlan {
        TimeQueryPlan::ProcessTimes { output }
    }

    pub fn stage_timespec(&self, destination: u64, value: Timespec) -> Result<StagedTimeCopyout, Error> {
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(&(value.seconds() as i64).to_le_bytes());
        bytes.extend_from_slice(&(value.subsecond_nanoseconds() as i64).to_le_bytes());
        self.stage(destination, bytes)
    }

    pub fn stage_timeval(&self, destination: u64, seconds: i64, microseconds: i64) -> Result<StagedTimeCopyout, Error> {
        if !(0..1_000_000).contains(&microseconds) {
            return Err(Error::Invalid);
        }
        let mut bytes = Vec::with_capacity(16);
        bytes.extend_from_slice(&seconds.to_le_bytes());
        bytes.extend_from_slice(&microseconds.to_le_bytes());
        self.stage(destination, bytes)
    }

    pub fn stage_process_times(&self, destination: u64, values: [i64; 4]) -> Result<StagedTimeCopyout, Error> {
        let bytes = values.iter().flat_map(|value| value.to_le_bytes()).collect();
        self.stage(destination, bytes)
    }

    pub fn stage_timer(
        &self,
        destination: u64,
        interval: Timespec,
        value: Timespec,
    ) -> Result<StagedTimeCopyout, Error> {
        let mut bytes = Vec::with_capacity(32);
        Self::encode_timespec(&mut bytes, interval);
        Self::encode_timespec(&mut bytes, value);
        self.stage(destination, bytes)
    }

    pub fn stage_robust_list(
        &self,
        head_output: u64,
        length_output: u64,
        head: u64,
    ) -> Result<(StagedTimeCopyout, StagedTimeCopyout), Error> {
        let head = self.stage(head_output, head.to_le_bytes().to_vec())?;
        let length = self.stage(length_output, 24_u64.to_le_bytes().to_vec())?;
        Ok((head, length))
    }

    pub(crate) fn timespec(&self, address: u64) -> Result<Timespec, Error> {
        let bytes = self.marshaller.copy_struct_from::<16>(address)?;
        Self::decode_timespec(&bytes)
    }

    fn decode_timespec(bytes: &[u8]) -> Result<Timespec, Error> {
        let seconds = i64::from_le_bytes(bytes[..8].try_into().expect("seconds"));
        let nanoseconds = i64::from_le_bytes(bytes[8..16].try_into().expect("nanoseconds"));
        if seconds < 0 || !(0..1_000_000_000).contains(&nanoseconds) {
            return Err(Error::Invalid);
        }
        Timespec::new(seconds as u64, nanoseconds as u32).ok_or(Error::Invalid)
    }

    fn decode_timeval(bytes: &[u8]) -> Result<Timespec, Error> {
        let seconds = i64::from_le_bytes(bytes[..8].try_into().expect("seconds"));
        let microseconds = i64::from_le_bytes(bytes[8..16].try_into().expect("microseconds"));
        if seconds < 0 || !(0..1_000_000).contains(&microseconds) {
            return Err(Error::Invalid);
        }
        Timespec::new(seconds as u64, (microseconds as u32) * 1_000).ok_or(Error::Invalid)
    }

    fn encode_timeval(bytes: &mut Vec<u8>, value: Timespec) {
        bytes.extend_from_slice(&(value.seconds() as i64).to_le_bytes());
        bytes.extend_from_slice(&i64::from(value.subsecond_nanoseconds() / 1_000).to_le_bytes());
    }

    fn stage(&self, destination: u64, bytes: Vec<u8>) -> Result<StagedTimeCopyout, Error> {
        let available = self
            .marshaller
            .probe(destination, bytes.len(), GuestAccess::Write)
            .map_err(|error| match error {
                MarshalError::Overflow => Error::Fault,
                error => error.into(),
            })?;
        if available != bytes.len() {
            return Err(Error::Fault);
        }
        Ok(StagedTimeCopyout { destination, bytes })
    }

    const fn clock(raw: i32) -> Result<ClockIdentity, Error> {
        match raw {
            0 => Ok(ClockIdentity::Realtime),
            1 => Ok(ClockIdentity::Monotonic),
            2 => Ok(ClockIdentity::ProcessCpu),
            3 => Ok(ClockIdentity::ThreadCpu),
            4 => Ok(ClockIdentity::MonotonicRaw),
            5 => Ok(ClockIdentity::RealtimeCoarse),
            6 => Ok(ClockIdentity::MonotonicCoarse),
            7 => Ok(ClockIdentity::BootTime),
            _ => Err(Error::Invalid),
        }
    }

    const fn read_clock(raw: i32) -> Result<ClockIdentity, Error> {
        if raw < 0 {
            let pid = !(raw >> 3);
            let kind = raw & 3;
            if pid == 0 && kind == 2 {
                return if raw & 4 != 0 {
                    Ok(ClockIdentity::ThreadCpu)
                } else {
                    Ok(ClockIdentity::ProcessCpu)
                };
            }
            return Err(Error::Invalid);
        }
        match raw {
            0..=7 => Self::clock(raw),
            8 => Ok(ClockIdentity::RealtimeAlarm),
            9 => Ok(ClockIdentity::BootTimeAlarm),
            10 => Ok(ClockIdentity::Cycle),
            11 => Ok(ClockIdentity::Tai),
            _ => Err(Error::Invalid),
        }
    }

    fn encode_timespec(bytes: &mut Vec<u8>, value: Timespec) {
        bytes.extend_from_slice(&(value.seconds() as i64).to_le_bytes());
        bytes.extend_from_slice(&(value.subsecond_nanoseconds() as i64).to_le_bytes());
    }

    pub(super) fn word(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("word"))
    }

    fn signed(bytes: &[u8], offset: usize) -> i32 {
        i32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("signed"))
    }
}
