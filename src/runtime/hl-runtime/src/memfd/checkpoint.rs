use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::{
    DescriptionIdentity, DescriptorCheckpointError, DescriptorObjectCheckpoint, ObjectError, ObjectKind,
    OpenDescriptionImage, OpenFileDescription, PreparedSpliceRead, SeekPosition,
};
use hl_memory::{SharedObjectId, SharedObjectStore, SharedSeal};

use crate::{MemoryResourceRestore, MemoryResourceTransaction};

use super::{Position, Registry, RegistryState, RuntimeMemfd};

const VERSION: u8 = 1;
const PAYLOAD_BYTES: usize = 26;

#[derive(Clone, Copy)]
struct Image {
    object: SharedObjectId,
    size: u64,
    cursor: u64,
    allow_sealing: bool,
}

struct Proxy {
    state: Mutex<ProxyState>,
    #[cfg(test)]
    fail_bind: AtomicBool,
    #[cfg(test)]
    close_on_bind: AtomicBool,
}

struct ProxyState {
    object: Option<Arc<RuntimeMemfd>>,
    closed: bool,
    published: bool,
}

impl Proxy {
    fn new() -> Self {
        Self {
            state: Mutex::new(ProxyState {
                object: None,
                closed: false,
                published: false,
            }),
            #[cfg(test)]
            fail_bind: AtomicBool::new(false),
            #[cfg(test)]
            close_on_bind: AtomicBool::new(false),
        }
    }

    fn bind(&self, object: Arc<RuntimeMemfd>) -> Result<(), ()> {
        #[cfg(test)]
        if self.fail_bind.swap(false, Ordering::AcqRel) {
            return Err(());
        }
        let mut state = self.state.lock().map_err(|_| ())?;
        #[cfg(test)]
        if self.close_on_bind.swap(false, Ordering::AcqRel) {
            state.closed = true;
            return Err(());
        }
        if state.closed || state.object.is_some() {
            return Err(());
        }
        state.object = Some(object);
        Ok(())
    }

    fn unbind(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.published = false;
            state.object.take();
        }
    }

    fn publish(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.published = true;
            if state.closed {
                if let Some(object) = state.object.take() {
                    object.close();
                }
            }
        }
    }

    #[cfg(test)]
    fn fail_next_bind(&self) {
        self.fail_bind.store(true, Ordering::Release);
    }

    #[cfg(test)]
    fn close_during_bind(&self) {
        self.close_on_bind.store(true, Ordering::Release);
    }

    fn object(&self) -> Result<Arc<RuntimeMemfd>, ObjectError> {
        let state = self.state.lock().map_err(|_| ObjectError::Io)?;
        if state.closed {
            return Err(ObjectError::Retired);
        }
        state.object.clone().ok_or(ObjectError::Retired)
    }
}

impl std::fmt::Debug for Proxy {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("PendingMemfd").finish_non_exhaustive()
    }
}

impl OpenFileDescription for Proxy {
    fn kind(&self) -> ObjectKind {
        ObjectKind::File
    }
    fn metadata(&self) -> Result<hl_descriptor::OfdMetadata, ObjectError> {
        self.object()?.metadata()
    }
    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.object()?.read(output)
    }
    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.object()?.write(input)
    }
    fn read_at(&self, offset: u64, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.object()?.read_at(offset, output)
    }
    fn write_at(&self, offset: u64, input: &[u8]) -> Result<usize, ObjectError> {
        self.object()?.write_at(offset, input)
    }
    fn seek(&self, position: SeekPosition) -> Result<u64, ObjectError> {
        self.object()?.seek(position)
    }
    fn prepare_splice_read(
        &self,
        offset: Option<u64>,
        maximum: usize,
        nonblocking: bool,
        cancellation: Option<&dyn hl_descriptor::OperationCancellation>,
    ) -> Result<Option<Box<dyn PreparedSpliceRead>>, ObjectError> {
        self.object()?
            .prepare_splice_read(offset, maximum, nonblocking, cancellation)
    }
    fn close(&self) {
        if let Ok(mut state) = self.state.lock() {
            if state.closed {
                return;
            }
            state.closed = true;
            if state.published {
                if let Some(object) = state.object.take() {
                    object.close();
                }
            }
        }
    }
}

struct State {
    pending: BTreeMap<DescriptionIdentity, (Image, Arc<Proxy>)>,
    staged: bool,
}

pub struct Bindings {
    registry: Arc<Registry>,
    state: Arc<Mutex<State>>,
}

