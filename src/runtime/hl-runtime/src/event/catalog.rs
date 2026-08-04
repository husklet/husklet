use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::{
    DescriptionIdentity, DescriptionRef, ObjectError, ObjectKind, OfdMetadata, OpenFileDescription, OperationContext,
    PreparedAtomicRead, Readiness, ReadinessObserver, ReadinessSubscription, StatusFlags,
};
use hl_event::{EventCatalog, EventObjectId};

use crate::{Control, EventObjectBindings, OperationRegistry};

pub(crate) struct CatalogBoundEvent {
    object: Arc<dyn OpenFileDescription>,
    catalog: Arc<EventCatalog>,
    id: Mutex<Option<EventObjectId>>,
    closed: AtomicBool,
    epoll: Mutex<Option<(Arc<Control>, DescriptionIdentity)>>,
    operations: Mutex<Option<(Arc<OperationRegistry>, DescriptionIdentity)>>,
    checkpoint: Mutex<Option<(Arc<EventObjectBindings>, u64, EventObjectId)>>,
}

impl std::fmt::Debug for CatalogBoundEvent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("CatalogBoundEvent").finish_non_exhaustive()
    }
}

impl CatalogBoundEvent {
    pub(crate) fn new(object: Arc<dyn OpenFileDescription>, catalog: Arc<EventCatalog>) -> Self {
        Self {
            object,
            catalog,
            id: Mutex::new(None),
            closed: AtomicBool::new(false),
            epoll: Mutex::new(None),
            operations: Mutex::new(None),
            checkpoint: Mutex::new(None),
        }
    }
    pub(crate) fn bind(&self, id: EventObjectId) {
        *self.id.lock().unwrap_or_else(|error| error.into_inner()) = Some(id);
    }
    pub(crate) fn bind_epoll(&self, control: Arc<Control>, identity: DescriptionIdentity) {
        *self.epoll.lock().unwrap_or_else(|error| error.into_inner()) = Some((control, identity));
    }
    pub(crate) fn bind_operations(&self, registry: Arc<OperationRegistry>, identity: DescriptionIdentity) {
        *self.operations.lock().unwrap_or_else(|error| error.into_inner()) = Some((registry, identity));
    }

    pub(crate) fn bind_checkpoint(
        &self,
        bindings: Arc<EventObjectBindings>,
        identity: DescriptionIdentity,
        id: EventObjectId,
    ) -> Result<(), hl_event::EventCheckpointError> {
        bindings.register_object(identity.identity, id)?;
        *self.checkpoint.lock().unwrap_or_else(|error| error.into_inner()) = Some((bindings, identity.identity, id));
        Ok(())
    }
}

impl OpenFileDescription for CatalogBoundEvent {
    fn transfer_dependencies(&self) -> Vec<DescriptionRef> {
        self.object.transfer_dependencies()
    }
    fn kind(&self) -> ObjectKind {
        self.object.kind()
    }
    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        let mut metadata = self.object.metadata()?;
        if let Some(id) = *self.id.lock().unwrap_or_else(|error| error.into_inner()) {
            metadata.inode = u64::from(id.slot) | (u64::from(id.generation) << 32);
        }
        Ok(metadata)
    }
    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.object.read(output)
    }
    fn read_context(&self, output: &mut [u8], context: OperationContext<'_>) -> Result<usize, ObjectError> {
        self.object.read_context(output, context)
    }
    fn prepare_atomic_read(&self, maximum: usize) -> Result<Option<Box<dyn PreparedAtomicRead>>, ObjectError> {
        self.object.prepare_atomic_read(maximum)
    }
    fn prepare_atomic_context(
        &self,
        maximum: usize,
        context: OperationContext<'_>,
    ) -> Result<Option<Box<dyn PreparedAtomicRead>>, ObjectError> {
        self.object.prepare_atomic_context(maximum, context)
    }
    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.object.write(input)
    }
    fn write_context(&self, input: &[u8], context: OperationContext<'_>) -> Result<usize, ObjectError> {
        self.object.write_context(input, context)
    }
    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        self.object.set_status_flags(flags)
    }
    fn readiness(&self, interests: Readiness) -> Readiness {
        self.object.readiness(interests)
    }
    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.object.subscribe_readiness(observer)
    }
    fn retire(&self) {
        self.object.retire();
    }
    fn close(&self) {
        if self.closed.swap(true, Ordering::AcqRel) {
            return;
        }
        self.object.close();
        if let Some(id) = self.id.lock().unwrap_or_else(|error| error.into_inner()).take() {
            let _ = self.catalog.remove(id);
        }
        if let Some((control, identity)) = self.epoll.lock().unwrap_or_else(|error| error.into_inner()).take() {
            control.retire_identity(identity);
        }
        if let Some((registry, identity)) = self.operations.lock().unwrap_or_else(|error| error.into_inner()).take() {
            registry.retire(identity);
        }
        if let Some((bindings, identity, id)) = self.checkpoint.lock().unwrap_or_else(|error| error.into_inner()).take()
        {
            bindings.unregister_object(identity, id);
        }
    }
}
