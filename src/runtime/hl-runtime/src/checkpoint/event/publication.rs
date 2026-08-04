use std::collections::BTreeMap;
use std::fmt;
use std::sync::{Arc, Mutex, RwLock};

use hl_descriptor::{
    DescriptionRef, DescriptorCheckpointError, DescriptorObjectCheckpoint, ObjectError, ObjectKind, OfdMetadata,
    OpenDescriptionImage, OpenFileDescription, Readiness, ReadinessObserver, ReadinessSubscription, StatusFlags,
};
use hl_event::{
    EventCheckpointError, EventObjectId, EventResourceKey, Inotify, InotifySnapshot, InotifyWatchCheckpoint, SignalFd,
    SignalFdSnapshot, TimerFd, TimerFdSnapshot,
};

use super::{BindingRestore, DescriptorReference};

const OBJECT_VERSION: u8 = 1;
const OBJECT_BYTES: usize = 9;

pub trait ResourceRestore: Send + Sync {
    fn timerfd(&self, snapshot: TimerFdSnapshot, clock: EventResourceKey)
    -> Result<Arc<TimerFd>, EventCheckpointError>;

    fn signalfd(
        &self,
        snapshot: SignalFdSnapshot,
        queue: EventResourceKey,
    ) -> Result<Arc<SignalFd>, EventCheckpointError>;

    fn inotify(
        &self,
        snapshot: &InotifySnapshot,
        source: EventResourceKey,
        watches: &[InotifyWatchCheckpoint],
    ) -> Result<Arc<Inotify>, EventCheckpointError>;
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Staging,
    Committed,
    Resumed,
}

struct State {
    descriptors: BTreeMap<EventResourceKey, DescriptorReference>,
    identities: BTreeMap<u64, EventObjectId>,
    inotify_sources: BTreeMap<u64, EventResourceKey>,
    pending: BTreeMap<EventObjectId, Vec<Arc<PendingObject>>>,
    rebound: BTreeMap<EventObjectId, Arc<dyn OpenFileDescription>>,
    phase: Phase,
}

pub struct ObjectBindings {
    fallback: Arc<dyn DescriptorObjectCheckpoint>,
    resources: Arc<dyn ResourceRestore>,
    state: Mutex<State>,
}

impl ObjectBindings {
    #[must_use]
    pub fn new(fallback: Arc<dyn DescriptorObjectCheckpoint>, resources: Arc<dyn ResourceRestore>) -> Self {
        Self {
            fallback,
            resources,
            state: Mutex::new(State {
                descriptors: BTreeMap::new(),
                identities: BTreeMap::new(),
                inotify_sources: BTreeMap::new(),
                pending: BTreeMap::new(),
                rebound: BTreeMap::new(),
                phase: Phase::Staging,
            }),
        }
    }

    pub fn register_descriptor(
        &self,
        key: EventResourceKey,
        reference: DescriptorReference,
    ) -> Result<(), EventCheckpointError> {
        let mut state = self.state.lock().map_err(|_| EventCheckpointError::InvalidImage)?;
        if state.phase == Phase::Resumed {
            state.phase = Phase::Staging;
        }
        if state.phase != Phase::Staging {
            return Err(EventCheckpointError::InvalidImage);
        }
        if let Some(current) = state.descriptors.get(&key) {
            return (*current == reference)
                .then_some(())
                .ok_or(EventCheckpointError::InvalidImage);
        }
        state.descriptors.insert(key, reference);
        Ok(())
    }

    pub fn register_object(&self, identity: u64, id: EventObjectId) -> Result<(), EventCheckpointError> {
        let mut state = self.state.lock().map_err(|_| EventCheckpointError::InvalidImage)?;
        if state.phase == Phase::Resumed {
            state.phase = Phase::Staging;
        }
        if identity == 0 || state.phase != Phase::Staging {
            return Err(EventCheckpointError::InvalidImage);
        }
        if let Some(current) = state.identities.get(&identity) {
            return (*current == id).then_some(()).ok_or(EventCheckpointError::InvalidImage);
        }
        state.identities.insert(identity, id);
        Ok(())
    }

