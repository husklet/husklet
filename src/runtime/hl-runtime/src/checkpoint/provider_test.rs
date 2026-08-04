use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hl_checkpoint::{Section, SectionKind};
use hl_provider::{
    HandleKind, HandleNamespace, NamespaceError, PROVIDER_CHECKPOINT_VERSION, ProviderCheckpointCapture,
    ProviderCheckpointImage, ProviderCheckpointReconnect, ProviderClientCheckpoint, ProviderRemoteRestore,
    ProviderResourceKey, ProviderSubscriptionCheckpoint, RemoteId,
};

use crate::{
    CheckpointParticipant, CheckpointProviderNamespace, PortableProviderCodec, ProviderCheckpointCodec,
    ProviderCheckpointParticipant, ProviderRegistry,
};

fn client(events: &[&[u8]]) -> ProviderClientCheckpoint {
    ProviderClientCheckpoint {
        request_generations: vec![0, 2],
        subscription_generations: vec![0, 7],
        next_request: 3,
        next_subscription: 4,
        late_replies: 1,
        stale_events: 2,
        subscriptions: vec![ProviderSubscriptionCheckpoint {
            slot: 1,
            identity_owner: 13,
            identity_generation: 2,
            key_id: 3,
            key_generation: 7,
            queued: events.iter().map(|event| event.to_vec()).collect(),
            lost: 1,
        }],
    }
}

#[test]
fn portable_event_order() {
    let namespace = HandleNamespace::new(2).unwrap();
    namespace.open(RemoteId::new(9).unwrap(), HandleKind::File).unwrap();
    namespace.freeze_checkpoint();
    let mut image = ProviderCheckpointImage::capture(
        &namespace,
        &External {
            state: Arc::new(State::default()),
        },
    )
    .unwrap();
    namespace.thaw_checkpoint();
    image.client.subscription_generations = vec![0, 7];
    image.client.next_subscription = 4;
    image.client.subscriptions.push(ProviderSubscriptionCheckpoint {
        slot: 1,
        identity_owner: 13,
        identity_generation: 2,
        key_id: 3,
        key_generation: 7,
        queued: vec![b"first".to_vec(), b"second".to_vec()],
        lost: 1,
    });
    let codec = PortableProviderCodec;
    let bytes = codec.encode(&image).unwrap();
    let decoded = codec.decode(&bytes).unwrap();
    assert_eq!(decoded, image);
    assert_eq!(decoded.client.subscriptions[0].queued[0], b"first");
    assert_eq!(decoded.client.subscriptions[0].queued[1], b"second");

    let mut trailing = bytes;
    trailing.push(0);
    assert!(codec.decode(&trailing).is_err());
    image.client.subscriptions[0].key_generation = 8;
    assert!(codec.encode(&image).is_err());
}

fn registry_fixture() -> (
    Arc<HandleNamespace>,
    Arc<CheckpointProviderNamespace>,
    Arc<ProviderRegistry>,
    ProviderCheckpointParticipant,
) {
    let namespace = Arc::new(HandleNamespace::new(2).unwrap());
    let current = Arc::new(CheckpointProviderNamespace::new(namespace.clone()));
    let registry = Arc::new(ProviderRegistry::new());
    let participant = ProviderCheckpointParticipant::new(
        current.clone(),
        registry.clone(),
        registry.clone(),
        Arc::new(PortableProviderCodec),
    );
    (namespace, current, registry, participant)
}

fn section(participant: &ProviderCheckpointParticipant) -> Section {
    participant.freeze().unwrap();
    let bytes = participant.snapshot().unwrap();
    participant.thaw().unwrap();
    Section::new(SectionKind::new(4).unwrap(), PROVIDER_CHECKPOINT_VERSION, bytes)
}

#[test]
fn lease_lifetime() {
    let (namespace, _, registry, participant) = registry_fixture();
    namespace.open(RemoteId::new(9).unwrap(), HandleKind::File).unwrap();
    let first = registry.register(RemoteId::new(9).unwrap()).unwrap();
    let key = first.key();
    let second = first.try_clone().unwrap();
    assert_eq!(second.key(), key);
    drop(first);
    let _ = section(&participant);
    drop(second);
    participant.freeze().unwrap();
    assert!(participant.snapshot().is_err());
    participant.thaw().unwrap();
    let replacement = registry.register(RemoteId::new(9).unwrap()).unwrap();
    assert!(replacement.key().get() > key.get());
}

