use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hl_checkpoint::{Section, SectionKind};
use hl_ipc::{
    Credentials, IPC_CHECKPOINT_VERSION, IpcCatalog, IpcCatalogRestore, IpcCheckpointError, IpcCheckpointImage,
    IpcCheckpointRebind, IpcKey, IpcResourceKey, MessageLimits, MessageQueueNamespace, Pipe, PipeEndpointBinding,
    PipeEndpointKind, SEM_UNDO, SemGetRequest, SemaphoreLimits, SemaphoreNamespace, SemaphoreOperation,
    SharedBackingAccess, SharedBackingCheckpoint, SharedBackingKey, SharedMemoryLimits, SharedMemoryNamespace,
    ShmGetRequest, TaskCheckpoint,
};
use hl_memory::{SharedLimits, SharedObjectStore};

use crate::{CheckpointIpcCatalog, CheckpointParticipant, IpcCheckpointCodec, IpcCheckpointParticipant};

#[derive(Default)]
struct Codec {
    next: AtomicU64,
    images: Mutex<BTreeMap<u64, IpcCheckpointImage>>,
}

impl IpcCheckpointCodec for Codec {
    fn encode(&self, image: &IpcCheckpointImage) -> Result<Vec<u8>, ()> {
        let key = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.images.lock().map_err(|_| ())?.insert(key, image.clone());
        Ok(key.to_le_bytes().to_vec())
    }

    fn decode(&self, bytes: &[u8]) -> Result<IpcCheckpointImage, ()> {
        let key = u64::from_le_bytes(bytes.try_into().map_err(|_| ())?);
        self.images.lock().map_err(|_| ())?.get(&key).cloned().ok_or(())
    }
}

#[derive(Default)]
struct State {
    failure: AtomicUsize,
    descriptors: AtomicUsize,
    bindings: AtomicUsize,
    rollbacks: AtomicUsize,
    tasks: AtomicUsize,
    memory: Mutex<Option<Arc<SharedObjectStore>>>,
}

struct Rebind(Arc<State>);
struct Transaction(Arc<State>);
struct Binding(Arc<State>);
struct InstalledBinding;

impl PipeEndpointBinding for InstalledBinding {
    fn bind(&self, _: Arc<hl_ipc::PipeEndpoint>) -> Result<(), IpcCheckpointError> {
        Ok(())
    }
}