impl Bindings {
    #[must_use]
    pub fn new(registry: Arc<Registry>) -> Self {
        Self {
            registry,
            state: Arc::new(Mutex::new(State {
                pending: BTreeMap::new(),
                staged: false,
            })),
        }
    }

    fn encode(object: &RuntimeMemfd) -> Result<Vec<u8>, DescriptorCheckpointError> {
        let position = object.position.lock().map_err(|_| DescriptorCheckpointError::Object)?;
        if position.splice_reserved {
            return Err(DescriptorCheckpointError::Object);
        }
        let mut bytes = Vec::with_capacity(PAYLOAD_BYTES);
        bytes.push(VERSION);
        bytes.extend_from_slice(&object.id.slot.to_le_bytes());
        bytes.extend_from_slice(&object.id.generation.to_le_bytes());
        bytes.extend_from_slice(&object.size.load(Ordering::Acquire).to_le_bytes());
        bytes.extend_from_slice(&position.offset.to_le_bytes());
        bytes.push(u8::from(object.allow_sealing));
        Ok(bytes)
    }

    fn decode(description: &OpenDescriptionImage) -> Result<Image, DescriptorCheckpointError> {
        let bytes = &description.object;
        if description.kind != ObjectKind::File || bytes.len() != PAYLOAD_BYTES || bytes[0] != VERSION {
            return Err(DescriptorCheckpointError::Object);
        }
        let object = SharedObjectId {
            slot: u32::from_le_bytes(bytes[1..5].try_into().unwrap()),
            generation: u32::from_le_bytes(bytes[5..9].try_into().unwrap()),
        };
        let size = u64::from_le_bytes(bytes[9..17].try_into().unwrap());
        let cursor = u64::from_le_bytes(bytes[17..25].try_into().unwrap());
        let allow_sealing = match bytes[25] {
            0 => false,
            1 => true,
            _ => return Err(DescriptorCheckpointError::Object),
        };
        if object.generation == 0 {
            return Err(DescriptorCheckpointError::Object);
        }
        Ok(Image {
            object,
            size,
            cursor,
            allow_sealing,
        })
    }
}

impl DescriptorObjectCheckpoint for Bindings {
    fn snapshot(&self, identity: u64, object: &dyn OpenFileDescription) -> Result<Vec<u8>, DescriptorCheckpointError> {
        if object.kind() != ObjectKind::File {
            return Err(DescriptorCheckpointError::Object);
        }
        let state = self
            .registry
            .state
            .lock()
            .map_err(|_| DescriptorCheckpointError::Object)?;
        let identity = state
            .objects
            .keys()
            .find(|key| key.identity == identity)
            .ok_or(DescriptorCheckpointError::Object)?;
        Self::encode(&state.objects[identity])
    }

    fn rebind(
        &self,
        description: &OpenDescriptionImage,
    ) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError> {
        let image = Self::decode(description)?;
        let identity = DescriptionIdentity {
            identity: description.identity,
            generation: description.generation,
        };
        let pending = Arc::new(Proxy::new());
        let mut state = self.state.lock().map_err(|_| DescriptorCheckpointError::Object)?;
        if state.pending.insert(identity, (image, pending.clone())).is_some() {
            return Err(DescriptorCheckpointError::Object);
        }
        Ok(pending)
    }
}

impl crate::FileObjectCheckpoint for Bindings {
    fn snapshot_size(&self, _: u64, _: &dyn OpenFileDescription) -> Result<usize, DescriptorCheckpointError> {
        Ok(PAYLOAD_BYTES)
    }

    fn owns(&self, identity: u64, object: &dyn OpenFileDescription) -> Result<bool, DescriptorCheckpointError> {
        let Some(object) = object
            .domain_extension()
            .and_then(|extension| extension.downcast_ref::<RuntimeMemfd>())
        else {
            return Ok(false);
        };
        let state = self
            .registry
            .state
            .lock()
            .map_err(|_| DescriptorCheckpointError::Object)?;
        let binding = object.binding.lock().map_err(|_| DescriptorCheckpointError::Object)?;
        let Some((registry, bound, epoch)) = binding.as_ref() else {
            return Ok(false);
        };
        let Some(registry) = registry.upgrade() else {
            return Ok(false);
        };
        Ok(Arc::ptr_eq(&registry, &self.registry)
            && *epoch == state.epoch
            && bound.identity == identity
            && state
                .objects
                .get(bound)
                .is_some_and(|candidate| std::ptr::eq(candidate.as_ref(), object)))
    }
}

