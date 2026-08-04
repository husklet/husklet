use std::collections::BTreeMap;
use std::fmt;
use std::io::{IoSlice, IoSliceMut};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use hl_descriptor::{
    DescriptorCheckpointError, DescriptorObjectCheckpoint, ObjectError, ObjectKind, OfdMetadata, OpenDescriptionImage,
    OpenFileDescription, OperationCancellation, PipeTransferEndpoint, PreparedSpliceRead, Readiness, ReadinessObserver,
    ReadinessSubscription, StatusFlags,
};
use hl_ipc::{
    IpcCatalog, IpcCheckpointError, IpcPipeId, IpcResourceKey, PipeEndpoint, PipeEndpointBinding, PipeEndpointKind,
};

const OBJECT_VERSION: u8 = 1;
const OBJECT_BYTES: usize = 10;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Staging,
    Committed,
    Resumed,
}

#[derive(Clone, Copy)]
struct Resource {
    key: IpcResourceKey,
    kind: PipeEndpointKind,
}

struct State {
    resources: BTreeMap<u64, Resource>,
    staged_resources: BTreeMap<u64, Resource>,
    previous_resources: Option<BTreeMap<u64, Resource>>,
    pending: BTreeMap<IpcResourceKey, Arc<PendingObject>>,
    phase: Phase,
}

impl State {
    fn remove(&mut self, identity: u64, resource: Resource) {
        let matches = match self.resources.get(&identity) {
            Some(value) => value.key == resource.key,
            None => false,
        };
        if matches {
            self.resources.remove(&identity);
        }
    }
}

/// Joins descriptor object payloads to IPC pipe endpoint resource keys.
pub struct PipeBindings {
    state: Arc<Mutex<State>>,
}

pub(super) struct Registration {
    state: Arc<Mutex<State>>,
    resources: [(u64, Resource); 2],
    published: bool,
}

