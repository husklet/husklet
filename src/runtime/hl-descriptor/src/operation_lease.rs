use std::io::{IoSlice, IoSliceMut};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::{
    DescriptionIdentity, DirectoryBatch, DirectoryBatchToken, ObjectError, OfdMetadata, OpenFileDescription,
    OperationCancellation, OperationLease, PipeTransferEndpoint, PreparedAtomicRead, PreparedSpliceRead, Readiness,
    ReadinessObserver, ReadinessSubscription, SeekPosition, StatusFlags,
};

impl Clone for OperationLease {
    fn clone(&self) -> Self {
        if self.admitted {
            self.checkpoint
                .retain()
                .expect("an admitted descriptor lease keeps checkpoint activity open");
        }
        self.description.active_operations.fetch_add(1, Ordering::AcqRel);
        Self {
            description: Arc::clone(&self.description),
            descriptor_number: self.descriptor_number,
            descriptor_generation: self.descriptor_generation,
            checkpoint: Arc::clone(&self.checkpoint),
            admitted: self.admitted,
        }
    }
}

impl OperationLease {
    /// Creates a transfer-lifetime reference to this lease's OFD.
    #[must_use]
    pub fn transfer_reference(&self) -> crate::DescriptionRef {
        crate::DescriptionRef::shallow(Arc::clone(&self.description))
    }

    #[must_use]
    pub fn domain_extension(&self) -> Option<&dyn std::any::Any> {
        self.description.object.domain_extension()
    }

    /// Converts an admitted operation pin into a durable ownership pin.
    ///
    /// Long-lived readiness registrations retain the OFD but are not active
    /// guest operations and therefore must not prevent checkpoint quiescence.
    #[must_use]
    pub fn into_durable(mut self) -> Self {
        if self.admitted {
            self.checkpoint.release();
            self.admitted = false;
        }
        self
    }

    #[must_use]
    pub fn description_identity(&self) -> DescriptionIdentity {
        DescriptionIdentity {
            identity: self.description.identity,
            generation: self.description.generation,
        }
    }

    #[must_use]
    pub const fn descriptor_number(&self) -> i32 {
        self.descriptor_number
    }

    #[must_use]
    pub const fn descriptor_generation(&self) -> u32 {
        self.descriptor_generation
    }

    #[must_use]
    pub fn retired(&self) -> bool {
        self.description.retired.load(Ordering::Acquire)
    }

    #[must_use]
    pub fn object(&self) -> &dyn OpenFileDescription {
        self.description.object.as_ref()
    }

    #[must_use]
    pub fn offset(&self) -> u64 {
        self.description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .offset
    }

    pub fn set_offset(&self, offset: u64) {
        self.description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .offset = offset;
    }