impl MemoryResourceRestore for Bindings {
    fn stage(&self, shared: Arc<SharedObjectStore>) -> Result<Box<dyn MemoryResourceTransaction>, ()> {
        let mut bindings = self.state.lock().map_err(|_| ())?;
        if bindings.staged {
            return Err(());
        }
        let current = self.registry.state.lock().map_err(|_| ())?;
        let epoch = current.epoch.checked_add(1).ok_or(())?;
        let owner = current.store.as_ref().map(|(_, owner)| *owner).ok_or(())?;
        drop(current);
        let mut replacement = BTreeMap::new();
        let snapshots = shared
            .snapshot()
            .objects
            .into_iter()
            .map(|object| (object.id, object))
            .collect::<BTreeMap<_, _>>();
        let mut proxies = Vec::with_capacity(bindings.pending.len());
        for (identity, (image, proxy)) in &bindings.pending {
            let snapshot = snapshots.get(&image.object).ok_or(())?;
            if image.size > snapshot.bytes.len() as u64
                || snapshot.seals.bits()
                    & !(SharedSeal::SEAL
                        | SharedSeal::SHRINK
                        | SharedSeal::GROW
                        | SharedSeal::WRITE
                        | SharedSeal::FUTURE_WRITE)
                    != 0
                || !image.allow_sealing && !snapshot.seals.contains(SharedSeal::SEAL)
            {
                return Err(());
            }
            let object = Arc::new(RuntimeMemfd {
                store: Arc::clone(&shared),
                id: image.object,
                allow_sealing: image.allow_sealing,
                size: AtomicU64::new(image.size),
                position: Arc::new(Mutex::new(Position {
                    offset: image.cursor,
                    splice_reserved: false,
                })),
                binding: Mutex::new(None),
                owns_backing: AtomicBool::new(false),
                released: AtomicBool::new(false),
            });
            object.bind(Arc::downgrade(&self.registry), *identity, epoch);
            if replacement.insert(*identity, object.clone()).is_some() {
                return Err(());
            }
            proxies.push((Arc::clone(proxy), object));
        }
        let mut bound: Vec<Arc<Proxy>> = Vec::with_capacity(proxies.len());
        for (proxy, object) in &proxies {
            if proxy.bind(Arc::clone(object)).is_err() {
                for proxy in bound {
                    proxy.unbind();
                }
                return Err(());
            }
            bound.push(Arc::clone(proxy));
        }
        bindings.staged = true;
        drop(bindings);
        Ok(Box::new(Transaction {
            registry: Arc::clone(&self.registry),
            bindings: Arc::clone(&self.state),
            epoch,
            replacement: Some(replacement),
            previous: None,
            committed: false,
            store: Some((shared, owner)),
            proxies: proxies.into_iter().map(|(proxy, _)| proxy).collect(),
            pending: None,
        }))
    }
}

struct Transaction {
    registry: Arc<Registry>,
    bindings: Arc<Mutex<State>>,
    epoch: u64,
    replacement: Option<BTreeMap<DescriptionIdentity, Arc<RuntimeMemfd>>>,
    previous: Option<RegistryState>,
    committed: bool,
    store: Option<(Arc<SharedObjectStore>, u64)>,
    proxies: Vec<Arc<Proxy>>,
    pending: Option<BTreeMap<DescriptionIdentity, (Image, Arc<Proxy>)>>,
}

impl MemoryResourceTransaction for Transaction {
    fn commit(&mut self) -> Result<(), ()> {
        if self.committed {
            return Err(());
        }
        let replacement = self.replacement.take().ok_or(())?;
        let store = self.store.take().ok_or(())?;
        let mut bindings = self.bindings.lock().map_err(|_| ())?;
        let mut state = self.registry.state.lock().map_err(|_| ())?;
        if state.epoch.checked_add(1) != Some(self.epoch) {
            self.replacement = Some(replacement);
            self.store = Some(store);
            return Err(());
        }
        for object in replacement.values() {
            object.activate_backing();
        }
        let previous = std::mem::replace(
            &mut *state,
            RegistryState {
                epoch: self.epoch,
                store: Some(store),
                objects: replacement,
            },
        );
        self.previous = Some(previous);
        self.pending = Some(std::mem::take(&mut bindings.pending));
        drop(state);
        for proxy in &self.proxies {
            proxy.publish();
        }
        self.committed = true;
        Ok(())
    }

