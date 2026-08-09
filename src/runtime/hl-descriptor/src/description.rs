//! The open-file-description contract and the value vocabulary its operations take.
use crate::readiness::{ReadinessObserver, ReadinessSubscription};
use crate::{DirectoryBatch, DirectoryBatchToken, OfdMetadata, StatusFlags};
use crate::{
    ObjectError, ObjectKind, OperationCancellation, OperationContext, PipeTransferEndpoint, PreparedAtomicRead,
    PreparedSpliceRead, Readiness,
};
use std::any::Any;
use std::fmt::Debug;
use std::io::{IoSlice, IoSliceMut};
use std::sync::Arc;

/// The segment list a vectored operation is given.
struct Segments;

impl Segments {
    /// Concatenates the segments of a vectored write so a description without its own
    /// vectored path still delivers the whole request instead of one short segment.
    /// Returns `None` when every segment is empty.
    fn gather(input: &[IoSlice<'_>]) -> Result<Option<Vec<u8>>, ObjectError> {
        let length = input
            .iter()
            .try_fold(0_usize, |total, part| total.checked_add(part.len()))
            .ok_or(ObjectError::ResourceLimit)?;
        if length == 0 {
            return Ok(None);
        }
        let mut buffer = Vec::with_capacity(length);
        for part in input {
            buffer.extend_from_slice(part);
        }
        Ok(Some(buffer))
    }
}

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
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn linkable_inode(&self) -> Result<Option<Arc<dyn LinkableInode>>, ObjectError> {
        Ok(None)
    }
    /// Identifies the concrete object's broad readiness/lifecycle family.
    fn kind(&self) -> ObjectKind {
        ObjectKind::Other
    }
    /// Performs one object read after descriptor admission and OFD pinning.
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn read(&self, _output: &mut [u8]) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn read_with_cancellation(
        &self,
        output: &mut [u8],
        _cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.read(output)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn read_context(&self, output: &mut [u8], context: OperationContext<'_>) -> Result<usize, ObjectError> {
        match context.cancellation {
            Some(cancellation) => self.read_with_cancellation(output, cancellation),
            None => self.read(output),
        }
    }
    /// Observes whether a read would copy data without consuming the source.
    /// `None` means this object cannot provide a non-destructive observation.
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn probe_read(&self, _maximum: usize) -> Result<Option<usize>, ObjectError> {
        Ok(None)
    }
    /// Resolves writes whose sink does not consume source bytes. Returning
    /// `Some` lets Linux preserve descriptor and length validation without
    /// faulting or materializing an otherwise-unused guest buffer.
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn probe_write(&self, _maximum: usize) -> Result<Option<usize>, ObjectError> {
        Ok(None)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn prepare_atomic_read(&self, _maximum: usize) -> Result<Option<Box<dyn PreparedAtomicRead>>, ObjectError> {
        Ok(None)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn prepare_atomic_context(
        &self,
        maximum: usize,
        _context: OperationContext<'_>,
    ) -> Result<Option<Box<dyn PreparedAtomicRead>>, ObjectError> {
        self.prepare_atomic_read(maximum)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn prepare_splice_read(
        &self,
        _offset: Option<u64>,
        _maximum: usize,
        _nonblocking: bool,
        _cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<Option<Box<dyn PreparedSpliceRead>>, ObjectError> {
        Ok(None)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
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
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn write(&self, _input: &[u8]) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn write_with_cancellation(
        &self,
        input: &[u8],
        _cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.write(input)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn write_context(&self, input: &[u8], context: OperationContext<'_>) -> Result<usize, ObjectError> {
        match context.cancellation {
            Some(cancellation) => self.write_with_cancellation(input, cancellation),
            None => self.write(input),
        }
    }
    /// Reports whether a zero-length write is a meaningful operation on this object rather than the
    /// no-op the write path returns for everything else; Linux delivers it to `/proc/<pid>/comm`.
    fn accepts_empty_write(&self) -> bool {
        false
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn read_vector_context(
        &self,
        _output: &mut [IoSliceMut<'_>],
        _context: OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn write_vector_context(&self, input: &[IoSlice<'_>], context: OperationContext<'_>) -> Result<usize, ObjectError> {
        let Some(buffer) = Segments::gather(input)? else {
            return Ok(0);
        };
        self.write_context(&buffer, context)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn read_vector_at(&self, _offset: u64, _output: &mut [IoSliceMut<'_>]) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn write_vector_at(&self, offset: u64, input: &[IoSlice<'_>]) -> Result<usize, ObjectError> {
        let Some(buffer) = Segments::gather(input)? else {
            return Ok(0);
        };
        self.write_at(offset, &buffer)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn read_at(&self, _offset: u64, _output: &mut [u8]) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn write_at(&self, _offset: u64, _input: &[u8]) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn seek(&self, _position: SeekPosition) -> Result<u64, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    /// Whether this object's resolving mount is a host-passthrough bind or volume,
    /// making its inode reachable from other containers in the same daemon process.
    ///
    /// Only host-backed files can answer yes; image layers, tmpfs, procfs and
    /// synthetic objects are all confined to one container.
    fn shared_domain(&self) -> bool {
        false
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn truncate(&self, _size: u64) -> Result<(), ObjectError> {
        Err(ObjectError::NotSupported)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn allocate(&self, _request: AllocationRequest) -> Result<(), ObjectError> {
        Err(ObjectError::NotSupported)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn flock(&self, _operation: u32, _cancellation: &dyn OperationCancellation) -> Result<(), ObjectError> {
        Err(ObjectError::NotSupported)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn synchronize(&self, _data_only: bool) -> Result<(), ObjectError> {
        Err(ObjectError::NotSupported)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn read_directory(&self, _maximum: usize) -> Result<DirectoryBatch, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn commit_directory(&self, _token: DirectoryBatchToken, _count: usize) -> Result<(), ObjectError> {
        Err(ObjectError::NotSupported)
    }
    /// Applies the settable OFD status flags transactionally.
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn set_status_flags(&self, _flags: StatusFlags) -> Result<(), ObjectError> {
        Ok(())
    }
    /// Returns pipe capacity in bytes, or `NotSupported` for non-pipes.
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn pipe_capacity(&self) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    /// Maximum write size whose payload is admitted atomically, if any.
    fn atomic_write_limit(&self) -> Option<usize> {
        None
    }
    /// Applies a pipe-domain capacity request and returns the rounded capacity.
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn set_pipe_capacity(&self, _requested: usize) -> Result<usize, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
    fn add_seals(&self, _seals: u8) -> Result<u8, ObjectError> {
        Err(ObjectError::NotSupported)
    }
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
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
    ///
    /// # Errors
    /// Returns an error if the object does not support the operation or the operation fails.
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
