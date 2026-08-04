use hl_descriptor::{DescriptionIdentity, ObjectError, OperationLease, Readiness};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct Interest(u32);
pub type EpollInterest = Interest;

impl EpollInterest {
    pub const READ: u32 = Readiness::READ;
    pub const WRITE: u32 = Readiness::WRITE;
    pub const PRIORITY: u32 = Readiness::PRIORITY;
    pub const ERROR: u32 = Readiness::ERROR;
    pub const HANGUP: u32 = Readiness::HANGUP;
    pub const READ_HANGUP: u32 = Readiness::READ_HANGUP;
    pub const EXCLUSIVE: u32 = 1 << 28;
    pub const ONESHOT: u32 = 1 << 30;
    pub const EDGE_TRIGGERED: u32 = 1 << 31;
    const READINESS: u32 = Self::READ | Self::WRITE | Self::PRIORITY | Self::ERROR | Self::HANGUP | Self::READ_HANGUP;
    const ALLOWED: u32 = Self::READINESS | Self::EXCLUSIVE | Self::ONESHOT | Self::EDGE_TRIGGERED;

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    pub(crate) const fn valid(self) -> bool {
        self.0 & !Self::ALLOWED == 0
    }

    pub(crate) const fn readiness(self) -> Readiness {
        Readiness::from_bits(self.0 & Self::READINESS)
    }

    pub(crate) const fn edge_triggered(self) -> bool {
        self.0 & Self::EDGE_TRIGGERED != 0
    }

    pub(crate) const fn oneshot(self) -> bool {
        self.0 & Self::ONESHOT != 0
    }

    pub(crate) const fn exclusive(self) -> bool {
        self.0 & Self::EXCLUSIVE != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct WatchKey {
    pub descriptor_number: i32,
    pub descriptor_generation: u32,
    pub description: DescriptionIdentity,
}
pub type EpollWatchKey = WatchKey;

impl EpollWatchKey {
    pub(crate) fn from_lease(lease: &OperationLease) -> Self {
        Self {
            descriptor_number: lease.descriptor_number(),
            descriptor_generation: lease.descriptor_generation(),
            description: lease.description_identity(),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Event {
    pub readiness: Readiness,
    pub data: u64,
}
pub type EpollEvent = Event;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WatchSnapshot {
    pub key: EpollWatchKey,
    pub interests: EpollInterest,
    pub data: u64,
    pub previous: Readiness,
    pub disabled: bool,
}
pub type EpollWatchSnapshot = WatchSnapshot;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub watch_limit: usize,
    pub next_token: u64,
    pub epoch: u64,
    pub watches: Vec<EpollWatchSnapshot>,
    pub ready: Vec<EpollWatchKey>,
}
pub type EpollSnapshot = Snapshot;

pub type Status = crate::EventStatus;
pub type EpollStatus = Status;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidArgument,
    AlreadyExists,
    NotFound,
    ResourceLimit,
    Interrupted,
    Retired,
    TargetUnavailable,
}
pub type EpollError = Error;

impl From<ObjectError> for EpollError {
    fn from(error: ObjectError) -> Self {
        match error {
            ObjectError::Interrupted => Self::Interrupted,
            ObjectError::ResourceLimit => Self::ResourceLimit,
            ObjectError::Retired => Self::Retired,
            _ => Self::TargetUnavailable,
        }
    }
}
