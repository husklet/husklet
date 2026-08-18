use super::super::authority::PrepareId;
use std::{
    collections::{HashMap, HashSet},
    num::NonZeroU64,
};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum AdmissionError {
    Closed,
    Conflict,
    Deadline,
    NotDurable,
    Poisoned,
    Stale,
    Unauthorized,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct Epoch(NonZeroU64);

impl Epoch {
    pub(super) fn new(value: u64) -> Result<Self, AdmissionError> {
        NonZeroU64::new(value).map(Self).ok_or(AdmissionError::Conflict)
    }

    pub(super) fn next(self) -> Result<Self, AdmissionError> {
        self.0
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(Self)
            .ok_or(AdmissionError::Poisoned)
    }
}

fn nonzero(value: PrepareId) -> Result<PrepareId, AdmissionError> {
    (value.0 != [0; 16]).then_some(value).ok_or(AdmissionError::Conflict)
}

macro_rules! capability {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
        pub(crate) struct $name(pub(super) PrepareId);
        impl $name {
            pub(super) fn new(value: PrepareId) -> Result<Self, AdmissionError> {
                nonzero(value).map(Self)
            }
        }
    };
}

capability!(CloseId);
capability!(TicketId);
capability!(ParentRole);
capability!(ChildRole);
capability!(CaptureChannel);
capability!(LifecycleRole);
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct CheckpointGeneration(PrepareId);

impl CheckpointGeneration {
    pub(in crate::runtime::checkpoint) fn authenticated(uuid: [u8; 16]) -> Result<Self, AdmissionError> {
        nonzero(PrepareId(uuid)).map(Self)
    }

    #[cfg(test)]
    pub(super) fn new(value: PrepareId) -> Result<Self, AdmissionError> {
        nonzero(value).map(Self)
    }