    pub fn unregister_object(&self, identity: u64, id: EventObjectId) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        if state.identities.get(&identity) == Some(&id) {
            state.identities.remove(&identity);
            state.inotify_sources.remove(&identity);
        }
    }

    pub fn object_id(&self, identity: u64) -> Option<EventObjectId> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .identities
            .get(&identity)
            .copied()
    }

    pub fn register_inotify_source(&self, identity: u64, source: EventResourceKey) -> Result<(), EventCheckpointError> {
        let mut state = self.state.lock().map_err(|_| EventCheckpointError::InvalidImage)?;
        if state.phase == Phase::Resumed {
            state.phase = Phase::Staging;
        }
        if identity == 0 || state.phase != Phase::Staging {
            return Err(EventCheckpointError::InvalidImage);
        }
        if let Some(current) = state.inotify_sources.get(&identity) {
            return (*current == source)
                .then_some(())
                .ok_or(EventCheckpointError::InvalidImage);
        }
        state.inotify_sources.insert(identity, source);
        Ok(())
    }

    pub fn inotify_source(&self, identity: u64) -> Option<EventResourceKey> {
        self.state
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .inotify_sources
            .get(&identity)
            .copied()
    }

    fn decode(description: &OpenDescriptionImage) -> Result<EventObjectId, DescriptorCheckpointError> {
        if description.object.len() != OBJECT_BYTES || description.object[0] != OBJECT_VERSION {
            return Err(DescriptorCheckpointError::Object);
        }
        let slot = u32::from_le_bytes(description.object[1..5].try_into().unwrap());
        let generation = u32::from_le_bytes(description.object[5..9].try_into().unwrap());
        if generation == 0 {
            return Err(DescriptorCheckpointError::Object);
        }
        Ok(EventObjectId { slot, generation })
    }
}

impl DescriptorObjectCheckpoint for ObjectBindings {
    fn snapshot(&self, identity: u64, object: &dyn OpenFileDescription) -> Result<Vec<u8>, DescriptorCheckpointError> {
        if matches!(
            object.kind(),
            ObjectKind::Event | ObjectKind::EventCounter | ObjectKind::Poll
        ) {
            let state = self.state.lock().map_err(|_| DescriptorCheckpointError::Object)?;
            let id = state
                .identities
                .get(&identity)
                .ok_or(DescriptorCheckpointError::Object)?;
            let mut bytes = Vec::with_capacity(OBJECT_BYTES);
            bytes.push(OBJECT_VERSION);
            bytes.extend_from_slice(&id.slot.to_le_bytes());
            bytes.extend_from_slice(&id.generation.to_le_bytes());
            return Ok(bytes);
        }
        self.fallback.snapshot(identity, object)
    }

    fn rebind(
        &self,
        description: &OpenDescriptionImage,
    ) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError> {
        if !matches!(
            description.kind,
            ObjectKind::Event | ObjectKind::EventCounter | ObjectKind::Poll
        ) {
            return self.fallback.rebind(description);
        }
        let id = Self::decode(description)?;
        let pending = Arc::new(PendingObject::new(description.kind));
        let mut state = self.state.lock().map_err(|_| DescriptorCheckpointError::Object)?;
        if state.phase == Phase::Resumed {
            state.phase = Phase::Staging;
        }
        if state.phase != Phase::Staging {
            return Err(DescriptorCheckpointError::Object);
        }
        if let Some(current) = state.identities.get(&description.identity) {
            if *current != id {
                return Err(DescriptorCheckpointError::Object);
            }
        } else {
            state.identities.insert(description.identity, id);
        }
        state.pending.entry(id).or_default().push(pending.clone());
        Ok(pending)
    }
}

impl BindingRestore for ObjectBindings {
    fn descriptor(&self, key: EventResourceKey) -> Result<DescriptorReference, EventCheckpointError> {
        if let Some(reference) = self
            .state
            .lock()
            .map_err(|_| EventCheckpointError::InvalidImage)?
            .descriptors
            .get(&key)
            .copied()
        {
            return Ok(reference);
        }
        let encoded = key.value();
        let number = u32::try_from(encoded >> 32)
            .ok()
            .and_then(|value| value.checked_sub(1))
            .and_then(|value| i32::try_from(value).ok())
            .ok_or(EventCheckpointError::InvalidImage)?;
        let generation = encoded as u32;
        if generation == 0 {
            return Err(EventCheckpointError::InvalidImage);
        }
        Ok(DescriptorReference { number, generation })
    }