impl PipeBindings {
    #[must_use]
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(State {
                resources: BTreeMap::new(),
                staged_resources: BTreeMap::new(),
                previous_resources: None,
                pending: BTreeMap::new(),
                phase: Phase::Staging,
            })),
        }
    }

    pub fn register(
        &self,
        identity: u64,
        key: IpcResourceKey,
        kind: PipeEndpointKind,
    ) -> Result<(), IpcCheckpointError> {
        let mut state = self.state.lock().map_err(|_| IpcCheckpointError::InvalidImage)?;
        if identity == 0
            || state.resources.values().any(|resource| resource.key == key)
            || state.resources.insert(identity, Resource { key, kind }).is_some()
        {
            return Err(IpcCheckpointError::InvalidImage);
        }
        Ok(())
    }

    pub(super) fn register_pair(
        &self,
        identities: [u64; 2],
        keys: [IpcResourceKey; 2],
    ) -> Result<Registration, IpcCheckpointError> {
        let resources = [
            (
                identities[0],
                Resource {
                    key: keys[0],
                    kind: PipeEndpointKind::Reader,
                },
            ),
            (
                identities[1],
                Resource {
                    key: keys[1],
                    kind: PipeEndpointKind::Writer,
                },
            ),
        ];
        let mut state = self.state.lock().map_err(|_| IpcCheckpointError::InvalidImage)?;
        if identities.contains(&0)
            || identities[0] == identities[1]
            || state.resources.values().any(|resource| keys.contains(&resource.key))
            || resources
                .iter()
                .any(|(identity, _)| state.resources.contains_key(identity))
        {
            return Err(IpcCheckpointError::InvalidImage);
        }
        for (identity, resource) in resources {
            state.resources.insert(identity, resource);
        }
        Ok(Registration {
            state: self.state.clone(),
            resources,
            published: false,
        })
    }

    pub(super) fn unregister(&self, identity: u64, key: IpcResourceKey) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state
            .resources
            .get(&identity)
            .is_some_and(|resource| resource.key == key)
        {
            state.resources.remove(&identity);
        }
    }

    #[cfg(test)]
    pub(super) fn registered(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .resources
            .len()
    }

    pub(crate) fn descriptor(
        &self,
        key: IpcResourceKey,
        kind: PipeEndpointKind,
    ) -> Result<Arc<dyn PipeEndpointBinding>, IpcCheckpointError> {
        let state = self.state.lock().map_err(|_| IpcCheckpointError::InvalidImage)?;
        if state.phase != Phase::Staging {
            return Err(IpcCheckpointError::InvalidImage);
        }
        let pending = state.pending.get(&key).ok_or(IpcCheckpointError::InvalidImage)?;
        if pending.kind != kind {
            return Err(IpcCheckpointError::InvalidImage);
        }
        Ok(pending.clone())
    }

    pub(crate) fn commit(&self) -> Result<(), IpcCheckpointError> {
        let mut state = self.state.lock().map_err(|_| IpcCheckpointError::InvalidImage)?;
        if state.phase != Phase::Staging || state.pending.values().any(|pending| pending.endpoint.get().is_none()) {
            return Err(IpcCheckpointError::InvalidImage);
        }
        if state.previous_resources.is_some() {
            return Err(IpcCheckpointError::InvalidImage);
        }
        let staged = std::mem::take(&mut state.staged_resources);
        state.previous_resources = Some(std::mem::replace(&mut state.resources, staged));
        for pending in state.pending.values() {
            pending.activate();
        }
        state.phase = Phase::Committed;
        Ok(())
    }

    pub(crate) fn rollback(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        for pending in state.pending.values() {
            pending.deactivate();
        }
        if let Some(previous) = state.previous_resources.take() {
            state.resources = previous;
        }
        state.staged_resources.clear();
        state.pending.clear();
        state.phase = Phase::Staging;
    }

    pub(crate) fn resume(&self) -> Result<(), IpcCheckpointError> {
        let mut state = self.state.lock().map_err(|_| IpcCheckpointError::InvalidImage)?;
        if state.phase != Phase::Committed {
            return Err(IpcCheckpointError::InvalidImage);
        }
        state.phase = Phase::Resumed;
        Ok(())
    }

    pub(crate) fn finish(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.phase != Phase::Resumed {
            return;
        }
        state.previous_resources = None;
        state.pending.clear();
        state.phase = Phase::Staging;
    }

    fn decode(description: &OpenDescriptionImage) -> Result<Resource, DescriptorCheckpointError> {
        if description.object.len() != OBJECT_BYTES || description.object[0] != OBJECT_VERSION {
            return Err(DescriptorCheckpointError::Object);
        }
        let kind = match description.object[1] {
            1 => PipeEndpointKind::Reader,
            2 => PipeEndpointKind::Writer,
            _ => return Err(DescriptorCheckpointError::Object),
        };
        let key = u64::from_le_bytes(description.object[2..10].try_into().unwrap());
        Ok(Resource {
            key: IpcResourceKey::new(key).ok_or(DescriptorCheckpointError::Object)?,
            kind,
        })
    }
}

impl Registration {
    pub(super) fn publish(mut self) {
        self.published = true;
    }
}

impl Drop for Registration {
    fn drop(&mut self) {
        if self.published {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        for (identity, resource) in self.resources {
            state.remove(identity, resource);
        }
    }
}

impl Default for PipeBindings {
    fn default() -> Self {
        Self::new()
    }
}

impl DescriptorObjectCheckpoint for PipeBindings {
    fn snapshot(&self, identity: u64, object: &dyn OpenFileDescription) -> Result<Vec<u8>, DescriptorCheckpointError> {
        let state = self.state.lock().map_err(|_| DescriptorCheckpointError::Object)?;
        let resource = state
            .resources
            .get(&identity)
            .ok_or(DescriptorCheckpointError::Object)?;
        let endpoint = object
            .pipe_transfer_endpoint()
            .and_then(|endpoint| endpoint.as_any().downcast_ref::<PipeEndpoint>())
            .ok_or(DescriptorCheckpointError::Object)?;
        if endpoint.checkpoint_kind() != resource.kind {
            return Err(DescriptorCheckpointError::Object);
        }
        let mut bytes = Vec::with_capacity(OBJECT_BYTES);
        bytes.push(OBJECT_VERSION);
        bytes.push(match resource.kind {
            PipeEndpointKind::Reader => 1,
            PipeEndpointKind::Writer => 2,
        });
        bytes.extend_from_slice(&resource.key.get().to_le_bytes());
        Ok(bytes)
    }