    pub(crate) fn bytes(self) -> [u8; 16] {
        self.0.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct LineageId(PrepareId);

impl LineageId {
    pub(in crate::runtime::checkpoint) fn authenticated(bytes: [u8; 16]) -> Result<Self, AdmissionError> {
        nonzero(PrepareId(bytes)).map(Self)
    }

    #[cfg(test)]
    pub(super) fn new(value: PrepareId) -> Result<Self, AdmissionError> {
        nonzero(value).map(Self)
    }

    pub(crate) fn bytes(self) -> [u8; 16] {
        self.0.0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct MemberOrdinal(NonZeroU64);

impl MemberOrdinal {
    pub(crate) fn new(value: u64) -> Result<Self, AdmissionError> {
        NonZeroU64::new(value).map(Self).ok_or(AdmissionError::Conflict)
    }

    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }

    pub(super) fn next(self) -> Result<Self, AdmissionError> {
        self.0
            .get()
            .checked_add(1)
            .and_then(NonZeroU64::new)
            .map(Self)
            .ok_or(AdmissionError::Poisoned)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct AddressSpaceOrdinal(NonZeroU64);

impl AddressSpaceOrdinal {
    pub(crate) fn new(value: u64) -> Result<Self, AdmissionError> {
        NonZeroU64::new(value).map(Self).ok_or(AdmissionError::Conflict)
    }

    pub(crate) fn get(self) -> u64 {
        self.0.get()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct OfdId {
    pub(super) generation: CheckpointGeneration,
    pub(super) member: MemberOrdinal,
    pub(super) sequence: NonZeroU64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct OfdNamespace {
    pub(super) lineage: LineageId,
    pub(super) generation: CheckpointGeneration,
    pub(super) next_member: MemberOrdinal,
    /// Next never-issued sequence for each stable member ordinal.
    pub(super) next: HashMap<MemberOrdinal, NonZeroU64>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ProcessKey {
    pub(super) pid: i32,
    pub(super) birth: u64,
}

impl ProcessKey {
    pub(crate) fn new(pid: i32, birth: u64) -> Result<Self, AdmissionError> {
        (pid > 0 && birth != 0)
            .then_some(Self { pid, birth })
            .ok_or(AdmissionError::Conflict)
    }

    pub(crate) fn pid(self) -> i32 {
        self.pid
    }

    pub(crate) fn birth(self) -> u64 {
        self.birth
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct ProcessIdentity {
    pub(super) key: ProcessKey,
    pub(super) parent: Option<ProcessKey>,
}

/// Logical identity recorded in a checkpoint. It is deliberately distinct
/// from the authenticated identity of the newly spawned host process.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub(crate) struct SavedProcessIdentity {
    pub(super) key: ProcessKey,
    pub(super) parent: Option<ProcessKey>,
    pub(super) member: MemberOrdinal,
}

impl SavedProcessIdentity {
    pub(crate) fn new(key: ProcessKey, parent: Option<ProcessKey>, member: MemberOrdinal) -> Self {
        Self { key, parent, member }
    }

    pub(crate) fn key(self) -> ProcessKey {
        self.key
    }

    pub(crate) fn parent(self) -> Option<ProcessKey> {
        self.parent
    }

    pub(crate) fn member(self) -> MemberOrdinal {
        self.member
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct Publication<D> {
    pub(super) task: PrepareId,
    pub(super) resource: PrepareId,
    pub(super) snapshot: D,
}

impl<D> Publication<D> {
    pub(super) fn new(task: PrepareId, resource: PrepareId, snapshot: D) -> Result<Self, AdmissionError> {
        let task = nonzero(task)?;
        let resource = nonzero(resource)?;
        (task != resource)
            .then_some(Self {
                task,
                resource,
                snapshot,
            })
            .ok_or(AdmissionError::Conflict)
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct ResourceSnapshot<D, I: Eq + std::hash::Hash = ProcessIdentity> {
    pub(super) digest: D,
    pub(super) channels: HashMap<I, CaptureChannel>,
}

pub(super) fn validate_topology(
    root: ProcessIdentity,
    members: &HashSet<ProcessIdentity>,
) -> Result<(), AdmissionError> {
    if root.parent.is_some() || !members.contains(&root) {
        return Err(AdmissionError::Conflict);
    }
    let mut by_key = HashMap::new();
    let mut pids = HashSet::new();
    for member in members {
        if by_key.insert(member.key, *member).is_some() || !pids.insert(member.key.pid) {
            return Err(AdmissionError::Conflict);
        }
    }
    for member in members {
        let mut cursor = *member;
        let mut seen = HashSet::new();
        while cursor != root {
            if !seen.insert(cursor.key) {
                return Err(AdmissionError::Conflict);
            }
            let parent = cursor.parent.ok_or(AdmissionError::Conflict)?;
            cursor = *by_key.get(&parent).ok_or(AdmissionError::Conflict)?;
        }
    }
    Ok(())
}

pub(super) fn validate_saved_topology(
    members: &HashSet<SavedProcessIdentity>,
) -> Result<SavedProcessIdentity, AdmissionError> {
    let roots = members
        .iter()
        .filter(|member| member.parent.is_none())
        .copied()
        .collect::<Vec<_>>();
    if roots.len() != 1 {
        return Err(AdmissionError::Conflict);
    }
    let root = roots[0];
    let mut by_key = HashMap::new();
    let mut pids = HashSet::new();
    let mut ordinals = HashSet::new();
    for member in members {
        if by_key.insert(member.key, *member).is_some()
            || !pids.insert(member.key.pid)
            || !ordinals.insert(member.member)
        {
            return Err(AdmissionError::Conflict);
        }
    }
    for member in members {
        let mut cursor = *member;
        let mut seen = HashSet::new();
        while cursor != root {
            if !seen.insert(cursor.key) {
                return Err(AdmissionError::Conflict);
            }
            cursor = *by_key
                .get(&cursor.parent.ok_or(AdmissionError::Conflict)?)
                .ok_or(AdmissionError::Conflict)?;
        }
    }
    Ok(root)
}
