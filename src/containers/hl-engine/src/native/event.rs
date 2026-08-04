use super::{Descriptor, DescriptorSyscalls, HostError};
use std::num::NonZeroU64;
use std::sync::Arc;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EventMode {
    Counter,
    Semaphore,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventInterest(u32);

impl EventInterest {
    pub const READABLE: Self = Self(1);
    pub const WRITABLE: Self = Self(2);
    pub const EDGE: Self = Self(4);
    pub const ONESHOT: Self = Self(8);

    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    pub(crate) const fn bits(self) -> u32 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct GenerationToken(NonZeroU64);

impl GenerationToken {
    pub fn new(slot: u32, generation: u32) -> Result<Self, HostError> {
        let value = (u64::from(generation) << 32) | u64::from(slot);
        NonZeroU64::new(value).map(Self).ok_or(HostError::Invalid)
    }

    pub(crate) fn value(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EventReady {
    pub token: GenerationToken,
    pub readable: bool,
    pub writable: bool,
    pub hangup: bool,
    pub error: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimerSetting {
    pub initial_ns: u64,
    pub interval_ns: u64,
}

pub trait EventSyscalls: DescriptorSyscalls {
    fn event_create(&self, initial: u64, semaphore: bool) -> Result<i32, HostError>;
    fn event_read(&self, descriptor: i32) -> Result<u64, HostError>;
    fn event_write(&self, descriptor: i32, value: u64) -> Result<(), HostError>;
    fn timer_create(&self) -> Result<i32, HostError>;
    fn timer_set(&self, descriptor: i32, setting: TimerSetting) -> Result<(), HostError>;
    fn timer_read(&self, descriptor: i32) -> Result<u64, HostError>;
    fn poll_create(&self) -> Result<i32, HostError>;
    fn poll_control(&self, poll: i32, source: i32, operation: u8, interests: u32, token: u64) -> Result<(), HostError>;
    fn poll_wait(&self, poll: i32, timeout_ms: i32, events: &mut [(u32, u64)]) -> Result<usize, HostError>;
}

pub struct Timer<S: EventSyscalls> {
    descriptor: Descriptor<S>,
}

impl<S: EventSyscalls> Timer<S> {
    pub fn create(syscalls: Arc<S>) -> Result<Self, HostError> {
        let raw = syscalls.timer_create()?;
        Ok(Self {
            descriptor: Descriptor::from_raw(syscalls, raw)?,
        })
    }

    pub fn set(&self, setting: TimerSetting) -> Result<(), HostError> {
        self.descriptor.syscalls().timer_set(self.descriptor.raw(), setting)
    }

    pub fn read_expirations(&self) -> Result<u64, HostError> {
        self.descriptor.syscalls().timer_read(self.descriptor.raw())
    }
}

pub struct EventCounter<S: EventSyscalls> {
    descriptor: Descriptor<S>,
}

impl<S: EventSyscalls> EventCounter<S> {
    pub fn create(syscalls: Arc<S>, initial: u64, mode: EventMode) -> Result<Self, HostError> {
        let raw = syscalls.event_create(initial, mode == EventMode::Semaphore)?;
        Ok(Self {
            descriptor: Descriptor::from_raw(syscalls, raw)?,
        })
    }

    pub fn read(&self) -> Result<u64, HostError> {
        self.descriptor.syscalls().event_read(self.descriptor.raw())
    }

    pub fn write(&self, value: u64) -> Result<(), HostError> {
        if value == u64::MAX {
            return Err(HostError::Invalid);
        }
        self.descriptor.syscalls().event_write(self.descriptor.raw(), value)
    }
}

pub struct PollSet<S: EventSyscalls> {
    descriptor: Descriptor<S>,
}

pub trait PollSource<S: EventSyscalls> {
    fn poll_descriptor(&self) -> &Descriptor<S>;
}

impl<S: EventSyscalls> PollSource<S> for EventCounter<S> {
    fn poll_descriptor(&self) -> &Descriptor<S> {
        &self.descriptor
    }
}

impl<S: EventSyscalls> PollSet<S> {
    pub fn create(syscalls: Arc<S>) -> Result<Self, HostError> {
        let raw = syscalls.poll_create()?;
        Ok(Self {
            descriptor: Descriptor::from_raw(syscalls, raw)?,
        })
    }

    pub fn add(
        &self,
        source: &impl PollSource<S>,
        interests: EventInterest,
        token: GenerationToken,
    ) -> Result<(), HostError> {
        self.control(source, 1, interests, token)
    }

    pub fn modify(
        &self,
        source: &impl PollSource<S>,
        interests: EventInterest,
        token: GenerationToken,
    ) -> Result<(), HostError> {
        self.control(source, 2, interests, token)
    }

    pub fn remove(&self, source: &impl PollSource<S>) -> Result<(), HostError> {
        self.descriptor
            .syscalls()
            .poll_control(self.descriptor.raw(), source.poll_descriptor().raw(), 3, 0, 0)
    }

    pub fn wait(&self, timeout_ms: i32, capacity: usize) -> Result<Vec<EventReady>, HostError> {
        if timeout_ms < -1 || capacity > 256 {
            return Err(HostError::Invalid);
        }
        let mut native = vec![(0_u32, 0_u64); capacity];
        let count = self
            .descriptor
            .syscalls()
            .poll_wait(self.descriptor.raw(), timeout_ms, &mut native)?;
        let mut output = Vec::with_capacity(count);
        for (events, token) in native.into_iter().take(count) {
            output.push(EventReady {
                token: GenerationToken(NonZeroU64::new(token).ok_or(HostError::Failed)?),
                readable: events & 1 != 0,
                writable: events & 2 != 0,
                hangup: events & 4 != 0,
                error: events & 8 != 0,
            });
        }
        Ok(output)
    }

    fn control(
        &self,
        source: &impl PollSource<S>,
        operation: u8,
        interests: EventInterest,
        token: GenerationToken,
    ) -> Result<(), HostError> {
        self.descriptor.syscalls().poll_control(
            self.descriptor.raw(),
            source.poll_descriptor().raw(),
            operation,
            interests.bits(),
            token.value(),
        )
    }
}