    fn rollback(&mut self) {
        if !self.committed {
            self.replacement.take();
            for proxy in &self.proxies {
                proxy.unbind();
            }
            if let Ok(mut bindings) = self.bindings.lock() {
                bindings.staged = false;
            }
            return;
        }
        if let Some(previous) = self.previous.take() {
            let mut state = self
                .registry
                .state
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            if state.epoch == self.epoch {
                let replacement = std::mem::replace(&mut *state, previous);
                for object in replacement.objects.values() {
                    object.deactivate_backing();
                }
                drop(state);
                drop(replacement);
            }
        }
        for proxy in &self.proxies {
            proxy.unbind();
        }
        if let Ok(mut bindings) = self.bindings.lock() {
            if let Some(pending) = self.pending.take() {
                bindings.pending = pending;
            }
            bindings.staged = false;
        }
        self.committed = false;
    }

    fn resume(&mut self) -> Result<(), ()> {
        if !self.committed {
            return Err(());
        }
        Ok(())
    }

    fn finish(&mut self) {
        self.previous.take();
        self.pending.take();
        if let Ok(mut bindings) = self.bindings.lock() {
            bindings.staged = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use hl_descriptor::{DescriptorFlags, DescriptorTable};
    use hl_memory::SharedLimits;

    #[derive(Debug)]
    struct CollidingFile;

    impl OpenFileDescription for CollidingFile {
        fn kind(&self) -> ObjectKind {
            ObjectKind::File
        }
    }

    fn fixture() -> (
        Arc<SharedObjectStore>,
        Arc<Registry>,
        Arc<Bindings>,
        DescriptorTable,
        i32,
        Arc<RuntimeMemfd>,
    ) {
        let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
        let registry = Arc::new(Registry::new());
        registry.configure(store.clone(), 7).unwrap();
        let object = registry.create(true).unwrap();
        object.write(b"abcdef").unwrap();
        object.seek(SeekPosition::Start(2)).unwrap();
        let table = DescriptorTable::new(8).unwrap();
        let number = table.install(0, object.clone(), DescriptorFlags::default()).unwrap();
        let identity = table.pin(number).unwrap().description_identity();
        registry.register(identity, object.clone()).unwrap();
        let bindings = Arc::new(Bindings::new(registry.clone()));
        (store, registry, bindings, table, number, object)
    }

    #[test]
    fn queued_cursor_restore() {
        let (store, _, bindings, table, number, _) = fixture();
        let queued = table.export_description(number).unwrap();
        let identity = queued.identity();
        table.close(number).unwrap();
        table.freeze_checkpoint();
        let image = table.checkpoint_image(bindings.as_ref()).unwrap();
        table.thaw_checkpoint();
        assert!(image.entries.is_empty());
        assert_eq!(image.descriptions.len(), 1);

        let replacement = Arc::new(SharedObjectStore::restore(store.limits(), store.snapshot()).unwrap());
        let restored = DescriptorTable::restore_checkpoint(&image, bindings.as_ref()).unwrap();
        let mut transaction = bindings.stage(replacement).unwrap();
        transaction.commit().unwrap();
        restored.freeze_checkpoint();
        let reference = restored.export_checkpoint_identity(identity).unwrap();
        restored.release_checkpoint_roots();
        restored.thaw_checkpoint();
        let receiver = DescriptorTable::new(4).unwrap();
        let received = receiver
            .install_description(0, &reference, DescriptorFlags::default())
            .unwrap();
        let mut output = [0_u8; 4];
        assert_eq!(receiver.pin(received).unwrap().read(&mut output), Ok(4));
        assert_eq!(&output, b"cdef");
        transaction.resume().unwrap();
    }

    #[test]
    fn binding_exact() {
        let (_, _, bindings, table, number, object) = fixture();
        let identity = table.pin(number).unwrap().description_identity();
        assert!(crate::FileObjectCheckpoint::owns(bindings.as_ref(), identity.identity, object.as_ref()).unwrap());
        assert!(!crate::FileObjectCheckpoint::owns(bindings.as_ref(), identity.identity, &CollidingFile).unwrap());
        assert!(!crate::FileObjectCheckpoint::owns(bindings.as_ref(), identity.identity + 1, object.as_ref()).unwrap());
    }

    #[test]
    fn splice_capture_rejected() {
        let (_, _, bindings, table, number, object) = fixture();
        let identity = table.pin(number).unwrap().description_identity().identity;
        let prepared = object.prepare_splice_read(None, 1, false, None).unwrap().unwrap();
        assert_eq!(
            bindings.snapshot(identity, object.as_ref()),
            Err(DescriptorCheckpointError::Object),
        );
        drop(prepared);
        assert!(bindings.snapshot(identity, object.as_ref()).is_ok());
    }

    #[test]
    fn corrupt_payload_rejected() {
        let (_, _, bindings, table, _number, _) = fixture();
        table.freeze_checkpoint();
        let mut image = table.checkpoint_image(bindings.as_ref()).unwrap();
        table.thaw_checkpoint();
        image.descriptions[0].object[25] = 2;
        assert!(matches!(
            DescriptorTable::restore_checkpoint(&image, bindings.as_ref()),
            Err(DescriptorCheckpointError::Object)
        ));
    }

    #[test]
    fn logical_size_bound() {
        let (store, _, bindings, table, _, _) = fixture();
        table.freeze_checkpoint();
        let mut image = table.checkpoint_image(bindings.as_ref()).unwrap();
        table.thaw_checkpoint();
        image.descriptions[0].object[9..17].copy_from_slice(&u64::MAX.to_le_bytes());
        let _restored = DescriptorTable::restore_checkpoint(&image, bindings.as_ref()).unwrap();
        let replacement = Arc::new(SharedObjectStore::restore(store.limits(), store.snapshot()).unwrap());
        assert!(bindings.stage(replacement).is_err());
    }

    #[test]
    fn registry_rollback_exact() {
        let (store, registry, bindings, table, _, object) = fixture();
        table.freeze_checkpoint();
        let image = table.checkpoint_image(bindings.as_ref()).unwrap();
        table.thaw_checkpoint();
        let _restored = DescriptorTable::restore_checkpoint(&image, bindings.as_ref()).unwrap();
        let replacement = Arc::new(SharedObjectStore::restore(store.limits(), store.snapshot()).unwrap());
        let mut transaction = bindings.stage(replacement).unwrap();
        transaction.commit().unwrap();
        transaction.rollback();
        let identity = table.pin(0).unwrap().description_identity();
        assert!(Arc::ptr_eq(&registry.object(identity).unwrap(), &object));
    }

    #[test]
    fn bind_retry() {
        let (store, registry, bindings, table, _, _) = fixture();
        let second = registry.create(true).unwrap();
        second.write(b"second").unwrap();
        let number = table.install(1, second.clone(), DescriptorFlags::default()).unwrap();
        registry
            .register(table.pin(number).unwrap().description_identity(), second)
            .unwrap();
        table.freeze_checkpoint();
        let image = table.checkpoint_image(bindings.as_ref()).unwrap();
        table.thaw_checkpoint();
        let _restored = DescriptorTable::restore_checkpoint(&image, bindings.as_ref()).unwrap();
        let pending = bindings
            .state
            .lock()
            .unwrap()
            .pending
            .values()
            .map(|(_, proxy)| Arc::clone(proxy))
            .collect::<Vec<_>>();
        pending.last().unwrap().fail_next_bind();
        let replacement = Arc::new(SharedObjectStore::restore(store.limits(), store.snapshot()).unwrap());
        assert!(bindings.stage(Arc::clone(&replacement)).is_err());
        assert!(matches!(pending[0].object(), Err(ObjectError::Retired)));
        let mut transaction = bindings.stage(replacement).unwrap();
        transaction.commit().unwrap();
        assert!(pending.iter().all(|proxy| proxy.object().is_ok()));
        transaction.rollback();
    }

    #[test]
    fn stage_close() {
        let (store, registry, bindings, table, _, _) = fixture();
        let second = registry.create(true).unwrap();
        second.write(b"second").unwrap();
        let number = table.install(1, second.clone(), DescriptorFlags::default()).unwrap();
        registry
            .register(table.pin(number).unwrap().description_identity(), second)
            .unwrap();
        table.freeze_checkpoint();
        let image = table.checkpoint_image(bindings.as_ref()).unwrap();
        table.thaw_checkpoint();
        let _restored = DescriptorTable::restore_checkpoint(&image, bindings.as_ref()).unwrap();
        let pending = bindings
            .state
            .lock()
            .unwrap()
            .pending
            .values()
            .map(|(_, proxy)| Arc::clone(proxy))
            .collect::<Vec<_>>();
        pending.last().unwrap().close_during_bind();
        let replacement = Arc::new(SharedObjectStore::restore(store.limits(), store.snapshot()).unwrap());
        let before = replacement.snapshot().objects.len();
        assert!(bindings.stage(Arc::clone(&replacement)).is_err());
        assert_eq!(replacement.snapshot().objects.len(), before);
        assert!(matches!(pending[0].object(), Err(ObjectError::Retired)));
    }
}
