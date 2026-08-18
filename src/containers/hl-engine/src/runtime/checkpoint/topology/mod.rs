//! Broker-owned checkpoint topology admission.

mod broker;
mod close;
mod model;
mod restore;
mod ticket;

pub(crate) use broker::{
    BrokerAdmission, PeerAuthenticator, ReservationDomainPlan, ReservationKey, ReservationPlanner,
    SavedReservationBinding,
};
pub(crate) use close::{FreezeGuard, Reaper, StorageGuard};
pub(crate) use model::{
    AddressSpaceOrdinal, AdmissionError, CaptureChannel, CheckpointGeneration, ChildRole, CloseId, Epoch,
    LifecycleRole, LineageId, MemberOrdinal, OfdId, OfdNamespace, ParentRole, ProcessIdentity, ProcessKey, Publication,
    ResourceSnapshot, SavedProcessIdentity, TicketId,
};
pub(crate) use restore::RestoreAdmission;
pub(crate) use ticket::{Authority, ForkAdmission};

#[cfg(test)]
mod test;