    fn rebind(
        &self,
        description: &OpenDescriptionImage,
    ) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError> {
        if description.kind != ObjectKind::Pipe {
            return Err(DescriptorCheckpointError::Object);
        }
        let resource = Self::decode(description)?;
        let pending = Arc::new(PendingObject {
            identity: description.identity,
            key: resource.key,
            kind: resource.kind,
            endpoint: OnceLock::new(),
            catalog: OnceLock::new(),
            state: self.state.clone(),
            active: AtomicBool::new(false),
        });
        let mut state = self.state.lock().map_err(|_| DescriptorCheckpointError::Object)?;
        if state.phase != Phase::Staging
            || state.staged_resources.insert(description.identity, resource).is_some()
            || state.pending.insert(resource.key, pending.clone()).is_some()
        {
            state.staged_resources.remove(&description.identity);
            return Err(DescriptorCheckpointError::Object);
        }
        Ok(pending)
    }
}

struct PendingObject {
    identity: u64,
    key: IpcResourceKey,
    kind: PipeEndpointKind,
    endpoint: OnceLock<Arc<PipeEndpoint>>,
    catalog: OnceLock<(Weak<IpcCatalog>, IpcPipeId)>,
    state: Arc<Mutex<State>>,
    active: AtomicBool,
}

impl PendingObject {
    fn current(&self) -> Result<&PipeEndpoint, ObjectError> {
        self.endpoint.get().map(Arc::as_ref).ok_or(ObjectError::Busy)
    }

    fn activate(&self) {
        self.active.store(true, Ordering::Release);
    }

    fn deactivate(&self) {
        self.active.store(false, Ordering::Release);
    }
}

impl fmt::Debug for PendingObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingPipe")
            .field("kind", &self.kind)
            .field("bound", &self.endpoint.get().is_some())
            .finish()
    }
}

impl PipeEndpointBinding for PendingObject {
    fn bind(&self, endpoint: Arc<PipeEndpoint>) -> Result<(), IpcCheckpointError> {
        if endpoint.checkpoint_kind() != self.kind || self.endpoint.set(endpoint).is_err() {
            return Err(IpcCheckpointError::InvalidImage);
        }
        Ok(())
    }

    fn attach(&self, catalog: Weak<IpcCatalog>, pipe: IpcPipeId) -> Result<(), IpcCheckpointError> {
        self.catalog
            .set((catalog, pipe))
            .map_err(|_| IpcCheckpointError::InvalidImage)
    }
}

impl OpenFileDescription for PendingObject {
    fn kind(&self) -> ObjectKind {
        ObjectKind::Pipe
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.current()?.read(output)
    }

    fn read_with_cancellation(
        &self,
        output: &mut [u8],
        cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.current()?.read_with_cancellation(output, cancellation)
    }

    fn probe_read(&self, maximum: usize) -> Result<Option<usize>, ObjectError> {
        self.current()?.probe_read(maximum)
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.current()?.write(input)
    }

    fn write_with_cancellation(
        &self,
        input: &[u8],
        cancellation: &dyn OperationCancellation,
    ) -> Result<usize, ObjectError> {
        self.current()?.write_with_cancellation(input, cancellation)
    }

    fn read_vector_context(
        &self,
        output: &mut [IoSliceMut<'_>],
        context: hl_descriptor::OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        self.current()?.read_vector_context(output, context)
    }

    fn write_vector_context(
        &self,
        input: &[IoSlice<'_>],
        context: hl_descriptor::OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        self.current()?.write_vector_context(input, context)
    }

    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        self.current()?.metadata()
    }

    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        self.current()?.set_status_flags(flags)
    }

    fn pipe_capacity(&self) -> Result<usize, ObjectError> {
        self.current()?.pipe_capacity()
    }

    fn atomic_write_limit(&self) -> Option<usize> {
        self.current().ok()?.atomic_write_limit()
    }

    fn set_pipe_capacity(&self, requested: usize) -> Result<usize, ObjectError> {
        self.current()?.set_pipe_capacity(requested)
    }