#[test]
fn registry_rollback() {
    let (namespace, current, registry, participant) = registry_fixture();
    namespace.open(RemoteId::new(9).unwrap(), HandleKind::File).unwrap();
    let lease = registry.register(RemoteId::new(9).unwrap()).unwrap();
    registry
        .replace_projected(Vec::new(), client(&[b"first", b"second"]))
        .unwrap();
    let captured = section(&participant);
    registry.replace_projected(Vec::new(), client(&[b"mutated"])).unwrap();
    let previous = current.current();
    let reservation = participant.stage(&captured).unwrap();
    participant.commit(reservation).unwrap();
    participant.resume(reservation).unwrap();
    assert_eq!(
        registry.projected().1.subscriptions[0].queued,
        vec![b"first".to_vec(), b"second".to_vec()]
    );
    participant.rollback(reservation);
    assert!(Arc::ptr_eq(&current.current(), &previous));
    assert_eq!(
        registry.projected().1.subscriptions[0].queued,
        vec![b"mutated".to_vec()]
    );
    assert_eq!(lease.remote(), RemoteId::new(9).unwrap());
}

#[derive(Default)]
struct Codec {
    next: AtomicU64,
    images: Mutex<BTreeMap<u64, ProviderCheckpointImage>>,
}

impl ProviderCheckpointCodec for Codec {
    fn encode(&self, image: &ProviderCheckpointImage) -> Result<Vec<u8>, ()> {
        let key = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.images.lock().map_err(|_| ())?.insert(key, image.clone());
        Ok(key.to_le_bytes().to_vec())
    }

    fn decode(&self, bytes: &[u8]) -> Result<ProviderCheckpointImage, ()> {
        let key = u64::from_le_bytes(bytes.try_into().map_err(|_| ())?);
        self.images.lock().map_err(|_| ())?.get(&key).cloned().ok_or(())
    }
}

#[derive(Default)]
struct State {
    frozen: AtomicBool,
    failure: AtomicUsize,
    remote_calls: AtomicUsize,
    rollbacks: AtomicUsize,
}

impl State {
    fn assert_restore_failure(failure: usize) {
        let (handle, state, participant) = fixture();
        let previous = handle.current();
        participant.freeze().unwrap();
        let section = Section::new(
            SectionKind::new(4).unwrap(),
            PROVIDER_CHECKPOINT_VERSION,
            participant.snapshot().unwrap(),
        );
        participant.thaw().unwrap();
        state.failure.store(failure, Ordering::Relaxed);
        let staged = participant.stage(&section);
        if let Ok(reservation) = staged {
            let _ = participant
                .commit(reservation)
                .and_then(|()| participant.resume(reservation));
            participant.rollback(reservation);
        }
        assert!(Arc::ptr_eq(&handle.current(), &previous));
        previous.open(RemoteId::new(10).unwrap(), HandleKind::Event).unwrap();
    }
}

struct External {
    state: Arc<State>,
}

impl ProviderCheckpointCapture for External {
    fn freeze(&self) -> Result<(), NamespaceError> {
        self.state.frozen.store(true, Ordering::Relaxed);
        Ok(())
    }

    fn thaw(&self) {
        self.state.frozen.store(false, Ordering::Relaxed);
    }

    fn resource_key(&self, slot: usize, _: RemoteId) -> Result<ProviderResourceKey, NamespaceError> {
        ProviderResourceKey::new(slot as u64 + 1).ok_or(NamespaceError::InvalidSnapshot)
    }

    fn projected_state(
        &self,
    ) -> Result<(Vec<hl_provider::ProviderFileCheckpoint>, ProviderClientCheckpoint), NamespaceError> {
        Ok((
            Vec::new(),
            ProviderClientCheckpoint {
                request_generations: vec![0],
                subscription_generations: vec![0],
                next_request: 1,
                next_subscription: 1,
                late_replies: 0,
                stale_events: 0,
                subscriptions: Vec::new(),
            },
        ))
    }
}

struct RemoteTransaction {
    state: Arc<State>,
}

impl ProviderRemoteRestore for RemoteTransaction {
    fn remote(&mut self, key: ProviderResourceKey) -> Result<RemoteId, NamespaceError> {
        self.state.remote_calls.fetch_add(1, Ordering::Relaxed);
        if self.state.failure.load(Ordering::Relaxed) == 4 && key.get() == 2 {
            return Err(NamespaceError::InvalidSnapshot);
        }
        RemoteId::new(key.get() + 100).ok_or(NamespaceError::InvalidSnapshot)
    }

    fn commit(&mut self) -> Result<(), NamespaceError> {
        if self.state.failure.load(Ordering::Relaxed) == 2 {
            return Err(NamespaceError::InvalidSnapshot);
        }
        Ok(())
    }

    fn rollback(&mut self) {
        self.state.rollbacks.fetch_add(1, Ordering::Relaxed);
    }

    fn resume(&mut self) -> Result<(), NamespaceError> {
        if self.state.failure.load(Ordering::Relaxed) == 3 {
            return Err(NamespaceError::InvalidSnapshot);
        }
        Ok(())
    }
}

impl ProviderCheckpointReconnect for External {
    fn stage(&self, _: &ProviderCheckpointImage) -> Result<Box<dyn ProviderRemoteRestore>, NamespaceError> {
        if self.state.failure.load(Ordering::Relaxed) == 1 {
            return Err(NamespaceError::InvalidSnapshot);
        }
        Ok(Box::new(RemoteTransaction {
            state: self.state.clone(),
        }))
    }
}

