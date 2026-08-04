//! Guest descriptor-number and open-file-description ownership.
//!
//! [`DescriptorTable`] owns descriptor numbers and descriptor-local flags.
//! Duplicated descriptors share description state such as offsets and status
//! flags while retaining independent close-on-exec state.

#![forbid(unsafe_code)]

mod checkpoint;
mod checkpoint_activity;
mod description_state;
mod model;
mod object_contract;
mod ofd_values;
mod operation_lease;
mod readiness;
mod state;
mod table;
mod table_snapshot;
mod transfer;

pub use checkpoint::{
    DESCRIPTION_CHECKPOINT_BYTES_MAXIMUM, DESCRIPTOR_CHECKPOINT_MAXIMUM, DESCRIPTOR_CHECKPOINT_VERSION,
    DescriptorCheckpointError, DescriptorEntryImage, DescriptorGenerationImage, DescriptorObjectCheckpoint,
    DescriptorTableImage, OpenDescriptionImage,
};
pub(crate) use checkpoint_activity::CheckpointActivity;
pub(crate) use model::Descriptor;
pub use model::{
    AccessMode, AllocationRequest, DescriptorError, DescriptorFlags, DescriptorSnapshot, ExactDuplicate, LeaseKind,
    LinkableInode, OpenFileDescription, OperationLease, SeekPosition, SignalDelivery, SignalOwner, SignalSource,
    StatusFlags,
};
pub use object_contract::{
    CancellationNotification, CancellationSubscription, ObjectError, ObjectKind, OperationActor, OperationCancellation,
    OperationContext, PipeTransferEndpoint, PreparedAtomicRead, PreparedSpliceRead, Readiness,
};
pub use ofd_values::{DirectoryBatch, DirectoryBatchToken, OfdDirectoryEntry, OfdMetadata, OfdTimestamp};
pub use readiness::{DescriptionIdentity, ReadinessObserver, ReadinessRegistry, ReadinessSubscription};
pub use table::{DescriptorTable, FIRST_DESCRIPTOR, Reservation};
pub use transfer::{DescriptionInstallTransaction, DescriptionRef, PreparedDescriptorInstall, PreparedInstallBatch};

#[cfg(test)]
mod checkpoint_test;
#[cfg(test)]
mod readiness_test;
#[cfg(test)]
mod test;
#[cfg(test)]
mod transfer_test;
