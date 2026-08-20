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
    /// Membership was sealed before this registration arrived. The coordinator seals only once every
    /// live guest process is frozen, so a registration after the seal describes a process the capture
    /// cannot prove it froze; admitting it would put unfrozen state in the image.
    Sealed,
}

/// Retains exactly what a second registration must be checked against: nothing
/// beyond the sealed identity set is stored, because nothing else has a reader
/// yet. The executor inventory is validated at admission, not retained.
pub(super) struct ParticipantLedger {
    capture_generation: u64,
    identities: BTreeMap<(u64, u64, u64), MemberId>,
    next_member: u64,
    /// The member count at the instant membership was sealed, once it has been.
    sealed: Option<u64>,
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
            sealed: None,
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
        if self.sealed.is_some() {
            return Err(Error::Sealed);
        }
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

    /// Closes membership and answers how many processes this capture sealed.
    ///
    /// This is the single point the manifest's expected process set is fixed at, and it is the whole
    /// reason the count can be exact. The coordinator calls it only after every live guest process has
    /// either committed its group or been proven gone-and-never-registered, and after its own dump has
    /// registered it -- so at the seal no unfrozen guest process remains that could still fork or
    /// register. A later registration is refused rather than admitted, because the capture cannot prove
    /// it froze that process; the member reads the refusal and refuses its own dump instead of
    /// publishing bytes into an image that is already being counted.
    ///
    /// Idempotent: a repeated seal answers the same count rather than failing, so a retried round trip
    /// cannot turn a sound capture into a refusal.
    pub(super) fn seal(&mut self, capture_generation: u64) -> Result<u64, Error> {
        if capture_generation != self.capture_generation {
            return Err(Error::Conflict);
        }
        let count = self.identities.len() as u64;
        Ok(*self.sealed.get_or_insert(count))
    }

    /// Whether this host pid holds a membership record for `capture_generation`.
    ///
    /// Keyed on the pid alone, while `register` keys on pid plus authenticated start time. That is
    /// deliberate and it fails CLOSED in the only direction that matters: the single caller acts on a
    /// `false`, and a recycled pid can only turn a `false` into a `true`, which withholds the exemption
    /// and refuses the capture. It can never invent membership that was never sealed.
    pub(super) fn registered(&self, capture_generation: u64, pid: u64) -> bool {
        capture_generation == self.capture_generation
            && pid != 0
            && self.identities.keys().any(|(identity, _, _)| *identity == pid)
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

    /// The exemption query answers only about processes this ledger actually sealed, and only at the
    /// generation it sealed them for. A `false` is what lets the coordinator drop a peer, so every way
    /// of being wrong here has to fall on the "still a member" side.
    #[test]
    fn only_a_registered_process_reads_as_a_member_of_this_generation() {
        let mut ledger = ParticipantLedger::new(7).unwrap();
        assert!(!ledger.registered(7, 11), "an empty ledger reported a member");
        ledger.register(7, identity(11), &executors(&[1])).unwrap();
        assert!(ledger.registered(7, 11));
        assert!(!ledger.registered(7, 12), "an unregistered process read as a member");
        assert!(
            !ledger.registered(8, 11),
            "another generation's membership was answered"
        );
        assert!(!ledger.registered(7, 0), "an unauthenticated pid read as a member");
    }

    /// The seal is what makes the manifest's expected process set exact. It answers the count of
    /// processes that proved membership -- not what anyone enumerated -- and it does so at one instant
    /// that no later registration can move.
    #[test]
    fn sealing_answers_the_exact_number_of_processes_that_proved_membership() {
        let mut ledger = ParticipantLedger::new(7).unwrap();
        ledger.register(7, identity(11), &executors(&[1])).unwrap();
        ledger.register(7, identity(12), &executors(&[1, 2])).unwrap();
        assert_eq!(ledger.seal(7), Ok(2));
    }

    /// A retried round trip must not be able to turn a sound capture into a refusal, so the seal is
    /// idempotent rather than a one-shot.
    #[test]
    fn a_repeated_seal_answers_the_same_count() {
        let mut ledger = ParticipantLedger::new(7).unwrap();
        ledger.register(7, identity(11), &executors(&[1])).unwrap();
        assert_eq!(ledger.seal(7), Ok(1));
        assert_eq!(ledger.seal(7), Ok(1));
    }

    #[test]
    fn another_generation_cannot_seal_this_captures_membership() {
        let mut ledger = ParticipantLedger::new(7).unwrap();
        assert_eq!(ledger.seal(8), Err(Error::Conflict));
    }

    /// The direction that stops silent loss: once the count is fixed, a process that registers late is
    /// refused rather than quietly added to an image whose manifest has already been counted. The
    /// refusal is what makes the member refuse its own dump instead of publishing unfrozen state.
    #[test]
    fn a_registration_after_the_seal_is_refused_and_does_not_move_the_count() {
        let mut ledger = ParticipantLedger::new(7).unwrap();
        ledger.register(7, identity(11), &executors(&[1])).unwrap();
        assert_eq!(ledger.seal(7), Ok(1));
        assert_eq!(ledger.register(7, identity(12), &executors(&[1])), Err(Error::Sealed));
        assert_eq!(ledger.seal(7), Ok(1));
        assert!(
            !ledger.registered(7, 12),
            "a process refused at the seal was recorded as a member anyway"
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