    fn pipe_transfer_endpoint(&self) -> Option<&dyn PipeTransferEndpoint> {
        self.endpoint
            .get()
            .map(|endpoint| endpoint.as_ref() as &dyn PipeTransferEndpoint)
    }

    fn prepare_splice_read(
        &self,
        offset: Option<u64>,
        maximum: usize,
        nonblocking: bool,
        cancellation: Option<&dyn OperationCancellation>,
    ) -> Result<Option<Box<dyn PreparedSpliceRead>>, ObjectError> {
        self.current()?
            .prepare_splice_read(offset, maximum, nonblocking, cancellation)
    }

    fn readiness(&self, interests: Readiness) -> Readiness {
        self.current()
            .map_or_else(|_| Readiness::default(), |endpoint| endpoint.readiness(interests))
    }

    fn subscribe_readiness(
        &self,
        observer: Arc<dyn ReadinessObserver>,
    ) -> Result<Box<dyn ReadinessSubscription>, ObjectError> {
        self.current()?.subscribe_readiness(observer)
    }

    fn retire(&self) {
        if let Ok(endpoint) = self.current() {
            endpoint.retire();
        }
    }

    fn close(&self) {
        if let Ok(endpoint) = self.current() {
            endpoint.close();
        }
        if !self.active.load(Ordering::Acquire) {
            return;
        }
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.remove(
            self.identity,
            Resource {
                key: self.key,
                kind: self.kind,
            },
        );
        drop(state);
        if let Some((catalog, pipe)) = self.catalog.get() {
            if let Some(catalog) = catalog.upgrade() {
                let _ = catalog.retire_pipe(*pipe);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_descriptor::{DescriptorFlags, DescriptorTable};
    use hl_ipc::Pipe;

    #[test]
    fn aliases_share_endpoint() {
        let bindings = Arc::new(PipeBindings::new());
        let pipe = Pipe::new(false);
        let table = DescriptorTable::new(16).unwrap();
        let reader = table
            .install(0, pipe.reader.clone(), DescriptorFlags::default())
            .unwrap();
        let alias = table.duplicate(reader, 0, DescriptorFlags::default()).unwrap();
        let identity = table.snapshot(reader).unwrap().description_identity;
        let key = IpcResourceKey::new(41).unwrap();
        bindings.register(identity, key, PipeEndpointKind::Reader).unwrap();
        table.freeze_checkpoint();
        let image = table.checkpoint_image(bindings.as_ref()).unwrap();
        table.thaw_checkpoint();

        let restored = DescriptorTable::restore_checkpoint(&image, bindings.as_ref()).unwrap();
        bindings
            .descriptor(key, PipeEndpointKind::Reader)
            .unwrap()
            .bind(pipe.reader.clone())
            .unwrap();
        bindings.commit().unwrap();
        bindings.resume().unwrap();

        assert_eq!(
            restored.snapshot(reader).unwrap().description_identity,
            restored.snapshot(alias).unwrap().description_identity,
        );
        pipe.writer.write(b"ipc").unwrap();
        let mut bytes = [0; 3];
        assert_eq!(restored.pin(reader).unwrap().read(&mut bytes).unwrap(), 3);
        assert_eq!(&bytes, b"ipc");
    }

    #[test]
    fn malformed_payload_rejected() {
        let bindings = PipeBindings::new();
        for object in [Vec::new(), vec![0; OBJECT_BYTES], vec![1; OBJECT_BYTES - 1]] {
            let image = OpenDescriptionImage {
                identity: 1,
                generation: 1,
                offset: 0,
                status: StatusFlags::default(),
                kind: ObjectKind::Pipe,
                object,
            };
            assert!(bindings.rebind(&image).is_err());
        }
    }

    #[test]
    fn rollback_restarts_staging() {
        let bindings = PipeBindings::new();
        let image = OpenDescriptionImage {
            identity: 1,
            generation: 1,
            offset: 0,
            status: StatusFlags::default(),
            kind: ObjectKind::Pipe,
            object: [vec![OBJECT_VERSION, 1], 9_u64.to_le_bytes().to_vec()].concat(),
        };
        assert!(bindings.rebind(&image).is_ok());
        bindings.rollback();
        assert!(bindings.rebind(&image).is_ok());
    }
}
