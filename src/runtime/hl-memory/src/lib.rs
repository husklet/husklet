//! Guest virtual-memory ledger.
//!
//! The ledger models guest-visible mappings only. It does not allocate host
//! memory, call `mmap`, or contain host pointers.

#![forbid(unsafe_code)]

mod atomic;
mod atomic_access;
mod atomic_batch;
mod backing;
mod checkpoint;
mod checkpoint_activity;
mod executable;
mod ledger;
mod mapping;
mod model;
mod object_store;
mod placement;
mod region_set;
mod reservation;
mod shared_model;
pub(crate) use checkpoint_activity::CheckpointActivity;
#[cfg(feature = "test-support")]
mod test_host;

pub use atomic::{AtomicOperation, AtomicOrder, AtomicValue, ExclusiveReservation};
pub use atomic_batch::{
    ATOMIC_U32_WRITE_BATCH_MAXIMUM, AtomicBatchHost, AtomicU32Write, PreparedAtomicBatch, SharedAtomicBatch,
};
pub use atomic_batch::{AtomicBatchHost as AtomicWriteBatchHost, PreparedAtomicBatch as PreparedAtomicU32Batch};
pub use backing::{SharedBacking, SharedBackingFactory};
pub use checkpoint::{
    FrozenSnapshotAuthority, MEMORY_ADDRESS_MAXIMUM, MEMORY_CHECKPOINT_BYTES_MAXIMUM, MEMORY_CHECKPOINT_REGION_MAXIMUM,
    MEMORY_CHECKPOINT_VERSION, MemoryCheckpointHost, MemoryCheckpointImage, MemoryHostRestore, MemoryHostStage,
    MemoryMappingSnapshot,
};
pub use checkpoint_activity::CheckpointContinuation;
pub use executable::ExecutableToken;
pub use ledger::{ExecutableAliasEvidence, MemoryLedger, MemoryLedgerSnapshot};
pub use mapping::TransitionObserver as MappingTransitionObserver;
pub use mapping::{
    ApertureLease, Batch as MappingBatch, Coordinator as MappingCoordinator, DIRTY_RANGE_MAXIMUM, DirectAuthorityLease,
    ExternalSpan, Host as MappingHost, HostAperture, HostProjection, LIVE_PROJECTION_MAXIMUM, MemoryAccessHost,
    Operation as MappingOperation, ProjectionGeneration, ProjectionLease, ProjectionView, RequestContinuation,
    VmaSnapshotLease, VmaSnapshotRecord, WritePublication, WriteReservation, WriteSpanTransaction, WriteTransaction,
};
pub use mapping::{BackingChange, BackingChangeFlags, BackingChangeHost};
pub use mapping::{ExitHost as ExitMappingHost, PreparedAddressExit, PreparedHostExit};
pub use model::{
    AddressSpaceId, Backing, FileIdentity, FutexAccess, FutexIdentity, MapRequest, MemoryError, Placement, Protection,
    Region, Resolution,
};
pub use object_store::{SharedBackingPin, SharedObjectStore};
pub use reservation::{ReservationCoordinate, ReservationEpochs};
pub use shared_model::{
    SharedBackingRef, SharedError, SharedLimits, SharedObjectId, SharedObjectSnapshot, SharedSeal, SharedStoreSnapshot,
};
#[cfg(feature = "test-support")]
pub use test_host::TestMappingHost;

#[cfg(test)]
mod atomic_access_test;
#[cfg(test)]
mod batch_test;
#[cfg(test)]
mod checkpoint_test;
#[cfg(test)]
mod host_test;
#[cfg(test)]
mod shared_test;
#[cfg(test)]
mod test;
