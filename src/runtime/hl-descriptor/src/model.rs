//! Guest descriptor-number ownership.
//!
//! A [`crate::DescriptorTable`] owns descriptor numbers and descriptor-local flags.
//! The object referenced by a descriptor is an [`crate::OpenFileDescription`], shared
//! through an [`Arc`]. Duplicating a descriptor therefore shares description
//! state (including offsets and status flags) while keeping `close_on_exec`
//! local to each descriptor.
use crate::CheckpointActivity;
use crate::readiness::ReadinessSubscription;
use crate::{DescriptorFlags, LeaseKind, ObjectKind, OpenFileDescription, SignalOwner, StatusFlags};
use std::fmt::Debug;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
#[derive(Debug)]
pub(crate) struct DescriptionState {
    pub(crate) offset: u64,
    pub(crate) status: StatusFlags,
    pub(crate) owner: SignalOwner,
    pub(crate) signal: u8,
    pub(crate) lease: Option<LeaseKind>,
    pub(crate) write_life_hint: u64,
}
pub(crate) struct OpenDescription {
    pub(crate) object: Arc<dyn OpenFileDescription>,
    pub(crate) state: Mutex<DescriptionState>,
    pub(crate) identity: u64,
    pub(crate) generation: u32,
    pub(crate) descriptor_references: AtomicU32,
    transfer_references: AtomicU32,
    pub(crate) active_operations: AtomicU32,
    pub(crate) retired: AtomicBool,
    pub(crate) async_subscription: Mutex<Option<Box<dyn ReadinessSubscription>>>,
    pub(crate) notify_subscription: Mutex<Option<Box<dyn ReadinessSubscription>>>,
}

impl Debug for OpenDescription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OpenDescription")
            .field("identity", &self.identity)
            .field("generation", &self.generation)
            .field("retired", &self.retired.load(Ordering::Acquire))
            .finish_non_exhaustive()
    }
}
impl OpenDescription {
    pub(crate) fn new(object: Arc<dyn OpenFileDescription>, identity: u64, status: StatusFlags) -> Self {
        Self {
            object,
            state: Mutex::new(DescriptionState {
                offset: 0,
                status,
                owner: SignalOwner::Process(0),
                signal: 0,
                lease: None,
                write_life_hint: 0,
            }),
            identity,
            generation: 1,
            descriptor_references: AtomicU32::new(1),
            transfer_references: AtomicU32::new(0),
            active_operations: AtomicU32::new(0),
            retired: AtomicBool::new(false),
            async_subscription: Mutex::new(None),
            notify_subscription: Mutex::new(None),
        }
    }
    pub(crate) fn restored(
        object: Arc<dyn OpenFileDescription>,
        identity: u64,
        generation: u32,
        offset: u64,
        status: StatusFlags,
        references: u32,
    ) -> Self {
        Self {
            object,
            state: Mutex::new(DescriptionState {
                offset,
                status,
                owner: SignalOwner::Process(0),
                signal: 0,
                lease: None,
                write_life_hint: 0,
            }),
            identity,
            generation,
            descriptor_references: AtomicU32::new(references),
            transfer_references: AtomicU32::new(0),
            active_operations: AtomicU32::new(0),
            retired: AtomicBool::new(false),
            async_subscription: Mutex::new(None),
            notify_subscription: Mutex::new(None),
        }
    }
    pub(crate) fn retain_descriptor(&self) {
        self.descriptor_references.fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) fn release_descriptor(&self) {
        if self.descriptor_references.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.try_retire();
        }
    }
    pub(crate) fn retain_transfer(&self) {
        self.transfer_references.fetch_add(1, Ordering::Relaxed);
    }
    pub(crate) fn release_transfer(&self) {
        if self.transfer_references.fetch_sub(1, Ordering::AcqRel) == 1 {
            self.try_retire();
        }
    }
    fn try_retire(&self) {
        if self.descriptor_references.load(Ordering::Acquire) != 0
            || self.transfer_references.load(Ordering::Acquire) != 0
        {
            return;
        }
        if !self.retired.swap(true, Ordering::AcqRel) {
            let subscription = self
                .async_subscription
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            drop(subscription);
            let notification = self
                .notify_subscription
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take();
            drop(notification);
            self.object.retire();
        }
    }
}
/// Lifetime pin for an operation already admitted through a descriptor.
#[derive(Debug)]
pub struct OperationLease {
    pub(crate) description: Arc<OpenDescription>,
    pub(crate) descriptor_number: i32,
    pub(crate) descriptor_generation: u32,
    pub(crate) checkpoint: Arc<CheckpointActivity>,
    pub(crate) admitted: bool,
}
impl Drop for OpenDescription {
    fn drop(&mut self) {
        self.object.close();
    }
}
/// One descriptor's reference to an open file description.
#[derive(Clone, Debug)]
pub struct Descriptor {
    pub(crate) description: Arc<OpenDescription>,
    pub(crate) flags: DescriptorFlags,
    pub(crate) generation: u32,
    pub(crate) transfer_dependencies: Vec<crate::DescriptionRef>,
}
impl Descriptor {
    pub(crate) fn new(description: Arc<OpenDescription>, flags: DescriptorFlags, generation: u32) -> Self {
        Self {
            description,
            flags,
            generation,
            transfer_dependencies: Vec::new(),
        }
    }
    pub(crate) fn transferred(
        description: Arc<OpenDescription>,
        flags: DescriptorFlags,
        generation: u32,
        transfer_dependencies: Vec<crate::DescriptionRef>,
    ) -> Self {
        Self {
            description,
            flags,
            generation,
            transfer_dependencies,
        }
    }
    /// Borrows the shared open file description.
    #[must_use]
    #[cfg(test)]
    pub fn description(&self) -> &Arc<dyn OpenFileDescription> {
        &self.description.object
    }
    /// Returns this descriptor number's local flags.
    #[must_use]
    #[cfg(test)]
    pub const fn flags(&self) -> DescriptorFlags {
        self.flags
    }
}
/// Value-only view used by checkpointing and differential tests.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DescriptorSnapshot {
    pub number: i32,
    pub description_identity: u64,
    pub offset: u64,
    pub status: StatusFlags,
    pub flags: DescriptorFlags,
    pub descriptor_generation: u32,
    pub description_generation: u32,
    pub descriptor_references: u32,
    pub kind: ObjectKind,
    pub flock_token: u64,
}
/// Failures with stable Linux descriptor-table meaning.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DescriptorError {
    /// The source descriptor is not open (`EBADF`).
    BadDescriptor,
    /// An argument violates the operation's contract (`EINVAL`).
    InvalidArgument,
    /// No descriptor is available below the table limit (`EMFILE`).
    TooManyOpenFiles,
    /// The exact descriptor is already live or reserved.
    AlreadyExists,
    /// A reservation no longer matches the slot generation.
    StaleReservation,
    /// Internal ownership invariants are inconsistent.
    Corrupt,
    /// The table is quiesced for a checkpoint transaction.
    CheckpointFrozen,
}
/// Atomic operation selected for duplication onto an exact descriptor number.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ExactDuplicate {
    /// `dup2`: equal source and destination is a successful no-op.
    Dup2,
    /// `dup3`: equal source and destination is invalid.
    Dup3(DescriptorFlags),
}
