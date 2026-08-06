use std::fmt;
use std::io::{IoSlice, IoSliceMut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::{
    AccessMode, ObjectError, ObjectKind, OpenFileDescription, PreparedSpliceRead, Readiness, ReadinessObserver,
    ReadinessSubscription, StatusFlags,
};

use crate::{
    AdvisoryLockCoordinator, FlockMode, FlockOwnerToken, GuestPathBytes, Identity, LockCancellation, LockError,
    Metadata,
};

use super::cursor::{Cursor, State};

/// Opaque host file identity valid until one matching close.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
#[repr(transparent)]
pub struct VfsFileToken(u64);

impl VfsFileToken {
    #[must_use]
    pub const fn from_raw(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn raw(self) -> u64 {
        self.0
    }
}

/// Host operations needed by one regular-file open description.
///
/// `append` must choose the current end and write as one atomic host operation.
/// Successful short operations are returned as `Ok(short_count)`. Interruption
/// without progress and nonblocking pressure remain distinct object errors.
pub trait VfsFileHost: Send + Sync + 'static {
    fn read_at(
        &self,
        file: VfsFileToken,
        offset: u64,
        output: &mut [u8],
        nonblocking: bool,
    ) -> Result<usize, ObjectError>;

    fn write_at(&self, file: VfsFileToken, offset: u64, input: &[u8], nonblocking: bool) -> Result<usize, ObjectError>;

    fn append(&self, file: VfsFileToken, input: &[u8], nonblocking: bool) -> Result<(usize, u64), ObjectError>;

    fn read_vector_at(
        &self,
        file: VfsFileToken,
        offset: u64,
        output: &mut [IoSliceMut<'_>],
        nonblocking: bool,
    ) -> Result<usize, ObjectError>;

    fn write_vector_at(
        &self,
        file: VfsFileToken,
        offset: u64,
        input: &[IoSlice<'_>],
        nonblocking: bool,
    ) -> Result<usize, ObjectError>;

    fn append_vector(
        &self,
        file: VfsFileToken,
        input: &[IoSlice<'_>],
        nonblocking: bool,
    ) -> Result<(usize, u64), ObjectError>;

    fn truncate(&self, file: VfsFileToken, size: u64) -> Result<(), ObjectError>;

    fn synchronize(&self, file: VfsFileToken, data_only: bool) -> Result<(), ObjectError>;

    fn metadata(&self, file: VfsFileToken) -> Result<Metadata, ObjectError>;

    fn readiness(&self, file: VfsFileToken, interests: Readiness) -> Readiness;

    fn subscribe(
        &self,
        file: VfsFileToken,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError>;

    /// Wakes blocking operations and prevents new host work for this token.
    fn cancel(&self, file: VfsFileToken);

    fn close(&self, file: VfsFileToken);
}

struct LockBinding {
    coordinator: Arc<AdvisoryLockCoordinator>,
    owner: FlockOwnerToken,
}

/// Regular-file implementation of the descriptor-owned object contract.
pub struct VfsFileDescription<H: VfsFileHost> {
    pub(super) host: Arc<H>,
    pub(super) token: VfsFileToken,
    identity: Identity,
    path: GuestPathBytes,
    access: AccessMode,
    pub(super) cursor: Arc<Cursor>,
    locks: Mutex<Option<LockBinding>>,
    pub(super) retired: AtomicBool,
    closed: AtomicBool,
}

impl<H: VfsFileHost> fmt::Debug for VfsFileDescription<H> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("VfsFileDescription")
            .field("token", &self.token)
            .field("identity", &self.identity)
            .field("path", &self.path)
            .field("access", &self.access)
            .finish_non_exhaustive()
    }
}

impl<H: VfsFileHost> VfsFileDescription<H> {
    #[must_use]
    pub fn new(
        host: H,
        token: VfsFileToken,
        identity: Identity,
        path: GuestPathBytes,
        access: AccessMode,
        status: StatusFlags,
    ) -> Self {
        Self {
            host: Arc::new(host),
            token,
            identity,
            path,
            access,
            cursor: Arc::new(Cursor::new(status)),
            locks: Mutex::new(None),
            retired: AtomicBool::new(false),
            closed: AtomicBool::new(false),
        }
    }

    #[must_use]
    pub const fn identity(&self) -> Identity {
        self.identity
    }

    #[must_use]
    pub fn path(&self) -> &GuestPathBytes {
        &self.path
    }

    #[must_use]
    pub fn offset(&self) -> u64 {
        self.lock_state().offset
    }

    pub fn seek(&self, position: SeekPosition) -> Result<u64, ObjectError> {
        self.ensure_live()?;
        if matches!(position, SeekPosition::Data(_) | SeekPosition::Hole(_)) {
            return Err(ObjectError::NotSupported);
        }
        let mut state = self.lock_cursor(None)?;
        let base = match position {
            SeekPosition::Start(offset) => return self.set_offset(&mut state, offset),
            SeekPosition::Current(_) => state.offset,
            SeekPosition::End(_) => self.host.metadata(self.token)?.size,
            SeekPosition::Data(_) | SeekPosition::Hole(_) => unreachable!("returned above"),
        };
        let delta = match position {
            SeekPosition::Start(_) => unreachable!("returned above"),
            SeekPosition::Current(delta) | SeekPosition::End(delta) => delta,
            SeekPosition::Data(_) | SeekPosition::Hole(_) => unreachable!("returned above"),
        };
        let offset = SeekPosition::apply_delta(base, delta).ok_or(ObjectError::InvalidArgument)?;
        self.set_offset(&mut state, offset)
    }

    pub fn pread(&self, offset: u64, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.ensure_readable()?;
        if output.is_empty() {
            return Ok(0);
        }
        let nonblocking = self.nonblocking();
        self.host.read_at(self.token, offset, output, nonblocking)
    }

    pub fn pwrite(&self, offset: u64, input: &[u8]) -> Result<usize, ObjectError> {
        self.ensure_writable()?;
        if input.is_empty() {
            return Ok(0);
        }
        let state = self.lock_state();
        if state.status.bits() & StatusFlags::APPEND != 0 {
            return self
                .host
                .append(self.token, input, state.status.bits() & StatusFlags::NONBLOCKING != 0)
                .map(|(written, _)| written);
        }
        self.host.write_at(
            self.token,
            offset,
            input,
            state.status.bits() & StatusFlags::NONBLOCKING != 0,
        )
    }

    pub fn truncate(&self, size: u64) -> Result<(), ObjectError> {
        self.ensure_writable()?;
        self.host.truncate(self.token, size)
    }

    pub fn synchronize(&self, data_only: bool) -> Result<(), ObjectError> {
        self.ensure_live()?;
        self.host.synchronize(self.token, data_only)
    }

    pub fn metadata(&self) -> Result<Metadata, ObjectError> {
        self.ensure_live()?;
        self.host.metadata(self.token)
    }

    /// Binds the OFD identity assigned by the descriptor/runtime layer.
    pub fn bind_flock_owner(
        &self,
        coordinator: Arc<AdvisoryLockCoordinator>,
        owner: FlockOwnerToken,
    ) -> Result<(), LockError> {
        let mut binding = self.locks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(existing) = binding.as_ref() {
            return if existing.owner == owner && Arc::ptr_eq(&existing.coordinator, &coordinator) {
                Ok(())
            } else {
                Err(LockError::InvalidArgument)
            };
        }
        *binding = Some(LockBinding { coordinator, owner });
        Ok(())
    }

    pub fn flock(
        &self,
        mode: Option<FlockMode>,
        blocking: bool,
        cancellation: &LockCancellation,
    ) -> Result<(), LockError> {
        let binding = self.locks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let binding = binding.as_ref().ok_or(LockError::InvalidArgument)?;
        binding
            .coordinator
            .set_flock(self.identity, binding.owner, mode, blocking, cancellation)
    }

    fn read_cursor(
        &self,
        output: &mut [u8],
        cancellation: Option<&dyn hl_descriptor::OperationCancellation>,
    ) -> Result<usize, ObjectError> {
        self.ensure_readable()?;
        if output.is_empty() {
            return Ok(0);
        }
        let mut state = self.lock_cursor(cancellation)?;
        let read = self.host.read_at(
            self.token,
            state.offset,
            output,
            state.status.bits() & StatusFlags::NONBLOCKING != 0,
        )?;
        state.offset = state
            .offset
            .checked_add(read as u64)
            .ok_or(ObjectError::InvalidArgument)?;
        Ok(read)
    }

    fn write_cursor(
        &self,
        input: &[u8],
        cancellation: Option<&dyn hl_descriptor::OperationCancellation>,
    ) -> Result<usize, ObjectError> {
        self.ensure_writable()?;
        if input.is_empty() {
            return Ok(0);
        }
        let mut state = self.lock_cursor(cancellation)?;
        let nonblocking = state.status.bits() & StatusFlags::NONBLOCKING != 0;
        if state.status.bits() & StatusFlags::APPEND != 0 {
            let (written, end) = self.host.append(self.token, input, nonblocking)?;
            state.offset = end;
            return Ok(written);
        }
        let written = self.host.write_at(self.token, state.offset, input, nonblocking)?;
        state.offset = state
            .offset
            .checked_add(written as u64)
            .ok_or(ObjectError::InvalidArgument)?;
        Ok(written)
    }

    fn set_offset(&self, state: &mut State, offset: u64) -> Result<u64, ObjectError> {
        state.offset = offset;
        Ok(offset)
    }

    fn nonblocking(&self) -> bool {
        self.lock_state().status.bits() & StatusFlags::NONBLOCKING != 0
    }

    pub(super) fn ensure_readable(&self) -> Result<(), ObjectError> {
        self.ensure_live()?;
        if self.access == AccessMode::WriteOnly {
            Err(ObjectError::BadDescriptor)
        } else {
            Ok(())
        }
    }

    pub(super) fn ensure_writable(&self) -> Result<(), ObjectError> {
        self.ensure_live()?;
        if self.access == AccessMode::ReadOnly {
            Err(ObjectError::BadDescriptor)
        } else {
            Ok(())
        }
    }

    fn ensure_live(&self) -> Result<(), ObjectError> {
        if self.retired.load(Ordering::Acquire) {
            Err(ObjectError::Retired)
        } else {
            Ok(())
        }
    }
}

impl<H: VfsFileHost> OpenFileDescription for VfsFileDescription<H> {
    fn domain_extension(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }
    fn copy_file_range(
        &self,
        target: &dyn OpenFileDescription,
        input_offset: Option<u64>,
        output_offset: Option<u64>,
        maximum: usize,
        nonblocking: bool,
        cancellation: Option<&dyn hl_descriptor::OperationCancellation>,
    ) -> Result<Option<(usize, u64, u64)>, ObjectError> {
        self.copy_to(target, input_offset, output_offset, maximum, nonblocking, cancellation)
            .map(Some)
    }
    fn kind(&self) -> ObjectKind {
        ObjectKind::File
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.read_cursor(output, None)
    }

    fn read_with_cancellation(
        &self,
        output: &mut [u8],
        cancellation: &dyn hl_descriptor::OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.read_cursor(output, Some(cancellation))
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.write_cursor(input, None)
    }

    fn write_with_cancellation(
        &self,
        input: &[u8],
        cancellation: &dyn hl_descriptor::OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.write_cursor(input, Some(cancellation))
    }

    fn read_vector_context(
        &self,
        output: &mut [IoSliceMut<'_>],
        context: hl_descriptor::OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        self.read_vector(output, context.cancellation)
    }

    fn write_vector_context(
        &self,
        input: &[IoSlice<'_>],
        context: hl_descriptor::OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        self.write_vector(input, context.cancellation)
    }

    fn read_vector_at(&self, offset: u64, output: &mut [IoSliceMut<'_>]) -> Result<usize, ObjectError> {
        self.ensure_readable()?;
        self.host.read_vector_at(self.token, offset, output, self.nonblocking())
    }

    fn write_vector_at(&self, offset: u64, input: &[IoSlice<'_>]) -> Result<usize, ObjectError> {
        self.ensure_writable()?;
        let state = self.lock_state();
        let nonblocking = state.status.bits() & StatusFlags::NONBLOCKING != 0;
        if state.status.bits() & StatusFlags::APPEND != 0 {
            return self
                .host
                .append_vector(self.token, input, nonblocking)
                .map(|(written, _)| written);
        }
        self.host.write_vector_at(self.token, offset, input, nonblocking)
    }

    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.pread(offset, output)
    }

    fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, ObjectError> {
        self.pwrite(offset, input)
    }

    fn prepare_splice_read(
        &self,
        offset: Option<u64>,
        maximum: usize,
        nonblocking: bool,
        cancellation: Option<&dyn hl_descriptor::OperationCancellation>,
    ) -> Result<Option<Box<dyn PreparedSpliceRead>>, ObjectError> {
        super::splice::VfsSpliceRead::prepare(self, offset, maximum, nonblocking, cancellation)
            .map(|prepared| Some(Box::new(prepared) as Box<dyn PreparedSpliceRead>))
    }

    fn seek(&self, position: hl_descriptor::SeekPosition) -> Result<u64, ObjectError> {
        let position = match position {
            hl_descriptor::SeekPosition::Start(value) => SeekPosition::Start(value),
            hl_descriptor::SeekPosition::Current(value) => SeekPosition::Current(value),
            hl_descriptor::SeekPosition::End(value) => SeekPosition::End(value),
            hl_descriptor::SeekPosition::Data(value) => SeekPosition::Data(value),
            hl_descriptor::SeekPosition::Hole(value) => SeekPosition::Hole(value),
        };
        VfsFileDescription::seek(self, position)
    }

    fn metadata(&self) -> Result<hl_descriptor::OfdMetadata, ObjectError> {
        let value = VfsFileDescription::metadata(self)?;
        Ok(super::adapter::MetadataAdapter::descriptor(value))
    }

    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        self.ensure_live()?;
        self.lock_state().status = flags;
        Ok(())
    }

    fn readiness(&self, interests: Readiness) -> Readiness {
        if self.retired.load(Ordering::Acquire) {
            return Readiness::from_bits(Readiness::ERROR | Readiness::HANGUP);
        }
        self.host.readiness(self.token, interests)
    }

    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.ensure_live()?;
        self.host.subscribe(self.token, observer)
    }

    fn retire(&self) {
        if self.retired.swap(true, Ordering::AcqRel) {
            return;
        }
        self.release_flock();
        self.host.cancel(self.token);
        self.cursor.wake();
    }

    fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.host.close(self.token);
        }
    }
}

impl<H: VfsFileHost> VfsFileDescription<H> {
    fn release_flock(&self) {
        let binding = self.locks.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some(binding) = binding.as_ref() else {
            return;
        };
        binding.coordinator.close_ofd(binding.owner);
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SeekPosition {
    Start(u64),
    Current(i64),
    End(i64),
    Data(u64),
    Hole(u64),
}

impl SeekPosition {
    fn apply_delta(base: u64, delta: i64) -> Option<u64> {
        if delta >= 0 {
            base.checked_add(delta as u64)
        } else {
            base.checked_sub(delta.unsigned_abs())
        }
    }
}
