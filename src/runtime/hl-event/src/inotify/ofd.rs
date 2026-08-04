use std::sync::Arc;

use hl_descriptor::{
    ObjectError, ObjectKind, OpenFileDescription, OperationContext, Readiness, ReadinessObserver,
    ReadinessSubscription, StatusFlags,
};

use super::model::InotifyError;
use crate::inotify::Inotify;

impl OpenFileDescription for Inotify {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Event
    }

    fn metadata(&self) -> Result<hl_descriptor::OfdMetadata, ObjectError> {
        Ok(self.status().metadata())
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        Inotify::read(self, output).map_err(InotifyError::object_error)
    }

    fn prepare_atomic_read(
        &self,
        maximum: usize,
    ) -> Result<Option<Box<dyn hl_descriptor::PreparedAtomicRead>>, ObjectError> {
        crate::inotify::prepared::AtomicRead::prepare(self, maximum)
    }

    fn prepare_atomic_context(
        &self,
        maximum: usize,
        context: OperationContext<'_>,
    ) -> Result<Option<Box<dyn hl_descriptor::PreparedAtomicRead>>, ObjectError> {
        crate::inotify::prepared::AtomicRead::prepare_context(self, maximum, context.cancellation)
    }

    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        self.set_nonblocking(flags.bits() & StatusFlags::NONBLOCKING != 0)
            .map_err(InotifyError::object_error)
    }

    fn readiness(&self, interests: Readiness) -> Readiness {
        Inotify::readiness(self, interests)
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

impl InotifyError {
    pub(crate) const fn object_error(self) -> ObjectError {
        match self {
            Self::InvalidArgument => ObjectError::InvalidArgument,
            Self::WouldBlock => ObjectError::WouldBlock,
            Self::ResourceLimit => ObjectError::ResourceLimit,
            Self::Interrupted => ObjectError::Interrupted,
            Self::Retired => ObjectError::Retired,
            Self::NotSupported => ObjectError::NotSupported,
            Self::AlreadyExists
            | Self::NotFound
            | Self::NotDirectory
            | Self::NameTooLong
            | Self::PermissionDenied
            | Self::SourceFailed => ObjectError::Io,
        }
    }
}
