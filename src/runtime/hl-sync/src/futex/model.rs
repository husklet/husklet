use hl_time::Timespec;

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum Key {
    Private { process: u64, address: u64 },
    Shared { backing: u64, offset: u64 },
}
pub type FutexKey = Key;

impl FutexKey {
    pub fn private(process: u64, address: u64) -> Result<Self, FutexError> {
        Self::validate(address)?;
        Ok(Self::Private { process, address })
    }

    pub fn shared(backing: u64, offset: u64) -> Result<Self, FutexError> {
        Self::validate(offset)?;
        Ok(Self::Shared { backing, offset })
    }

    // The mask spells the futex word's 4-byte alignment requirement more directly than a bit count.
    #[allow(clippy::verbose_bit_mask)]
    fn validate(coordinate: u64) -> Result<(), FutexError> {
        if coordinate & 3 == 0 {
            Ok(())
        } else {
            Err(FutexError::InvalidArgument)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Clock {
    Monotonic,
    Realtime,
}
pub type FutexClock = Clock;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Deadline {
    pub clock: FutexClock,
    pub value: Timespec,
}
pub type FutexDeadline = Deadline;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtomicOperation {
    Set(i32),
    Add(i32),
    Or(i32),
    AndNot(i32),
    Xor(i32),
}
pub type FutexAtomicOperation = AtomicOperation;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WaitOutcome {
    Woken,
    Interrupted,
    TimedOut,
}
pub type FutexWaitOutcome = WaitOutcome;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WaitTarget {
    pub key: FutexKey,
    pub expected: u32,
}
pub type FutexWaitTarget = WaitTarget;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MultipleOutcome {
    Woken(usize),
    Interrupted,
    TimedOut,
}
pub type FutexWaitMultipleOutcome = MultipleOutcome;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidArgument,
    ValueMismatch,
    Fault,
    ResourceLimit,
    CompareMismatch,
    ClockFailed,
}
pub type FutexError = Error;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub buckets: usize,
    pub keys: usize,
    pub waiters: usize,
}
pub type FutexLimits = Limits;

impl Default for FutexLimits {
    fn default() -> Self {
        Self {
            buckets: 64,
            keys: 4_096,
            waiters: 65_536,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WaitSnapshot {
    pub key: FutexKey,
    pub bitset: u32,
}
pub type FutexWaitSnapshot = WaitSnapshot;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub next_waiter: u64,
    pub waits: Vec<FutexWaitSnapshot>,
}
pub type FutexSnapshot = Snapshot;