impl PipeEndpointBinding for Binding {
    fn bind(&self, _: Arc<hl_ipc::PipeEndpoint>) -> Result<(), IpcCheckpointError> {
        self.0.bindings.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

impl IpcCheckpointRebind for Rebind {
    fn stage(&self, _: &IpcCheckpointImage) -> Result<Box<dyn IpcCatalogRestore>, IpcCheckpointError> {
        if self.0.failure.load(Ordering::Relaxed) == 1 {
            return Err(IpcCheckpointError::InvalidImage);
        }
        Ok(Box::new(Transaction(self.0.clone())))
    }
}

impl IpcCatalogRestore for Transaction {
    fn memory(&mut self, _: &[SharedBackingCheckpoint]) -> Result<Arc<dyn SharedBackingAccess>, IpcCheckpointError> {
        if self.0.failure.load(Ordering::Relaxed) == 6 {
            return Err(IpcCheckpointError::InvalidImage);
        }
        self.0
            .memory
            .lock()
            .map_err(|_| IpcCheckpointError::InvalidImage)?
            .clone()
            .map(|memory| memory as Arc<dyn SharedBackingAccess>)
            .ok_or(IpcCheckpointError::InvalidImage)
    }

    fn descriptor(
        &mut self,
        _: IpcResourceKey,
        _: PipeEndpointKind,
    ) -> Result<Arc<dyn PipeEndpointBinding>, IpcCheckpointError> {
        let count = self.0.descriptors.fetch_add(1, Ordering::Relaxed) + 1;
        if self.0.failure.load(Ordering::Relaxed) == 2 && count == 3 {
            return Err(IpcCheckpointError::InvalidImage);
        }
        Ok(Arc::new(Binding(Arc::clone(&self.0))))
    }

    fn task(&mut self, _: TaskCheckpoint) -> Result<(), IpcCheckpointError> {
        let count = self.0.tasks.fetch_add(1, Ordering::Relaxed) + 1;
        if self.0.failure.load(Ordering::Relaxed) == 5 && count == 2 {
            return Err(IpcCheckpointError::InvalidImage);
        }
        Ok(())
    }

    fn commit(&mut self) -> Result<(), IpcCheckpointError> {
        if self.0.failure.load(Ordering::Relaxed) == 3 {
            return Err(IpcCheckpointError::InvalidImage);
        }
        Ok(())
    }

    fn rollback(&mut self) {
        self.0.rollbacks.fetch_add(1, Ordering::Relaxed);
    }

    fn resume(&mut self) -> Result<(), IpcCheckpointError> {
        if self.0.failure.load(Ordering::Relaxed) == 4 {
            return Err(IpcCheckpointError::InvalidImage);
        }
        Ok(())
    }
}

struct IpcScenario;

impl IpcScenario {
    const OWNER: Credentials = Credentials { uid: 10, gid: 20 };

    fn catalog(pipe_count: usize) -> (Arc<IpcCatalog>, Arc<SharedObjectStore>) {
        let memory = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
        let catalog = Arc::new(IpcCatalog::new(
            Arc::new(SharedMemoryNamespace::new(memory.clone(), SharedMemoryLimits::default()).unwrap()),
            SharedMemoryLimits::default(),
            Vec::new(),
            Arc::new(MessageQueueNamespace::new(MessageLimits::default()).unwrap()),
            MessageLimits::default(),
            Arc::new(SemaphoreNamespace::new(SemaphoreLimits::default()).unwrap()),
            SemaphoreLimits::default(),
            Vec::new(),
        ));
        for index in 0..pipe_count {
            let key = index as u64 * 2 + 1;
            catalog
                .insert_pipe(
                    Arc::new(Pipe::new(true)),
                    IpcResourceKey::new(key).unwrap(),
                    IpcResourceKey::new(key + 1).unwrap(),
                    Arc::new(InstalledBinding),
                    Arc::new(InstalledBinding),
                )
                .unwrap();
        }
        (catalog, memory)
    }

    fn fixture(pipe_count: usize) -> (Arc<CheckpointIpcCatalog>, Arc<State>, IpcCheckpointParticipant) {
        let (catalog, memory) = Self::catalog(pipe_count);
        let handle = Arc::new(CheckpointIpcCatalog::new(catalog));
        let state = Arc::new(State::default());
        *state.memory.lock().unwrap() = Some(memory);
        let participant = IpcCheckpointParticipant::new(
            handle.clone(),
            Arc::new(Rebind(state.clone())),
            Arc::new(Codec::default()),
        );
        (handle, state, participant)
    }

    fn fixture_shared_undo() -> (Arc<CheckpointIpcCatalog>, Arc<State>, IpcCheckpointParticipant) {
        let memory = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
        let shared = Arc::new(SharedMemoryNamespace::new(memory.clone(), SharedMemoryLimits::default()).unwrap());
        let segment = shared
            .shmget(ShmGetRequest {
                key: IpcKey(7),
                size: 4096,
                create: true,
                exclusive: false,
                mode: 0o600,
                actor: Self::OWNER,
                pid: 100,
                now: 1,
            })
            .unwrap();
        let plan = shared.shmat_plan(segment, Self::OWNER, 0).unwrap();
        shared.commit_attach(plan, 100, 2).unwrap();
        let shared_image = shared.snapshot();
        let semaphores = Arc::new(SemaphoreNamespace::new(SemaphoreLimits::default()).unwrap());
        let set = semaphores
            .semget(SemGetRequest {
                key: IpcKey(8),
                semaphores: 1,
                create: true,
                exclusive: false,
                mode: 0o600,
                actor: Self::OWNER,
                pid: 200,
                now: 1,
            })
            .unwrap();
        semaphores.set_value(set, 0, 2, Self::OWNER, 200, 2).unwrap();
        semaphores
            .operate(
                set,
                Self::OWNER,
                200,
                &[SemaphoreOperation {
                    index: 0,
                    delta: -1,
                    flags: SEM_UNDO,
                }],
                3,
            )
            .unwrap();
        let catalog = Arc::new(IpcCatalog::new(
            shared,
            SharedMemoryLimits::default(),
            vec![SharedBackingCheckpoint {
                segment,
                object: shared_image.segments[0].backing,
                resource: SharedBackingKey::new(10).unwrap(),
            }],
            Arc::new(MessageQueueNamespace::new(MessageLimits::default()).unwrap()),
            MessageLimits::default(),
            semaphores,
            SemaphoreLimits::default(),
            vec![
                TaskCheckpoint {
                    process: 100,
                    resource: IpcResourceKey::new(11).unwrap(),
                },
                TaskCheckpoint {
                    process: 200,
                    resource: IpcResourceKey::new(12).unwrap(),
                },
            ],
        ));
        let handle = Arc::new(CheckpointIpcCatalog::new(catalog));
        let state = Arc::new(State::default());
        *state.memory.lock().unwrap() = Some(memory);
        let participant = IpcCheckpointParticipant::new(
            handle.clone(),
            Arc::new(Rebind(state.clone())),
            Arc::new(Codec::default()),
        );
        (handle, state, participant)
    }

    fn section(participant: &IpcCheckpointParticipant) -> Section {
        participant.freeze().unwrap();
        let section = Section::new(
            SectionKind::new(7).unwrap(),
            IPC_CHECKPOINT_VERSION,
            participant.snapshot().unwrap(),
        );
        participant.thaw().unwrap();
        section
    }

    fn assert_failure(failure: usize) {
        let (handle, state, participant) = Self::fixture(1);
        let previous = handle.current();
        state.failure.store(failure, Ordering::Relaxed);
        let staged = participant.stage(&Self::section(&participant));
        if let Ok(reservation) = staged {
            let _ = participant
                .commit(reservation)
                .and_then(|()| participant.resume(reservation));
            participant.rollback(reservation);
            participant.rollback(reservation);
            assert_eq!(state.rollbacks.load(Ordering::Relaxed), 1);
        }
        assert!(Arc::ptr_eq(&handle.current(), &previous));
    }
}

#[test]
fn ipc_restores_previous() {
    let (handle, state, participant) = IpcScenario::fixture(1);
    let previous = handle.current();
    let reservation = participant.stage(&IpcScenario::section(&participant)).unwrap();
    participant.commit(reservation).unwrap();
    participant.resume(reservation).unwrap();
    participant.rollback(reservation);
    assert!(Arc::ptr_eq(&handle.current(), &previous));
    assert_eq!(state.rollbacks.load(Ordering::Relaxed), 1);
    assert_eq!(state.bindings.load(Ordering::Relaxed), 2);
}

#[test]
fn partial_before_publication() {
    let (handle, state, participant) = IpcScenario::fixture(2);
    let previous = handle.current();
    state.failure.store(2, Ordering::Relaxed);
    assert!(participant.stage(&IpcScenario::section(&participant)).is_err());
    assert!(Arc::ptr_eq(&handle.current(), &previous));
    assert_eq!(state.descriptors.load(Ordering::Relaxed), 3);
    assert_eq!(state.rollbacks.load(Ordering::Relaxed), 1);
}

#[test]
fn stage_previous_catalog() {
    for failure in [1, 3, 4] {
        IpcScenario::assert_failure(failure);
    }
}

#[test]
fn finish_rollback_catalog() {
    let (handle, _, participant) = IpcScenario::fixture(1);
    let previous = handle.current();
    let weak = Arc::downgrade(&previous);
    let reservation = participant.stage(&IpcScenario::section(&participant)).unwrap();
    participant.commit(reservation).unwrap();
    participant.resume(reservation).unwrap();
    drop(previous);
    assert!(weak.upgrade().is_some());
    participant.finish(reservation);
    assert!(weak.upgrade().is_none());
}

#[test]
fn shared_failures_compensated() {
    for failure in [5, 6] {
        let (handle, state, participant) = IpcScenario::fixture_shared_undo();
        let previous = handle.current();
        state.failure.store(failure, Ordering::Relaxed);
        assert!(participant.stage(&IpcScenario::section(&participant)).is_err());
        assert!(Arc::ptr_eq(&handle.current(), &previous));
        assert_eq!(state.tasks.load(Ordering::Relaxed), 2);
        assert_eq!(state.rollbacks.load(Ordering::Relaxed), 1);
    }
}
