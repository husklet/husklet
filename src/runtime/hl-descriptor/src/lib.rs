//! Guest descriptor-number and open-file-description ownership.
//!
//! [`DescriptorTable`] owns descriptor numbers and descriptor-local flags.
//! Duplicated descriptors share description state such as offsets and status
//! flags while retaining independent close-on-exec state.

#![forbid(unsafe_code)]

mod checkpoint;
mod checkpoint_activity;
mod description;
mod description_state;
mod flags;
mod model;
mod object_contract;
mod ofd_values;
mod operation_lease;
mod readiness;
mod signal_delivery;
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
pub use description::{AccessMode, AllocationRequest, LeaseKind, LinkableInode, OpenFileDescription, SeekPosition};
pub use flags::{DescriptorFlags, StatusFlags};
pub(crate) use model::Descriptor;
pub use model::{DescriptorError, DescriptorSnapshot, ExactDuplicate, OperationLease};
pub use object_contract::{
    CancellationNotification, CancellationSubscription, ObjectError, ObjectKind, OperationActor, OperationCancellation,
    OperationContext, PipeTransferEndpoint, PreparedAtomicRead, PreparedSpliceRead, Readiness,
};
pub use ofd_values::{DirectoryBatch, DirectoryBatchToken, OfdDirectoryEntry, OfdMetadata, OfdTimestamp};
pub use readiness::{DescriptionIdentity, ReadinessObserver, ReadinessRegistry, ReadinessSubscription};
pub use signal_delivery::{SignalDelivery, SignalOwner, SignalSource};
pub use table::{DescriptorTable, FIRST_DESCRIPTOR, Reservation};
pub use table_snapshot::{SnapshotBudget, SnapshotError};
pub use transfer::{DescriptionInstallTransaction, DescriptionRef, PreparedDescriptorInstall, PreparedInstallBatch};

#[cfg(test)]
mod checkpoint_test;
#[cfg(test)]
mod readiness_test;
#[cfg(test)]
mod test;
#[cfg(test)]
mod transfer_test;
