use super::{
    model::{
        AddressSpaceOrdinal, AdmissionError, CheckpointGeneration, ChildRole, Epoch, LineageId, MemberOrdinal,
        ProcessIdentity, SavedProcessIdentity,
    },
    restore::RestoreAdmission,
    ticket::{Authority, Event, Phase},
};
use std::{
    collections::{BTreeMap, HashMap},
    hash::Hash,
};

pub(crate) const RESERVATION_DOMAIN_OBJECT: &str = ".husklet-authority/reservation-domain-v1";

/// Platform boundary that upgrades an untrusted broker PID into a birth-bound
/// live identity (Linux pidfd/start-time or macOS peer-PID plus birth proof).
pub(crate) trait PeerAuthenticator: Send + Sync {
    fn authenticate(&self, host_pid: u64) -> Result<ProcessIdentity, AdmissionError>;
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(crate) struct ReservationKey {
    pub(crate) lineage: LineageId,
    pub(crate) generation: CheckpointGeneration,
    pub(crate) address_space: AddressSpaceOrdinal,
}

pub(crate) trait ReservationPlanner {
    type Plan;
    fn address_space(&mut self, saved: SavedProcessIdentity) -> Result<AddressSpaceOrdinal, AdmissionError>;
    fn plan(&mut self, key: ReservationKey, members: &[SavedProcessIdentity]) -> Result<Self::Plan, AdmissionError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SavedReservationBinding {
    pub(crate) member: MemberOrdinal,
    pub(crate) address_space: AddressSpaceOrdinal,
}

pub(crate) struct ReservationDomainPlan<P> {
    pub(crate) lineage: LineageId,
    pub(crate) generation: CheckpointGeneration,
    pub(crate) epoch: Epoch,
    pub(crate) init_member: MemberOrdinal,
    pub(crate) plans: BTreeMap<AddressSpaceOrdinal, P>,
    pub(crate) source: HashMap<super::ProcessKey, SavedReservationBinding>,
}

/// Narrow seam used by the checkpoint socket broker. It makes physical child
/// ownership publication the first operation after accept, before task or
/// resource reports can be processed.
pub(crate) struct BrokerAdmission<'a, D, A> {
    authority: &'a Authority<D>,
    authenticator: &'a A,
}

impl<D: Copy + Eq + Hash> RestoreAdmission<'_, D> {
    /// Authenticates every saved member into one reservation plan before any
    /// live engine process is admitted. The returned set is exact and keyed by
    /// the same stable member ordinal used by OFD identities.
    pub(crate) fn prepare_reservations<P: ReservationPlanner>(
        &self,
        planner: &mut P,
    ) -> Result<ReservationDomainPlan<P::Plan>, AdmissionError> {
        let mut state = self.authority.lock()?;
        if state.phase != Phase::Restoring || !state.tickets.is_empty() || !state.restored.is_empty() {
            return Err(AdmissionError::Closed);
        }
        let lineage = state.lineage;
        let generation = state.generation;
        let epoch = state.epoch;
        let mut grouped = BTreeMap::<AddressSpaceOrdinal, Vec<SavedProcessIdentity>>::new();
        let mut source = HashMap::new();
        let mut init_member = None;
        for saved in &state.expected_restore {
            let address_space = planner.address_space(*saved)?;
            grouped.entry(address_space).or_default().push(*saved);
            let binding = SavedReservationBinding {
                member: saved.member,
                address_space,
            };
            if source.insert(saved.key, binding).is_some() {
                return Err(AdmissionError::Conflict);
            }
            if saved.parent.is_none() && init_member.replace(saved.member).is_some() {
                return Err(AdmissionError::Conflict);
            }
        }
        let mut plans = BTreeMap::new();
        for (address_space, mut members) in grouped {
            members.sort_by_key(|member| member.member);
            let key = ReservationKey {
                lineage,
                generation,
                address_space,
            };
            plans.insert(address_space, planner.plan(key, &members)?);
        }
        state.reservation_members = Some(source.values().map(|binding| binding.member).collect());
        Ok(ReservationDomainPlan {
            lineage,
            generation,
            epoch,
            init_member: init_member.ok_or(AdmissionError::Conflict)?,
            plans,
            source,
        })
    }
}

impl<'a, D: Copy + Eq + Hash, A: PeerAuthenticator> BrokerAdmission<'a, D, A> {
    pub(crate) fn new(authority: &'a Authority<D>, authenticator: &'a A) -> Self {
        Self {
            authority,
            authenticator,
        }
    }

    pub(crate) fn process_started(
        &self,
        event: Event<ChildRole>,
        host_pid: u64,
    ) -> Result<ProcessIdentity, AdmissionError> {
        let identity = self.authenticator.authenticate(host_pid)?;
        self.authority.process_started(event, identity)?;
        Ok(identity)
    }
}
