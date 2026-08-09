use std::io::{IoSlice, IoSliceMut};
use std::sync::Arc;

use hl_descriptor::{
    ObjectError, ObjectKind, OfdMetadata, OfdTimestamp, OpenFileDescription, OperationCancellation, Readiness,
    ReadinessObserver, ReadinessSubscription, StatusFlags,
};

use crate::PipeEndpoint;

impl OpenFileDescription for PipeEndpoint {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Pipe
    }

    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        let status = self.status();
        let timestamp = OfdTimestamp {
            seconds: 0,
            nanoseconds: 0,
        };
        Ok(OfdMetadata {
            device: 0,
            inode: 0,
            kind: ((status.mode >> 12) & 0xf) as u8,
            permissions: (status.mode & 0o7777) as u16,
            links: status.link_count,
            user: 0,
            group: 0,
            special_device: 0,
            size: status.size,
            blocks_512: 0,
            block_size: 4096,
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
        })
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.read_bytes(output, None)
    }

    fn read_with_cancellation(
        &self,
        output: &mut [u8],
        cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.read_bytes(output, Some(cancellation))
    }

    fn probe_read(&self, maximum: usize) -> Result<Option<usize>, ObjectError> {
        Ok(Some(self.buffered_len().min(maximum)))
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.write_bytes(input, None)
    }

    fn write_with_cancellation(
        &self,
        input: &[u8],
        cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.write_bytes(input, Some(cancellation))
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

    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        self.set_nonblocking(flags.bits() & StatusFlags::NONBLOCKING != 0);
        Ok(())
    }

    fn pipe_capacity(&self) -> Result<usize, ObjectError> {
        Ok(self.capacity())
    }

    fn atomic_write_limit(&self) -> Option<usize> {
        Some(super::PIPE_BUF)
    }

    fn set_pipe_capacity(&self, requested: usize) -> Result<usize, ObjectError> {
        self.resize_capacity(requested)
    }

    fn pipe_transfer_endpoint(&self) -> Option<&dyn hl_descriptor::PipeTransferEndpoint> {
        Some(self)
    }

    fn prepare_atomic_context(
        &self,
        maximum: usize,
        context: hl_descriptor::OperationContext<'_>,
    ) -> Result<Option<Box<dyn hl_descriptor::PreparedAtomicRead>>, ObjectError> {
        let prepared = super::splice::PreparedRead::prepare(
            self,
            maximum,
            self.nonblocking.load(std::sync::atomic::Ordering::Acquire),
            context.cancellation,
            true,
        )?;
        Ok(Some(prepared))
    }

    fn prepare_splice_read(
        &self,
        offset: Option<u64>,
        maximum: usize,
        nonblocking: bool,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<Option<Box<dyn hl_descriptor::PreparedSpliceRead>>, ObjectError> {
        if offset.is_some() {
            return Err(ObjectError::InvalidArgument);
        }
        let prepared = super::splice::PreparedRead::prepare(self, maximum, nonblocking, cancellation, false)?;
        Ok(Some(prepared))
    }

    fn readiness(&self, interests: Readiness) -> Readiness {
        self.endpoint_readiness(interests)
    }

    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.endpoint_registry().subscribe(observer)
    }

    fn retire(&self) {
        self.retire_endpoint();
        self.close_endpoint();
    }

    fn close(&self) {
        self.close_endpoint();
    }
}

impl hl_descriptor::PipeTransferEndpoint for PipeEndpoint {
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}
