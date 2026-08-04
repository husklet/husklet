use std::fmt;
use std::io::{IoSlice, IoSliceMut};
use std::sync::{Arc, OnceLock, Weak};

use hl_descriptor::{
    ObjectError, ObjectKind, OfdMetadata, OpenFileDescription, OperationCancellation, PipeTransferEndpoint,
    PreparedSpliceRead, Readiness, ReadinessObserver, ReadinessSubscription, StatusFlags,
};
use hl_ipc::{
    IpcCatalog as HostCatalog, IpcCheckpointError, IpcPipeId, IpcResourceKey, Pipe, PipeEndpoint, PipeEndpointBinding,
};

use super::{IpcCatalog, PipeBindings, binding::Registration};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RegistryError {
    Capacity,
    Invalid,
}

/// Production bridge shared by pipe creation and checkpoint participants.
pub struct PipeRegistry {
    catalog: Arc<IpcCatalog>,
    bindings: Arc<PipeBindings>,
}

pub struct OpenPipe {
    pipe: Arc<Pipe>,
    reader: Arc<TrackedEndpoint>,
    writer: Arc<TrackedEndpoint>,
    registry: Arc<PipeRegistry>,
}

pub struct Publication {
    catalog: Option<hl_ipc::PreparedPipe>,
    registration: Option<Registration>,
}

impl PipeRegistry {
    #[must_use]
    pub const fn new(catalog: Arc<IpcCatalog>, bindings: Arc<PipeBindings>) -> Self {
        Self { catalog, bindings }
    }

    #[must_use]
    pub fn bindings(&self) -> Arc<PipeBindings> {
        self.bindings.clone()
    }

    #[must_use]
    pub fn open(self: &Arc<Self>, pipe: Arc<Pipe>) -> OpenPipe {
        OpenPipe {
            reader: Arc::new(TrackedEndpoint::new(pipe.reader.clone())),
            writer: Arc::new(TrackedEndpoint::new(pipe.writer.clone())),
            pipe,
            registry: self.clone(),
        }
    }
}

impl OpenPipe {
    #[must_use]
    pub fn descriptions(&self) -> [Arc<dyn OpenFileDescription>; 2] {
        [self.reader.clone(), self.writer.clone()]
    }

    pub fn prepare(&self, identities: [u64; 2]) -> Result<Publication, RegistryError> {
        let catalog = self.registry.catalog.current();
        let keys = catalog.resource_pair().map_err(|_| RegistryError::Capacity)?;
        let registration = self
            .registry
            .bindings
            .register_pair(identities, keys)
            .map_err(|_| RegistryError::Invalid)?;
        let prepared = catalog
            .prepare_pipe(
                self.pipe.clone(),
                keys[0],
                keys[1],
                Arc::new(LiveBinding),
                Arc::new(LiveBinding),
            )
            .map_err(|_| RegistryError::Capacity)?;
        let id = prepared.id();
        self.reader.track(Lifetime::new(
            &catalog,
            id,
            identities[0],
            keys[0],
            self.registry.bindings.clone(),
        ))?;
        self.writer.track(Lifetime::new(
            &catalog,
            id,
            identities[1],
            keys[1],
            self.registry.bindings.clone(),
        ))?;
        Ok(Publication {
            catalog: Some(prepared),
            registration: Some(registration),
        })
    }
}

impl Publication {
    pub fn publish(mut self) {
        let catalog = self.catalog.take().expect("unpublished pipe registration");
        let registration = self.registration.take().expect("unpublished pipe bindings");
        let _ = catalog.publish();
        registration.publish();
    }
}

struct LiveBinding;

impl PipeEndpointBinding for LiveBinding {
    fn bind(&self, _: Arc<PipeEndpoint>) -> Result<(), IpcCheckpointError> {
        Ok(())
    }
}

struct Lifetime {
    catalog: Weak<HostCatalog>,
    id: IpcPipeId,
    identity: u64,
    key: IpcResourceKey,
    bindings: Arc<PipeBindings>,
}

impl Lifetime {
    fn new(
        catalog: &Arc<HostCatalog>,
        id: IpcPipeId,
        identity: u64,
        key: IpcResourceKey,
        bindings: Arc<PipeBindings>,
    ) -> Arc<Self> {
        Arc::new(Self {
            catalog: Arc::downgrade(catalog),
            id,
            identity,
            key,
            bindings,
        })
    }

    fn close(&self) {
        self.bindings.unregister(self.identity, self.key);
        if let Some(catalog) = self.catalog.upgrade() {
            let _ = catalog.retire_pipe(self.id);
        }
    }
}

struct TrackedEndpoint {
    endpoint: Arc<PipeEndpoint>,
    lifetime: OnceLock<Arc<Lifetime>>,
}

