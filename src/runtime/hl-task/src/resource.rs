use std::collections::BTreeMap;

use crate::TaskError;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Resource {
    CpuTime,
    FileSize,
    Data,
    Stack,
    Core,
    ResidentSet,
    Processes,
    OpenFiles,
    LockedMemory,
    AddressSpace,
    Locks,
    PendingSignals,
    MessageQueue,
    Nice,
    RealtimePriority,
    RealtimeTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limit {
    pub soft: u64,
    pub hard: u64,
}

impl Limit {
    pub const fn new(soft: u64, hard: u64) -> Result<Self, TaskError> {
        if soft > hard {
            Err(TaskError::InvalidLimit)
        } else {
            Ok(Self { soft, hard })
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProcessLimits(BTreeMap<Resource, Limit>);

impl ProcessLimits {
    #[must_use]
    pub const fn empty() -> Self {
        Self(BTreeMap::new())
    }

    pub fn set(&mut self, resource: Resource, limit: Limit) {
        self.0.insert(resource, limit);
    }

    #[must_use]
    pub fn get(&self, resource: Resource) -> Option<Limit> {
        self.0.get(&resource).copied()
    }

    pub(crate) fn entries(&self) -> Vec<(Resource, Limit)> {
        self.0.iter().map(|(resource, limit)| (*resource, *limit)).collect()
    }

    pub(crate) fn from_entries(entries: &[(Resource, Limit)]) -> Result<Self, TaskError> {
        let mut limits = BTreeMap::new();
        for (resource, limit) in entries {
            if limit.soft > limit.hard || limits.insert(*resource, *limit).is_some() {
                return Err(TaskError::InvalidLimit);
            }
        }
        Ok(Self(limits))
    }
}

impl Default for ProcessLimits {
    fn default() -> Self {
        const INFINITY: Limit = Limit {
            soft: u64::MAX,
            hard: u64::MAX,
        };
        let mut limits = Self::empty();
        for resource in [
            Resource::CpuTime,
            Resource::FileSize,
            Resource::Data,
            Resource::Stack,
            Resource::Core,
            Resource::ResidentSet,
            Resource::Processes,
            Resource::OpenFiles,
            Resource::LockedMemory,
            Resource::AddressSpace,
            Resource::Locks,
            Resource::PendingSignals,
            Resource::MessageQueue,
            Resource::Nice,
            Resource::RealtimePriority,
            Resource::RealtimeTime,
        ] {
            limits.set(resource, INFINITY);
        }
        limits.set(
            Resource::Stack,
            Limit {
                soft: 8 << 20,
                hard: u64::MAX,
            },
        );
        limits.set(
            Resource::Core,
            Limit {
                soft: 0,
                hard: u64::MAX,
            },
        );
        limits.set(
            Resource::OpenFiles,
            Limit {
                soft: 20_480,
                hard: 1_048_576,
            },
        );
        limits
    }
}
