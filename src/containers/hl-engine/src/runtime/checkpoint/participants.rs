//! Exact, generation-scoped membership for one capture.
//!
//! The broker is the only component that outlives every guest process and holds
//! an authenticated capability per process, so it is the only place an exact
//! member set can be sealed. `REGISTER_READY` installs one member per engine
//! process while that process is stopped with its thread registry held; nothing
//! may publish capture bytes before its membership is installed.

use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct MemberId(pub(super) u64);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(super) struct ExecutorId(pub(super) u64);

/// A host process identity that cannot be reused: pid alone is recycled, so the
/// broker's authenticated start time is part of the key. `generation` is the
/// platform's identity generation where it has one; the Linux broker
/// authenticates with `SO_PEERPIDFD` plus start time and leaves it 0, so it is
/// keyed but never required.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ProcessIdentity {
    pub(super) pid: u64,
    pub(super) birth: u64,
    pub(super) generation: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Error {
    /// The identity, executor inventory, or capture generation is not admissible.
    Conflict,
    /// This exact process identity is already a member of this capture.
    Duplicate,
}

/// Retains exactly what a second registration must be checked against: nothing
/// beyond the sealed identity set is stored, because nothing else has a reader
/// yet. The executor inventory is validated at admission, not retained.
pub(super) struct ParticipantLedger {
    capture_generation: u64,
    identities: BTreeMap<(u64, u64, u64), MemberId>,
    next_member: u64,
}

impl ParticipantLedger {
    pub(super) fn new(capture_generation: u64) -> Result<Self, Error> {
        if capture_generation == 0 {
            return Err(Error::Conflict);
        }
        Ok(Self {
            capture_generation,
            identities: BTreeMap::new(),
            next_member: 1,
        })
    }

    /// Installs one stopped engine process and its complete executor inventory.
    ///
    /// Fails closed on an unauthenticated identity (no pid or no start time), on a
    /// request that names another capture generation, on an empty or malformed
    /// inventory, and on any repeat of an identity already installed.
    pub(super) fn register(
        &mut self,
        capture_generation: u64,
        identity: ProcessIdentity,
        executors: &BTreeSet<ExecutorId>,
    ) -> Result<MemberId, Error> {
        if capture_generation != self.capture_generation
            || identity.pid == 0
            || identity.birth == 0
            || executors.is_empty()
            || executors.contains(&ExecutorId(0))
        {
            return Err(Error::Conflict);
        }
        let key = (identity.pid, identity.birth, identity.generation);
        if self.identities.contains_key(&key) {
            return Err(Error::Duplicate);
        }
        let id = MemberId(self.next_member);
        self.next_member = self.next_member.checked_add(1).ok_or(Error::Conflict)?;
        self.identities.insert(key, id);
        Ok(id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(pid: u64) -> ProcessIdentity {
        ProcessIdentity {
            pid,
            birth: pid * 10,
            generation: 3,
        }
    }

    fn executors(values: &[u64]) -> BTreeSet<ExecutorId> {
        values.iter().copied().map(ExecutorId).collect()
    }

    #[test]
    fn zero_capture_generation_has_no_ledger() {
        assert_eq!(ParticipantLedger::new(0).err(), Some(Error::Conflict));
    }

    #[test]
    fn distinct_processes_receive_distinct_member_identifiers() {
        let mut ledger = ParticipantLedger::new(7).unwrap();
        assert_eq!(ledger.register(7, identity(11), &executors(&[1, 2])), Ok(MemberId(1)));
        assert_eq!(ledger.register(7, identity(12), &executors(&[1])), Ok(MemberId(2)));
    }

    #[test]
    fn a_repeated_process_identity_is_refused_as_a_duplicate() {
        let mut ledger = ParticipantLedger::new(7).unwrap();
        ledger.register(7, identity(11), &executors(&[1])).unwrap();
        assert_eq!(
            ledger.register(7, identity(11), &executors(&[1])),
            Err(Error::Duplicate)
        );
        assert_eq!(
            ledger.register(7, identity(11), &executors(&[2, 3])),
            Err(Error::Duplicate)
        );
    }

    #[test]
    fn another_captures_generation_is_not_a_member_of_this_one() {
        let mut ledger = ParticipantLedger::new(7).unwrap();
        assert_eq!(ledger.register(8, identity(11), &executors(&[1])), Err(Error::Conflict));
        assert_eq!(ledger.register(0, identity(11), &executors(&[1])), Err(Error::Conflict));
    }

    #[test]
    fn an_unauthenticated_identity_or_empty_inventory_is_refused() {
        let mut ledger = ParticipantLedger::new(7).unwrap();
        let mut unauthenticated = identity(11);
        unauthenticated.birth = 0;
        assert_eq!(
            ledger.register(7, unauthenticated, &executors(&[1])),
            Err(Error::Conflict)
        );
        assert_eq!(ledger.register(7, identity(11), &executors(&[])), Err(Error::Conflict));
        assert_eq!(
            ledger.register(7, identity(11), &executors(&[0, 1])),
            Err(Error::Conflict)
        );
    }
}
