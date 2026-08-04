use std::sync::atomic::{AtomicBool, Ordering};

use crate::Identity;

/// Generation-safe identity of one open file description.
///
/// Descriptor duplication and fork retain this token; exec retains it only
/// when the underlying descriptor survives close-on-exec processing.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct FlockOwnerToken {
    pub identity: u64,
    pub generation: u32,
}

/// Generation-safe process ownership used by POSIX record locks.
///
/// Runtime retains this token across exec. A forked child receives a new token
/// and therefore does not inherit the parent's POSIX locks.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ProcessLockOwner {
    pub identity: u64,
    pub generation: u32,
}

impl ProcessLockOwner {
    const OPEN_FILE: u32 = 1 << 31;

    /// Produces a range-lock owner scoped to one open file description.
    #[must_use]
    pub const fn open_file(owner: FlockOwnerToken) -> Self {
        Self {
            identity: owner.identity,
            generation: owner.generation | Self::OPEN_FILE,
        }
    }

    #[must_use]
    pub const fn is_open_file(self) -> bool {
        self.generation & Self::OPEN_FILE != 0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlockMode {
    Shared,
    Exclusive,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeLockKind {
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RangeWhence {
    Start,
    Current,
    End,
}

/// Linux flock input before signed range normalization.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeLockRequest {
    pub kind: RangeLockKind,
    pub whence: RangeWhence,
    pub start: i64,
    /// Zero extends through end-of-file; negative lengths describe backwards.
    pub length: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LockRange {
    pub start: u64,
    /// Exclusive end; `None` is unbounded.
    pub end: Option<u64>,
}

impl LockRange {
    pub fn normalize(request: RangeLockRequest, current: u64, file_size: u64) -> Result<Self, LockError> {
        let base = match request.whence {
            RangeWhence::Start => 0,
            RangeWhence::Current => current,
            RangeWhence::End => file_size,
        };
        let start = Self::add_signed(base, request.start).ok_or(LockError::InvalidArgument)?;
        if request.length == 0 {
            return Ok(Self { start, end: None });
        }
        if request.length > 0 {
            let end = start.checked_add(request.length as u64).ok_or(LockError::Overflow)?;
            return Ok(Self { start, end: Some(end) });
        }
        let backwards = request.length.unsigned_abs();
        let beginning = start.checked_sub(backwards).ok_or(LockError::InvalidArgument)?;
        Ok(Self {
            start: beginning,
            end: Some(start),
        })
    }

    #[must_use]
    pub fn overlaps(self, other: Self) -> bool {
        Self::before(self.start, other.end) && Self::before(other.start, self.end)
    }

    fn before(start: u64, end: Option<u64>) -> bool {
        end.is_none_or(|end| start < end)
    }

    fn add_signed(base: u64, delta: i64) -> Option<u64> {
        if delta >= 0 {
            base.checked_add(delta as u64)
        } else {
            base.checked_sub(delta.unsigned_abs())
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeConflict {
    pub owner: ProcessLockOwner,
    pub kind: RangeLockKind,
    pub range: LockRange,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LockError {
    InvalidArgument,
    Overflow,
    WouldBlock,
    Interrupted,
    Canceled,
    Deadlock,
    ResourceLimit,
    ConcurrentMutation,
}

/// Cancellation state supplied by runtime signal/teardown policy.
#[derive(Debug, Default)]
pub struct LockCancellation {
    canceled: AtomicBool,
    interrupted: AtomicBool,
}

impl LockCancellation {
    pub fn cancel(&self) {
        self.canceled.store(true, Ordering::Release);
    }

    pub fn interrupt(&self) {
        self.interrupted.store(true, Ordering::Release);
    }

    pub(crate) fn failure(&self) -> Option<LockError> {
        if self.canceled.load(Ordering::Acquire) {
            Some(LockError::Canceled)
        } else if self.interrupted.swap(false, Ordering::AcqRel) {
            Some(LockError::Interrupted)
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FlockSnapshot {
    pub file: Identity,
    pub owner: FlockOwnerToken,
    pub mode: FlockMode,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RangeLockSnapshot {
    pub file: Identity,
    pub owner: ProcessLockOwner,
    pub kind: RangeLockKind,
    pub range: LockRange,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct LockSnapshot {
    pub flocks: Vec<FlockSnapshot>,
    pub ranges: Vec<RangeLockSnapshot>,
}