impl TrackedEndpoint {
    fn new(endpoint: Arc<PipeEndpoint>) -> Self {
        Self {
            endpoint,
            lifetime: OnceLock::new(),
        }
    }

    fn track(&self, lifetime: Arc<Lifetime>) -> Result<(), RegistryError> {
        self.lifetime.set(lifetime).map_err(|_| RegistryError::Invalid)
    }
}

impl fmt::Debug for TrackedEndpoint {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("TrackedPipe")
            .field("endpoint", &self.endpoint)
            .field("registered", &self.lifetime.get().is_some())
            .finish()
    }
}

impl OpenFileDescription for TrackedEndpoint {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Pipe
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.endpoint.read(output)
    }

    fn read_with_cancellation(
        &self,
        output: &mut [u8],
        cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.endpoint.read_with_cancellation(output, cancellation)
    }

    fn probe_read(&self, maximum: usize) -> Result<Option<usize>, ObjectError> {
        self.endpoint.probe_read(maximum)
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.endpoint.write(input)
    }

    fn write_with_cancellation(
        &self,
        input: &[u8],
        cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.endpoint.write_with_cancellation(input, cancellation)
    }

    fn read_vector_context(
        &self,
        output: &mut [IoSliceMut<'_>],
        context: hl_descriptor::OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        self.endpoint.read_vector_context(output, context)
    }

    fn write_vector_context(
        &self,
        input: &[IoSlice<'_>],
        context: hl_descriptor::OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        self.endpoint.write_vector_context(input, context)
    }

    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        self.endpoint.metadata()
    }

    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        self.endpoint.set_status_flags(flags)
    }

    fn pipe_capacity(&self) -> Result<usize, ObjectError> {
        self.endpoint.pipe_capacity()
    }

    fn atomic_write_limit(&self) -> Option<usize> {
        self.endpoint.atomic_write_limit()
    }

    fn set_pipe_capacity(&self, requested: usize) -> Result<usize, ObjectError> {
        self.endpoint.set_pipe_capacity(requested)
    }

    fn pipe_transfer_endpoint(&self) -> Option<&dyn PipeTransferEndpoint> {
        Some(self.endpoint.as_ref())
    }

    fn prepare_splice_read(
        &self,
        offset: Option<u64>,
        maximum: usize,
        nonblocking: bool,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<Option<Box<dyn PreparedSpliceRead>>, ObjectError> {
        self.endpoint
            .prepare_splice_read(offset, maximum, nonblocking, cancellation)
    }

    fn readiness(&self, interests: Readiness) -> Readiness {
        self.endpoint.readiness(interests)
    }

    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.endpoint.subscribe_readiness(observer)
    }

    fn retire(&self) {
        self.endpoint.retire();
    }

    fn close(&self) {
        self.endpoint.close();
        if let Some(lifetime) = self.lifetime.get() {
            lifetime.close();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use hl_descriptor::{DescriptorFlags, DescriptorTable, StatusFlags};
    use hl_ipc::{
        MessageLimits, MessageQueueNamespace, SemaphoreLimits, SemaphoreNamespace, SharedMemoryLimits,
        SharedMemoryNamespace,
    };
    use hl_memory::{SharedLimits, SharedObjectStore};

    use super::*;

    fn registry() -> (Arc<IpcCatalog>, Arc<PipeRegistry>, Arc<PipeBindings>) {
        let memory = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
        let shared_limits = SharedMemoryLimits::default();
        let message_limits = MessageLimits::default();
        let semaphore_limits = SemaphoreLimits::default();
        let catalog = Arc::new(HostCatalog::new(
            Arc::new(SharedMemoryNamespace::new(memory, shared_limits).unwrap()),
            shared_limits,
            Vec::new(),
            Arc::new(MessageQueueNamespace::new(message_limits).unwrap()),
            message_limits,
            Arc::new(SemaphoreNamespace::new(semaphore_limits).unwrap()),
            semaphore_limits,
            Vec::new(),
        ));
        let checkpoint = Arc::new(IpcCatalog::new(catalog));
        let bindings = Arc::new(PipeBindings::new());
        let registry = Arc::new(PipeRegistry::new(checkpoint.clone(), bindings.clone()));
        (checkpoint, registry, bindings)
    }

    fn prepare<'table>(
        registry: &Arc<PipeRegistry>,
        table: &'table DescriptorTable,
        flags: [DescriptorFlags; 2],
    ) -> (hl_descriptor::PreparedInstallBatch<'table>, Publication) {
        let opened = registry.open(Arc::new(Pipe::new(true)));
        let descriptions = opened.descriptions();
        let batch = table
            .prepare_open_batch(
                0,
                vec![
                    (descriptions[0].clone(), StatusFlags::default(), flags[0]),
                    (descriptions[1].clone(), StatusFlags::from_bits(1), flags[1]),
                ],
            )
            .unwrap();
        let identities = batch.description_identities();
        let publication = opened
            .prepare([identities[0].identity, identities[1].identity])
            .unwrap();
        (batch, publication)
    }

    fn image(catalog: &IpcCatalog) -> hl_ipc::IpcCheckpointImage {
        let current = catalog.current();
        current.freeze_checkpoint();
        let image = current.checkpoint_image().unwrap();
        current.thaw_checkpoint();
        image
    }

    #[test]
    fn alias_lifetime() {
        let (catalog, registry, bindings) = registry();
        let table = DescriptorTable::new(8).unwrap();
        let (batch, publication) = prepare(&registry, &table, [DescriptorFlags::default(); 2]);
        publication.publish();
        let numbers = batch.publish_all();
        let alias = table.duplicate(numbers[0], 0, DescriptorFlags::default()).unwrap();
        assert_eq!(image(&catalog).pipes.len(), 1);
        assert_eq!(bindings.registered(), 2);
        table.close(numbers[0]).unwrap();
        table.close(alias).unwrap();
        assert_eq!(image(&catalog).pipes[0].snapshot.readers, 0);
        assert_eq!(bindings.registered(), 1);
        table.close(numbers[1]).unwrap();
        assert!(image(&catalog).pipes.is_empty());
        assert_eq!(bindings.registered(), 0);
    }

    #[test]
    fn fork_lifetime() {
        let (catalog, registry, _) = registry();
        let parent = DescriptorTable::new(8).unwrap();
        let (batch, publication) = prepare(&registry, &parent, [DescriptorFlags::default(); 2]);
        publication.publish();
        let numbers = batch.publish_all();
        let child = parent.fork();
        parent.close(numbers[0]).unwrap();
        parent.close(numbers[1]).unwrap();
        assert_eq!(image(&catalog).pipes.len(), 1);
        child.close(numbers[0]).unwrap();
        child.close(numbers[1]).unwrap();
        assert!(image(&catalog).pipes.is_empty());
    }

    #[test]
    fn cloexec_half_closed() {
        let (catalog, registry, _) = registry();
        let table = DescriptorTable::new(8).unwrap();
        let cloexec = DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC);
        let (batch, publication) = prepare(&registry, &table, [cloexec, DescriptorFlags::default()]);
        publication.publish();
        let numbers = batch.publish_all();
        assert_eq!(table.close_on_exec(), [numbers[0]]);
        let checkpoint = image(&catalog);
        assert_eq!(checkpoint.pipes[0].snapshot.readers, 0);
        assert_eq!(checkpoint.pipes[0].snapshot.writers, 1);
        table.close(numbers[1]).unwrap();
        assert!(image(&catalog).pipes.is_empty());
    }

    #[test]
    fn rollback_empty() {
        let (catalog, registry, bindings) = registry();
        let table = DescriptorTable::new(8).unwrap();
        let (batch, publication) = prepare(&registry, &table, [DescriptorFlags::default(); 2]);
        drop(publication);
        drop(batch);
        assert!(image(&catalog).pipes.is_empty());
        assert_eq!(bindings.registered(), 0);
        assert_eq!(table.reserve(0).unwrap().number(), 0);
    }

    #[test]
    fn stable_keys() {
        let (catalog, registry, _) = registry();
        let table = DescriptorTable::new(8).unwrap();
        let (first_batch, first_publication) = prepare(&registry, &table, [DescriptorFlags::default(); 2]);
        first_publication.publish();
        let first_numbers = first_batch.publish_all();
        let first = image(&catalog).pipes[0].clone();
        table.close(first_numbers[0]).unwrap();
        table.close(first_numbers[1]).unwrap();

        let (second_batch, second_publication) = prepare(&registry, &table, [DescriptorFlags::default(); 2]);
        second_publication.publish();
        let _ = second_batch.publish_all();
        let second = image(&catalog).pipes[0].clone();
        assert_eq!([first.reader.get(), first.writer.get()], [1, 2]);
        assert_eq!([second.reader.get(), second.writer.get()], [3, 4]);
        assert!(second.id.generation > first.id.generation);
    }

    #[test]
    fn freeze_preparation() {
        let (catalog, registry, _) = registry();
        let table = DescriptorTable::new(8).unwrap();
        let (batch, publication) = prepare(&registry, &table, [DescriptorFlags::default(); 2]);
        let current = catalog.current();
        let frozen_catalog = current.clone();
        let (send, receive) = mpsc::channel();
        let freezer = thread::spawn(move || {
            frozen_catalog.freeze_checkpoint();
            send.send(()).unwrap();
        });
        assert!(receive.recv_timeout(Duration::from_millis(20)).is_err());
        publication.publish();
        let _ = batch.publish_all();
        receive.recv_timeout(Duration::from_secs(1)).unwrap();
        current.thaw_checkpoint();
        freezer.join().unwrap();
    }
}
