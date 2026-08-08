//! `OpenFileDescription` adapter over the regular-file description.

use std::io::{IoSlice, IoSliceMut};
use std::sync::Arc;
use std::sync::atomic::Ordering;

use hl_descriptor::{
    ObjectError, ObjectKind, OpenFileDescription, PreparedSpliceRead, Readiness, ReadinessObserver,
    ReadinessSubscription, StatusFlags,
};

use super::{SeekPosition, VfsFileDescription, VfsFileHost};

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
        crate::file::splice::VfsSpliceRead::prepare(self, offset, maximum, nonblocking, cancellation)
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
        Ok(crate::file::adapter::MetadataAdapter::descriptor(value))
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
        self.host.cancel(self.token);
        self.cursor.wake();
    }

    fn close(&self) {
        if !self.closed.swap(true, Ordering::AcqRel) {
            self.host.close(self.token);
        }
    }
}
