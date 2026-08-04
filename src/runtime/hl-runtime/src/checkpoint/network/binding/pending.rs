use std::fmt;
use std::sync::{Arc, RwLock};

use hl_descriptor::{
    DescriptionRef, ObjectError, ObjectKind, OpenFileDescription, Readiness, ReadinessObserver, ReadinessSubscription,
    StatusFlags,
};
use hl_network::NetworkCheckpointError;

pub(super) struct PendingSocket {
    object: RwLock<Option<Arc<dyn OpenFileDescription>>>,
}

impl PendingSocket {
    pub(super) const fn new() -> Self {
        Self {
            object: RwLock::new(None),
        }
    }

    pub(super) fn bind(&self, object: Arc<dyn OpenFileDescription>) -> Result<(), NetworkCheckpointError> {
        if object.kind() != ObjectKind::Socket {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        let mut current = self.object.write().map_err(|_| NetworkCheckpointError::InvalidImage)?;
        if current.is_some() {
            return Err(NetworkCheckpointError::InvalidImage);
        }
        *current = Some(object);
        Ok(())
    }

    fn current(&self) -> Result<Arc<dyn OpenFileDescription>, ObjectError> {
        self.object
            .read()
            .map_err(|_| ObjectError::Io)?
            .clone()
            .ok_or(ObjectError::Busy)
    }
}

impl fmt::Debug for PendingSocket {
    fn fmt(&self, output: &mut fmt::Formatter<'_>) -> fmt::Result {
        output.debug_struct("PendingSocket").finish_non_exhaustive()
    }
}

impl OpenFileDescription for PendingSocket {
    fn transfer_dependencies(&self) -> Vec<DescriptionRef> {
        self.current().map_or_else(|_| Vec::new(), |object| object.transfer_dependencies())
    }

    fn kind(&self) -> ObjectKind {
        ObjectKind::Socket
    }
    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.current()?.read(output)
    }
    fn read_with_cancellation(
        &self,
        output: &mut [u8],
        cancellation: &dyn hl_descriptor::OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.current()?.read_with_cancellation(output, cancellation)
    }
    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.current()?.write(input)
    }
    fn write_with_cancellation(
        &self,
        input: &[u8],
        cancellation: &dyn hl_descriptor::OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.current()?.write_with_cancellation(input, cancellation)
    }
    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        self.current()?.set_status_flags(flags)
    }
    fn readiness(&self, interests: Readiness) -> Readiness {
        self.current()
            .map_or_else(|_| Readiness::default(), |object| object.readiness(interests))
    }
    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.current()?.subscribe_readiness(observer)
    }
    fn retire(&self) {
        if let Ok(object) = self.current() {
            object.retire();
        }
    }
    fn close(&self) {
        if let Ok(object) = self.current() {
            object.close();
        }
    }
}
