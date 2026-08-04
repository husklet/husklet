use std::sync::Arc;

use hl_descriptor::{
    ObjectError, ObjectKind, OpenFileDescription, OperationContext, Readiness, ReadinessObserver,
    ReadinessSubscription, StatusFlags,
};

use super::{EventFd, EventFdError, EventInterest};

impl OpenFileDescription for EventFd {
    fn kind(&self) -> ObjectKind {
        ObjectKind::EventCounter
    }

    fn metadata(&self) -> Result<hl_descriptor::OfdMetadata, ObjectError> {
        Ok(self.status().metadata())
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        EventFd::read(self, output).map_err(EventFdError::object_error)
    }

    fn read_context(&self, output: &mut [u8], context: OperationContext<'_>) -> Result<usize, ObjectError> {
        EventFd::read_context(self, output, context.cancellation).map_err(EventFdError::object_error)
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        EventFd::write(self, input).map_err(EventFdError::object_error)
    }

    fn write_context(&self, input: &[u8], context: OperationContext<'_>) -> Result<usize, ObjectError> {
        EventFd::write_context(self, input, context.cancellation).map_err(EventFdError::object_error)
    }

    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        self.set_nonblocking(flags.bits() & StatusFlags::NONBLOCKING != 0)
            .map_err(EventFdError::object_error)
    }

    fn readiness(&self, interests: Readiness) -> Readiness {
        let interests = EventInterest::from_bits(interests.bits());
        Readiness::from_bits(EventFd::readiness(self, interests).bits())
    }

    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.inner.readiness.subscribe(observer)
    }

    fn retire(&self) {
        self.retire_inner();
    }

    fn close(&self) {
        self.retire_inner();
    }
}