    fn timerfd(
        &self,
        snapshot: TimerFdSnapshot,
        clock: EventResourceKey,
    ) -> Result<Arc<TimerFd>, EventCheckpointError> {
        self.resources.timerfd(snapshot, clock)
    }

    fn signalfd(
        &self,
        snapshot: SignalFdSnapshot,
        task_queue: EventResourceKey,
    ) -> Result<Arc<SignalFd>, EventCheckpointError> {
        self.resources.signalfd(snapshot, task_queue)
    }

    fn inotify(
        &self,
        snapshot: &InotifySnapshot,
        source: EventResourceKey,
        watches: &[InotifyWatchCheckpoint],
    ) -> Result<Arc<Inotify>, EventCheckpointError> {
        self.resources.inotify(snapshot, source, watches)
    }

    fn bind(&self, id: EventObjectId, object: Arc<dyn OpenFileDescription>) -> Result<(), EventCheckpointError> {
        let mut state = self.state.lock().map_err(|_| EventCheckpointError::InvalidImage)?;
        if state.phase != Phase::Staging {
            return Err(EventCheckpointError::InvalidImage);
        }
        let pending = state.pending.get(&id).cloned().unwrap_or_default();
        if state.rebound.insert(id, object.clone()).is_some() {
            return Err(EventCheckpointError::InvalidImage);
        }
        drop(state);
        for target in pending {
            target.bind(object.clone())?;
        }
        Ok(())
    }

    fn commit(&self) -> Result<(), EventCheckpointError> {
        let mut state = self.state.lock().map_err(|_| EventCheckpointError::InvalidImage)?;
        if state.phase != Phase::Staging || state.pending.values().flatten().any(|object| !object.bound()) {
            return Err(EventCheckpointError::InvalidImage);
        }
        state.phase = Phase::Committed;
        Ok(())
    }

    fn rollback(&self) {
        let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
        state.pending.clear();
        let rebound = std::mem::take(&mut state.rebound);
        state.phase = Phase::Staging;
        drop(state);
        for object in rebound.into_values() {
            object.retire();
            object.close();
        }
    }

    fn resume(&self) -> Result<(), EventCheckpointError> {
        let mut state = self.state.lock().map_err(|_| EventCheckpointError::InvalidImage)?;
        if state.phase != Phase::Committed {
            return Err(EventCheckpointError::InvalidImage);
        }
        state.pending.clear();
        state.rebound.clear();
        state.phase = Phase::Resumed;
        Ok(())
    }
}

struct PendingObject {
    kind: ObjectKind,
    object: RwLock<Option<Arc<dyn OpenFileDescription>>>,
}

impl PendingObject {
    const fn new(kind: ObjectKind) -> Self {
        Self {
            kind,
            object: RwLock::new(None),
        }
    }

    fn bind(&self, object: Arc<dyn OpenFileDescription>) -> Result<(), EventCheckpointError> {
        if object.kind() != self.kind {
            return Err(EventCheckpointError::InvalidImage);
        }
        let mut current = self.object.write().map_err(|_| EventCheckpointError::InvalidImage)?;
        if current.is_some() {
            return Err(EventCheckpointError::InvalidImage);
        }
        *current = Some(object);
        Ok(())
    }

    fn bound(&self) -> bool {
        self.object.read().unwrap_or_else(|error| error.into_inner()).is_some()
    }

    fn current(&self) -> Result<Arc<dyn OpenFileDescription>, ObjectError> {
        self.object
            .read()
            .map_err(|_| ObjectError::Io)?
            .clone()
            .ok_or(ObjectError::Busy)
    }
}

impl fmt::Debug for PendingObject {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PendingObject")
            .field("kind", &self.kind)
            .field("bound", &self.bound())
            .finish()
    }
}

impl OpenFileDescription for PendingObject {
    fn transfer_dependencies(&self) -> Vec<DescriptionRef> {
        self.current()
            .map_or_else(|_| Vec::new(), |object| object.transfer_dependencies())
    }

