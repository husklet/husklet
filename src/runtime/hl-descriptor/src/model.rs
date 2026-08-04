//! Guest descriptor-number ownership.
//!
//! A [`crate::DescriptorTable`] owns descriptor numbers and descriptor-local flags.
//! The object referenced by a descriptor is an [`OpenFileDescription`], shared
//! through an [`Arc`]. Duplicating a descriptor therefore shares description
//! state (including offsets and status flags) while keeping `close_on_exec`
//! local to each descriptor.
use crate::CheckpointActivity;
use crate::readiness::{ReadinessObserver, ReadinessSubscription};
use crate::{DirectoryBatch, DirectoryBatchToken, OfdMetadata};
use crate::{
    ObjectError, ObjectKind, OperationCancellation, OperationContext, PipeTransferEndpoint, PreparedAtomicRead,
    PreparedSpliceRead, Readiness,
};
use std::any::Any;
use std::fmt::Debug;
use std::io::{IoSlice, IoSliceMut};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
/// An object whose lifetime follows Linux open-file-description semantics.
///
/// File, socket, pipe, and event domains implement this marker for their
/// description objects. Operations remain owned by those domains; this crate
/// only controls descriptor references and their lifetime.
pub trait OpenFileDescription: Debug + Send + Sync + 'static {
    /// Returns OFDs whose lifetime accompanies this object when it is installed
    /// through descriptor transfer.
    fn transfer_dependencies(&self) -> Vec<crate::DescriptionRef> {
        Vec::new()
    }

    /// Exposes an optional domain-owned extension without teaching the
    /// descriptor layer about concrete object families.
    fn domain_extension(&self) -> Option<&dyn Any> {
        None
    }
    /// Returns a pathname-independent inode capability when this description
    /// may be used as a `linkat(AT_EMPTY_PATH)` source.
    fn linkable_inode(&self) -> Result<Option<Arc<dyn LinkableInode>>, ObjectError> {
        Ok(None)
    }
    /// Identifies the concrete object's broad readiness/lifecycle family.
    fn kind(&self) -> ObjectKind {
        ObjectKind::Other
    }
    /// Performs one object read after descriptor admission and OFD pinning.
    fn read(&self, _output: &mut [u8]) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn read_with_cancellation(
        &self,
        output: &mut [u8],
        _cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.read(output)
    }
    fn read_context(&self, output: &mut [u8], context: OperationContext<'_>) -> Result<usize, ObjectError> {
        match context.cancellation {
            Some(cancellation) => self.read_with_cancellation(output, cancellation),
            None => self.read(output),
        }
    }
    /// Observes whether a read would copy data without consuming the source.
    /// `None` means this object cannot provide a non-destructive observation.
    fn probe_read(&self, _maximum: usize) -> Result<Option<usize>, ObjectError> {
        Ok(None)
    }
    /// Resolves writes whose sink does not consume source bytes. Returning
    /// `Some` lets Linux preserve descriptor and length validation without
    /// faulting or materializing an otherwise-unused guest buffer.
    fn probe_write(&self, _maximum: usize) -> Result<Option<usize>, ObjectError> {
        Ok(None)
    }
    fn prepare_atomic_read(&self, _maximum: usize) -> Result<Option<Box<dyn PreparedAtomicRead>>, ObjectError> {
        Ok(None)
    }
    fn prepare_atomic_context(
        &self,
        maximum: usize,
        _context: OperationContext<'_>,
    ) -> Result<Option<Box<dyn PreparedAtomicRead>>, ObjectError> {
        self.prepare_atomic_read(maximum)
    }
    fn prepare_splice_read(
        &self,
        _offset: Option<u64>,
        _maximum: usize,
        _nonblocking: bool,
        _cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<Option<Box<dyn PreparedSpliceRead>>, ObjectError> {
        Ok(None)
    }
    fn copy_file_range(
        &self,
        _target: &dyn OpenFileDescription,
        _input_offset: Option<u64>,
        _output_offset: Option<u64>,
        _maximum: usize,
        _nonblocking: bool,
        _cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<Option<(usize, u64, u64)>, ObjectError> {
        Ok(None)
    }
    /// Performs one object write after descriptor admission and OFD pinning.
    fn write(&self, _input: &[u8]) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn write_with_cancellation(
        &self,
        input: &[u8],
        _cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.write(input)
    }
    fn write_context(&self, input: &[u8], context: OperationContext<'_>) -> Result<usize, ObjectError> {
        match context.cancellation {
            Some(cancellation) => self.write_with_cancellation(input, cancellation),
            None => self.write(input),
        }
    }
    fn read_vector_context(
        &self,
        _output: &mut [IoSliceMut<'_>],
        _context: OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn write_vector_context(
        &self,
        _input: &[IoSlice<'_>],
        _context: OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn read_vector_at(&self, _offset: u64, _output: &mut [IoSliceMut<'_>]) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn write_vector_at(&self, _offset: u64, _input: &[IoSlice<'_>]) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn read_at(&self, _offset: u64, _output: &mut [u8]) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn write_at(&self, _offset: u64, _input: &[u8]) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn seek(&self, _position: SeekPosition) -> Result<u64, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn truncate(&self, _size: u64) -> Result<(), ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn allocate(&self, _request: AllocationRequest) -> Result<(), ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn flock(&self, _operation: u32, _cancellation: &dyn OperationCancellation) -> Result<(), ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn synchronize(&self, _data_only: bool) -> Result<(), ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn read_directory(&self, _maximum: usize) -> Result<DirectoryBatch, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn commit_directory(&self, _token: DirectoryBatchToken, _count: usize) -> Result<(), ObjectError> {
        Err(ObjectError::NotSupported)
    }
    /// Applies the settable OFD status flags transactionally.
    fn set_status_flags(&self, _flags: StatusFlags) -> Result<(), ObjectError> {
        Ok(())
    }
    /// Returns pipe capacity in bytes, or `NotSupported` for non-pipes.
    fn pipe_capacity(&self) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    /// Maximum write size whose payload is admitted atomically, if any.
    fn atomic_write_limit(&self) -> Option<usize> {
        None
    }
    /// Applies a pipe-domain capacity request and returns the rounded capacity.
    fn set_pipe_capacity(&self, _requested: usize) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn add_seals(&self, _seals: u8) -> Result<u8, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn seals(&self) -> Result<u8, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    fn pipe_transfer_endpoint(&self) -> Option<&dyn PipeTransferEndpoint> {
        None
    }
    /// Reports readiness, retaining error/hangup even when not requested.
    fn readiness(&self, _interests: Readiness) -> Readiness {
        Readiness::default()
    }
    /// Registers an observer for readiness transitions.
    ///
    /// Implementations never invoke the observer while holding object-state
    /// locks. The returned registration owns callback lifetime.
    fn subscribe_readiness(
        &self,
        _observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    /// Begins nonblocking retirement after the last descriptor reference goes.
    fn retire(&self) {}
    /// Releases the concrete resource exactly once after outstanding references
    /// and operation leases have drained.
    fn close(&self) {}
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AllocationRequest {
    pub mode: u32,
    pub offset: u64,
    pub length: u64,
}

/// Opaque, lifetime-owning inode capability consumed by a concrete VFS host.
pub trait LinkableInode: Any + Debug + Send + Sync {
    fn as_any(&self) -> &dyn Any;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeekPosition {
    Start(u64),
    Current(i64),
    End(i64),
    Data(u64),
    Hole(u64),
}
/// Access permitted by an open file description.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AccessMode {
    ReadOnly,
    WriteOnly,
    ReadWrite,
}
/// Linux lease type retained by an open file description.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LeaseKind {
    Read,
    Write,
}
/// Linux signal-delivery owner retained by an open file description.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SignalOwner {
    Process(i32),
    Group(i32),
    Thread(i32),
}

/// Current signal-driven I/O delivery state for one open description.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SignalDelivery {
    pub owner: SignalOwner,
    pub signal: u8,
}

/// Weak observation capability used by an OFD-owned readiness callback.
#[derive(Clone, Debug)]
pub struct SignalSource(pub(crate) std::sync::Weak<OpenDescription>);

impl SignalSource {
    /// Returns current ownership independently of the `O_ASYNC` flag.
    #[must_use]
    pub fn notification(&self) -> Option<SignalDelivery> {
        let description = self.0.upgrade()?;
        if description.retired.load(Ordering::Acquire) {
            return None;
        }
        let state = description.state.lock().unwrap_or_else(|error| error.into_inner());
        Some(SignalDelivery {
            owner: state.owner,
            signal: state.signal,
        })
    }

    /// Returns delivery state only while signal-driven I/O remains armed.
    #[must_use]
    pub fn delivery(&self) -> Option<SignalDelivery> {
        let description = self.0.upgrade()?;
        if description.retired.load(Ordering::Acquire) {
            return None;
        }
        let state = description.state.lock().unwrap_or_else(|error| error.into_inner());
        state.status.contains(StatusFlags::ASYNC).then_some(SignalDelivery {
            owner: state.owner,
            signal: state.signal,
        })
    }
}

impl SignalOwner {
    #[must_use]
    pub const fn from_legacy(owner: i32) -> Self {
        if owner < 0 {
            Self::Group(owner.saturating_abs())
        } else {
            Self::Process(owner)
        }
    }

    #[must_use]
    pub const fn legacy(self) -> i32 {
        match self {
            Self::Process(identity) | Self::Thread(identity) => identity,
            Self::Group(identity) => identity.saturating_neg(),
        }
    }
}
/// Mutable flags shared by every descriptor referencing one description.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct StatusFlags(u32);
impl StatusFlags {
    pub const ACCESS_MODE_MASK: u32 = 0o00000003;
    pub const APPEND: u32 = 0o00002000;
    pub const NONBLOCKING: u32 = 0o00004000;
    pub const DIRECT: u32 = 0o00040000;
    pub const ASYNC: u32 = 0o00020000;
    pub const PATH_ONLY: u32 = 0o10000000;
    pub const SETTABLE_MASK: u32 = Self::APPEND | Self::NONBLOCKING | Self::DIRECT | Self::ASYNC;
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
    #[must_use]
    pub const fn contains(self, bit: u32) -> bool {
        self.0 & bit != 0
    }
    #[must_use]
    pub const fn update_from_fcntl(self, requested: u32) -> Self {
        Self((self.0 & (Self::ACCESS_MODE_MASK | Self::PATH_ONLY)) | (requested & Self::SETTABLE_MASK))
    }
}
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
                .unwrap_or_else(|error| error.into_inner())
                .take();
            drop(subscription);
            let notification = self
                .notify_subscription
                .lock()
                .unwrap_or_else(|error| error.into_inner())
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
/// Flags attached to one descriptor number rather than to its description.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct DescriptorFlags(u32);
impl DescriptorFlags {
    pub const CLOSE_ON_EXEC: u32 = 1;
    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }
    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }
    #[must_use]
    pub const fn from_fcntl(bits: u32) -> Self {
        Self(bits & Self::CLOSE_ON_EXEC)
    }
    #[must_use]
    pub const fn closes_on_exec(self) -> bool {
        self.0 & Self::CLOSE_ON_EXEC != 0
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