fn fixture() -> (
    Arc<CheckpointProviderNamespace>,
    Arc<State>,
    ProviderCheckpointParticipant,
) {
    let namespace = Arc::new(HandleNamespace::new(2).unwrap());
    namespace.open(RemoteId::new(9).unwrap(), HandleKind::File).unwrap();
    let handle = Arc::new(CheckpointProviderNamespace::new(namespace));
    let state = Arc::new(State::default());
    let external = Arc::new(External { state: state.clone() });
    let participant =
        ProviderCheckpointParticipant::new(handle.clone(), external.clone(), external, Arc::new(Codec::default()));
    (handle, state, participant)
}

#[test]
fn restore_previous_state() {
    let (handle, _, participant) = fixture();
    let previous = handle.current();
    let weak = Arc::downgrade(&previous);
    participant.freeze().unwrap();
    let section = Section::new(
        SectionKind::new(4).unwrap(),
        PROVIDER_CHECKPOINT_VERSION,
        participant.snapshot().unwrap(),
    );
    participant.thaw().unwrap();
    let reservation = participant.stage(&section).unwrap();
    participant.commit(reservation).unwrap();
    participant.resume(reservation).unwrap();
    assert_eq!(
        handle.current().snapshot().entries[0].remote,
        RemoteId::new(101).unwrap()
    );
    drop(previous);
    assert!(weak.upgrade().is_some());
    participant.finish(reservation);
    assert!(weak.upgrade().is_none());
}

#[test]
fn stage_previous_namespace() {
    for failure in 1..=3 {
        State::assert_restore_failure(failure);
    }
}

#[test]
fn partial_once_swapping() {
    let namespace = Arc::new(HandleNamespace::new(2).unwrap());
    namespace.open(RemoteId::new(9).unwrap(), HandleKind::File).unwrap();
    namespace.open(RemoteId::new(10).unwrap(), HandleKind::Event).unwrap();
    let handle = Arc::new(CheckpointProviderNamespace::new(namespace));
    let state = Arc::new(State::default());
    let external = Arc::new(External { state: state.clone() });
    let participant =
        ProviderCheckpointParticipant::new(handle.clone(), external.clone(), external, Arc::new(Codec::default()));
    let previous = handle.current();
    participant.freeze().unwrap();
    let section = Section::new(
        SectionKind::new(4).unwrap(),
        PROVIDER_CHECKPOINT_VERSION,
        participant.snapshot().unwrap(),
    );
    participant.thaw().unwrap();
    state.failure.store(4, Ordering::Relaxed);
    assert!(participant.stage(&section).is_err());
    assert!(Arc::ptr_eq(&handle.current(), &previous));
    assert_eq!(state.remote_calls.load(Ordering::Relaxed), 2);
    assert_eq!(state.rollbacks.load(Ordering::Relaxed), 1);
}

#[test]
fn downstream_provider_swap() {
    let (handle, state, participant) = fixture();
    let previous = handle.current();
    participant.freeze().unwrap();
    let section = Section::new(
        SectionKind::new(4).unwrap(),
        PROVIDER_CHECKPOINT_VERSION,
        participant.snapshot().unwrap(),
    );
    participant.thaw().unwrap();
    let reservation = participant.stage(&section).unwrap();
    participant.commit(reservation).unwrap();
    participant.resume(reservation).unwrap();
    participant.rollback(reservation);
    assert!(Arc::ptr_eq(&handle.current(), &previous));
    assert_eq!(state.rollbacks.load(Ordering::Relaxed), 1);
}

#[test]
fn stale_namespace_swap() {
    let namespace = Arc::new(HandleNamespace::new(1).unwrap());
    namespace.open(RemoteId::new(9).unwrap(), HandleKind::File).unwrap();
    let current = Arc::new(CheckpointProviderNamespace::new(namespace));
    let codec = Arc::new(Codec::default());
    let first_state = Arc::new(State::default());
    let second_state = Arc::new(State::default());
    let first_external = Arc::new(External { state: first_state });
    let second_external = Arc::new(External {
        state: second_state.clone(),
    });
    let first =
        ProviderCheckpointParticipant::new(current.clone(), first_external.clone(), first_external, codec.clone());
    let second = ProviderCheckpointParticipant::new(current, second_external.clone(), second_external, codec);
    let captured = section(&first);
    let first_reservation = first.stage(&captured).unwrap();
    let stale_reservation = second.stage(&captured).unwrap();
    first.commit(first_reservation).unwrap();
    assert!(second.commit(stale_reservation).is_err());
    second.rollback(stale_reservation);
    assert_eq!(second_state.rollbacks.load(Ordering::Relaxed), 1);
    first.resume(first_reservation).unwrap();
    first.finish(first_reservation);
}