    #[must_use]
    pub fn status(&self) -> StatusFlags {
        self.description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .status
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.description.object.read(output)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn read_with_cancellation(
        &self,
        output: &mut [u8],
        cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.description.object.read_with_cancellation(output, cancellation)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn read_context(&self, output: &mut [u8], context: crate::OperationContext<'_>) -> Result<usize, ObjectError> {
        self.description.object.read_context(output, context)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn probe_read(&self, maximum: usize) -> Result<Option<usize>, ObjectError> {
        self.description.object.probe_read(maximum)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn probe_write(&self, maximum: usize) -> Result<Option<usize>, ObjectError> {
        self.description.object.probe_write(maximum)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn prepare_atomic_read(&self, maximum: usize) -> Result<Option<Box<dyn PreparedAtomicRead>>, ObjectError> {
        self.description.object.prepare_atomic_read(maximum)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn prepare_atomic_context(
        &self,
        maximum: usize,
        context: crate::OperationContext<'_>,
    ) -> Result<Option<Box<dyn PreparedAtomicRead>>, ObjectError> {
        self.description.object.prepare_atomic_context(maximum, context)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn prepare_splice_read(
        &self,
        offset: Option<u64>,
        maximum: usize,
        nonblocking: bool,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<Option<Box<dyn PreparedSpliceRead>>, ObjectError> {
        self.description
            .object
            .prepare_splice_read(offset, maximum, nonblocking, cancellation)
    }
    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn copy_file_range(
        &self,
        target: &Self,
        input_offset: Option<u64>,
        output_offset: Option<u64>,
        maximum: usize,
        nonblocking: bool,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<Option<(usize, u64, u64)>, ObjectError> {
        self.description.object.copy_file_range(
            target.description.object.as_ref(),
            input_offset,
            output_offset,
            maximum,
            nonblocking,
            cancellation,
        )
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.description.object.write(input)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn write_with_cancellation(
        &self,
        input: &[u8],
        cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.description.object.write_with_cancellation(input, cancellation)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn write_context(&self, input: &[u8], context: crate::OperationContext<'_>) -> Result<usize, ObjectError> {
        self.description.object.write_context(input, context)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn read_vector_context(
        &self,
        output: &mut [IoSliceMut<'_>],
        context: crate::OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        self.description.object.read_vector_context(output, context)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn write_vector_context(
        &self,
        input: &[IoSlice<'_>],
        context: crate::OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        self.description.object.write_vector_context(input, context)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn read_vector_at(&self, offset: u64, output: &mut [IoSliceMut<'_>]) -> Result<usize, ObjectError> {
        self.description.object.read_vector_at(offset, output)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn write_vector_at(&self, offset: u64, input: &[IoSlice<'_>]) -> Result<usize, ObjectError> {
        self.description.object.write_vector_at(offset, input)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.description.object.read_at(offset, output)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, ObjectError> {
        self.description.object.write_at(offset, input)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn seek(&self, position: SeekPosition) -> Result<u64, ObjectError> {
        self.description.object.seek(position)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        self.description.object.metadata()
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn truncate(&self, size: u64) -> Result<(), ObjectError> {
        self.description.object.truncate(size)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn allocate(&self, request: crate::AllocationRequest) -> Result<(), ObjectError> {
        self.description.object.allocate(request)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn flock(&self, operation: u32, cancellation: &dyn crate::OperationCancellation) -> Result<(), ObjectError> {
        self.description.object.flock(operation, cancellation)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn synchronize(&self, data_only: bool) -> Result<(), ObjectError> {
        self.description.object.synchronize(data_only)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn read_directory(&self, maximum: usize) -> Result<DirectoryBatch, ObjectError> {
        self.description.object.read_directory(maximum)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn commit_directory(&self, token: DirectoryBatchToken, count: usize) -> Result<(), ObjectError> {
        self.description.object.commit_directory(token, count)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn set_status(&self, status: StatusFlags) -> Result<(), ObjectError> {
        self.description.object.set_status_flags(status)?;
        self.description
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .status = status;
        Ok(())
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn pipe_capacity(&self) -> Result<usize, ObjectError> {
        self.description.object.pipe_capacity()
    }

    #[must_use]
    pub fn atomic_write_limit(&self) -> Option<usize> {
        self.description.object.atomic_write_limit()
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn set_pipe_capacity(&self, requested: usize) -> Result<usize, ObjectError> {
        self.description.object.set_pipe_capacity(requested)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn add_seals(&self, seals: u8) -> Result<u8, ObjectError> {
        self.description.object.add_seals(seals)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn seals(&self) -> Result<u8, ObjectError> {
        self.description.object.seals()
    }

    #[must_use]
    pub fn pipe_transfer_endpoint(&self) -> Option<&dyn PipeTransferEndpoint> {
        self.description.object.pipe_transfer_endpoint()
    }

    #[must_use]
    pub fn readiness(&self, interests: Readiness) -> Readiness {
        self.description.object.readiness(interests)
    }

    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.description.object.subscribe_readiness(observer)
    }

    /// Returns a weak source suitable for an OFD-owned async callback.
    #[must_use]
    pub fn signal_source(&self) -> crate::SignalSource {
        crate::SignalSource(Arc::downgrade(&self.description))
    }

    /// Replaces the one signal-driven readiness registration owned by this OFD.
    ///
    /// # Errors
    /// Returns an error reported by the leased open-file description.
    pub fn set_async_observer(&self, observer: Option<Arc<dyn ReadinessObserver>>) -> Result<(), ObjectError> {
        let replacement = match observer {
            Some(observer) => Some(self.subscribe_readiness(observer)?),
            None => None,
        };
        let previous = {
            let mut registration = self
                .description
                .async_subscription
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::replace(&mut *registration, replacement)
        };
        drop(previous);
        Ok(())
    }

    /// Replaces the directory-notification registration owned by this OFD.
    pub fn set_notify_subscription(&self, replacement: Option<Box<dyn ReadinessSubscription>>) {
        let previous = {
            let mut registration = self
                .description
                .notify_subscription
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            std::mem::replace(&mut *registration, replacement)
        };
        drop(previous);
    }
}

impl Drop for OperationLease {
    fn drop(&mut self) {
        self.description.active_operations.fetch_sub(1, Ordering::AcqRel);
        if self.admitted {
            self.checkpoint.release();
        }
    }
}