    fn kind(&self) -> ObjectKind {
        self.kind
    }

    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.current()?.read(output)
    }

    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        self.current()?.write(input)
    }

    fn metadata(&self) -> Result<OfdMetadata, ObjectError> {
        self.current()?.metadata()
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

#[cfg(test)]
mod tests {
    use super::super::{Catalog, DescriptorRebind, Participant, WireCodec};
    use super::*;
    use crate::{CheckpointDescriptorTable, CheckpointParticipant, DescriptorCheckpointParticipant};
    use hl_checkpoint::{Section, SectionKind};
    use hl_descriptor::{DescriptorFlags, DescriptorTable};
    use hl_event::{Epoll, EpollInterest, EpollTargetCheckpoint, EventCatalog, EventFd, EventFdFlags};

    #[derive(Debug)]
    struct File;

    impl OpenFileDescription for File {
        fn kind(&self) -> ObjectKind {
            ObjectKind::File
        }
    }

    struct Fallback;

    impl DescriptorObjectCheckpoint for Fallback {
        fn snapshot(&self, _: u64, _: &dyn OpenFileDescription) -> Result<Vec<u8>, DescriptorCheckpointError> {
            Ok(vec![7])
        }

        fn rebind(
            &self,
            description: &OpenDescriptionImage,
        ) -> Result<Arc<dyn OpenFileDescription>, DescriptorCheckpointError> {
            if description.object == [7] {
                Ok(Arc::new(File))
            } else {
                Err(DescriptorCheckpointError::Object)
            }
        }
    }

    struct Resources;

    impl ResourceRestore for Resources {
        fn timerfd(&self, _: TimerFdSnapshot, _: EventResourceKey) -> Result<Arc<TimerFd>, EventCheckpointError> {
            Err(EventCheckpointError::InvalidImage)
        }

        fn signalfd(&self, _: SignalFdSnapshot, _: EventResourceKey) -> Result<Arc<SignalFd>, EventCheckpointError> {
            Err(EventCheckpointError::InvalidImage)
        }

        fn inotify(
            &self,
            _: &InotifySnapshot,
            _: EventResourceKey,
            _: &[InotifyWatchCheckpoint],
        ) -> Result<Arc<Inotify>, EventCheckpointError> {
            Err(EventCheckpointError::InvalidImage)
        }
    }

    fn bindings() -> Arc<ObjectBindings> {
        Arc::new(ObjectBindings::new(Arc::new(Fallback), Arc::new(Resources)))
    }

    fn image(object: Vec<u8>) -> OpenDescriptionImage {
        OpenDescriptionImage {
            identity: 4,
            generation: 2,
            offset: 0,
            status: StatusFlags::default(),
            kind: ObjectKind::EventCounter,
            object,
        }
    }

    #[test]
    fn alias_publication() {
        let bindings = bindings();
        let id = EventObjectId { slot: 3, generation: 8 };
        bindings.register_object(4, id).unwrap();
        let event = Arc::new(EventFd::new(11, EventFdFlags::default()).unwrap());
        let bytes = bindings.snapshot(4, event.as_ref()).unwrap();
        let image = image(bytes);
        let first = bindings.rebind(&image).unwrap();
        let second = bindings.rebind(&image).unwrap();
        let table = DescriptorTable::new(8).unwrap();
        let one = table.install(0, first, DescriptorFlags::default()).unwrap();
        let two = table.install(0, second, DescriptorFlags::default()).unwrap();
        assert_eq!(table.pin(one).unwrap().read(&mut [0; 8]), Err(ObjectError::Busy));
        bindings.bind(id, event).unwrap();
        bindings.commit().unwrap();
        bindings.resume().unwrap();
        let mut output = [0; 8];
        assert_eq!(table.pin(one).unwrap().read(&mut output).unwrap(), 8);
        assert_eq!(u64::from_ne_bytes(output), 11);
        assert_eq!(table.pin(two).unwrap().write(&5_u64.to_ne_bytes()).unwrap(), 8);
    }

    #[test]
    fn corruption_and_rollback() {
        let bindings = bindings();
        for bytes in [vec![], vec![2; OBJECT_BYTES], vec![1; OBJECT_BYTES - 1]] {
            assert_eq!(
                bindings.rebind(&image(bytes)).unwrap_err(),
                DescriptorCheckpointError::Object,
            );
        }
        let id = EventObjectId { slot: 1, generation: 2 };
        let mut bytes = vec![OBJECT_VERSION];
        bytes.extend_from_slice(&id.slot.to_le_bytes());
        bytes.extend_from_slice(&id.generation.to_le_bytes());
        bindings.rebind(&image(bytes)).unwrap();
        assert_eq!(bindings.commit(), Err(EventCheckpointError::InvalidImage));
        bindings.rollback();
        assert_eq!(bindings.resume(), Err(EventCheckpointError::InvalidImage));
    }

    #[test]
    fn exact_descriptor_keys() {
        let bindings = bindings();
        let key = EventResourceKey::new(9).unwrap();
        let reference = DescriptorReference {
            number: 4,
            generation: 7,
        };
        bindings.register_descriptor(key, reference).unwrap();
        assert_eq!(bindings.descriptor(key), Ok(reference));
        bindings.register_descriptor(key, reference).unwrap();
        assert!(
            bindings
                .register_descriptor(
                    key,
                    DescriptorReference {
                        number: 4,
                        generation: 8,
                    }
                )
                .is_err()
        );
        assert!(bindings.descriptor(EventResourceKey::new(10).unwrap()).is_err());
    }

    #[test]
    fn epoll_rebind_transaction() {
        let bindings = bindings();
        let table = Arc::new(DescriptorTable::new(8).unwrap());
        let eventfd = Arc::new(EventFd::new(13, EventFdFlags::default()).unwrap());
        let target = table.install(0, eventfd.clone(), DescriptorFlags::default()).unwrap();
        let target_lease = table.pin(target).unwrap();
        let epoll = Arc::new(Epoll::new());
        epoll
            .add(target_lease.clone(), EpollInterest::from_bits(EpollInterest::READ), 77)
            .unwrap();
        let source = table.install(0, epoll.clone(), DescriptorFlags::default()).unwrap();
        let source_lease = table.pin(source).unwrap();
        let catalog = Arc::new(EventCatalog::new(2).unwrap());
        let event_id = catalog.insert_eventfd(eventfd).unwrap();
        let key = EventResourceKey::new(44).unwrap();
        let epoll_id = catalog
            .insert_epoll(
                epoll,
                vec![EpollTargetCheckpoint {
                    watch: 0,
                    descriptor: key,
                    nested: None,
                }],
            )
            .unwrap();
        let target_identity = target_lease.description_identity().identity;
        let source_identity = source_lease.description_identity().identity;
        let target_generation = target_lease.descriptor_generation();
        drop(target_lease);
        drop(source_lease);
        bindings.register_object(target_identity, event_id).unwrap();
        bindings.register_object(source_identity, epoll_id).unwrap();
        bindings
            .register_descriptor(
                key,
                DescriptorReference {
                    number: target,
                    generation: target_generation,
                },
            )
            .unwrap();

        let tables = Arc::new(CheckpointDescriptorTable::new(table));
        let descriptors = DescriptorCheckpointParticipant::new(tables.clone(), bindings.clone());
        let catalogs = Arc::new(Catalog::new(catalog));
        let events = Participant::new(
            catalogs.clone(),
            Arc::new(DescriptorRebind::new(tables, bindings)),
            Arc::new(WireCodec),
        );
        descriptors.freeze().unwrap();
        events.freeze().unwrap();
        let descriptor_section = Section::new(
            SectionKind::new(2).unwrap(),
            descriptors.version(),
            descriptors.snapshot().unwrap(),
        );
        let event_section = Section::new(
            SectionKind::new(5).unwrap(),
            events.version(),
            events.snapshot().unwrap(),
        );
        events.thaw().unwrap();
        descriptors.thaw().unwrap();

        let descriptor_reservation = descriptors.stage(&descriptor_section).unwrap();
        let event_reservation = events.stage(&event_section).unwrap();
        descriptors.commit(descriptor_reservation).unwrap();
        events.commit(event_reservation).unwrap();
        descriptors.resume(descriptor_reservation).unwrap();
        events.resume(event_reservation).unwrap();
        descriptors.finish(descriptor_reservation);
        events.finish(event_reservation);

        catalogs
            .current()
            .with_epoll(epoll_id, |restored| {
                let snapshot = restored.snapshot();
                assert_eq!(snapshot.watches.len(), 1);
                assert_eq!(snapshot.watches[0].data, 77);
                assert_eq!(snapshot.watches[0].key.descriptor_number, target);
            })
            .unwrap();
    }
}
