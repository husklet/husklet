use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use hl_checkpoint::{Section, SectionKind};
use hl_linux::{FutexPlan, LinuxResult};
use hl_memory::{
    Backing, MEMORY_CHECKPOINT_VERSION, MapRequest, MappingCoordinator, MemoryCheckpointHost, MemoryCheckpointImage,
    MemoryError, MemoryHostRestore, MemoryHostStage, Placement, Protection, SharedLimits, SharedObjectStore,
    TestMappingHost,
};

use crate::{
    CheckpointMemory, CheckpointMemoryState, CheckpointParticipant, MemoryCheckpointCodec, MemoryCheckpointParticipant,
    PortableMemoryCodec, RuntimeFutexPort,
};

struct LiveFutex;

impl RuntimeFutexPort for LiveFutex {
    fn execute(&self, _: hl_task::ProcessId, _: hl_task::ThreadId, _: FutexPlan) -> LinuxResult {
        LinuxResult::Value(0)
    }

    fn checkpoint_quiescent(&self) -> bool {
        false
    }
}

#[derive(Default)]
struct Codec {
    next: AtomicU64,
    images: Mutex<BTreeMap<u64, MemoryCheckpointImage>>,
}

impl MemoryCheckpointCodec for Codec {
    fn encode(&self, image: &MemoryCheckpointImage) -> Result<Vec<u8>, ()> {
        let key = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        self.images.lock().map_err(|_| ())?.insert(key, image.clone());
        Ok(key.to_le_bytes().to_vec())
    }

    fn decode(&self, bytes: &[u8]) -> Result<MemoryCheckpointImage, ()> {
        let key = u64::from_le_bytes(bytes.try_into().map_err(|_| ())?);
        self.images.lock().map_err(|_| ())?.get(&key).cloned().ok_or(())
    }
}

#[derive(Default)]
struct HostState {
    stage_fail: AtomicBool,
    commit_fail: AtomicBool,
    resume_fail: AtomicBool,
    commits: AtomicUsize,
    rollbacks: AtomicUsize,
    resumes: AtomicUsize,
    replacement_owner: AtomicU64,
}

struct Host {
    state: Arc<HostState>,
}

struct CaptureHost;

impl MemoryCheckpointHost<TestMappingHost> for CaptureHost {
    fn address_limit(&self) -> u64 {
        65_536
    }
    fn snapshot_mapping(
        &self,
        _: &hl_memory::FrozenSnapshotAuthority,
        region: hl_memory::Region,
    ) -> Result<Vec<u8>, MemoryError> {
        let mut bytes = vec![0; region.range().length() as usize];
        bytes[3..7].copy_from_slice(b"rust");
        Ok(bytes)
    }

    fn stage(&self, image: &MemoryCheckpointImage) -> Result<MemoryHostStage<TestMappingHost>, MemoryError> {
        let shared = Arc::new(
            SharedObjectStore::restore(image.shared_limits, image.shared.clone()).map_err(MemoryError::Shared)?,
        );
        Ok(MemoryHostStage {
            mapping: TestMappingHost,
            shared,
            restore: Box::new(HostRebind {
                state: Arc::new(HostState::default()),
            }),
        })
    }
}

struct HostRebind {
    state: Arc<HostState>,
}

impl MemoryHostRestore<TestMappingHost> for HostRebind {
    fn commit(&mut self) -> Result<(), MemoryError> {
        self.state.commits.fetch_add(1, Ordering::Relaxed);
        if self.state.commit_fail.load(Ordering::Relaxed) {
            return Err(MemoryError::InvariantViolation);
        }
        Ok(())
    }

    fn rollback(&mut self) {
        self.state.rollbacks.fetch_add(1, Ordering::Relaxed);
    }

    fn resume(&mut self) -> Result<(), MemoryError> {
        self.state.resumes.fetch_add(1, Ordering::Relaxed);
        if self.state.resume_fail.load(Ordering::Relaxed) {
            return Err(MemoryError::InvariantViolation);
        }
        Ok(())
    }
}

impl MemoryCheckpointHost<TestMappingHost> for Host {
    fn address_limit(&self) -> u64 {
        65_536
    }
    fn snapshot_mapping(
        &self,
        _: &hl_memory::FrozenSnapshotAuthority,
        region: hl_memory::Region,
    ) -> Result<Vec<u8>, MemoryError> {
        Ok(vec![0; region.range().length() as usize])
    }

    fn stage(&self, image: &MemoryCheckpointImage) -> Result<MemoryHostStage<TestMappingHost>, MemoryError> {
        if self.state.stage_fail.load(Ordering::Relaxed) {
            return Err(MemoryError::InvariantViolation);
        }
        let mut snapshot = image.shared.clone();
        let owner = self.state.replacement_owner.load(Ordering::Relaxed);
        if owner != 0 {
            for object in &mut snapshot.objects {
                object.owner = owner;
            }
        }
        let shared = Arc::new(SharedObjectStore::restore(image.shared_limits, snapshot).map_err(MemoryError::Shared)?);
        Ok(MemoryHostStage {
            mapping: TestMappingHost,
            shared,
            restore: Box::new(HostRebind {
                state: self.state.clone(),
            }),
        })
    }
}

type MemoryParticipant = MemoryCheckpointParticipant<TestMappingHost>;

fn fixture() -> (
    Arc<CheckpointMemoryState<TestMappingHost>>,
    Arc<HostState>,
    Arc<Codec>,
    MemoryParticipant,
) {
    let shared = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let object = shared.create(1, 4096).unwrap();
    let coordinator = Arc::new(MappingCoordinator::with_shared(TestMappingHost, shared.clone()));
    coordinator.map(TestMappingHost::shared_request(object)).unwrap();
    let memory = Arc::new(CheckpointMemoryState::new(Arc::new(CheckpointMemory::new(
        coordinator,
        shared,
    ))));
    let state = Arc::new(HostState::default());
    let codec = Arc::new(Codec::default());
    let participant =
        MemoryCheckpointParticipant::new(memory.clone(), Arc::new(Host { state: state.clone() }), codec.clone());
    (memory, state, codec, participant)
}

#[test]
fn successful_after_finish() {
    let (memory, state, _, participant) = fixture();
    let previous = memory.current();
    let previous_weak = Arc::downgrade(&previous);
    participant.freeze().unwrap();
    let section = Section::new(
        SectionKind::new(3).unwrap(),
        MEMORY_CHECKPOINT_VERSION,
        participant.snapshot().unwrap(),
    );
    participant.thaw().unwrap();
    let reservation = participant.stage(&section).unwrap();
    participant.commit(reservation).unwrap();
    assert!(!Arc::ptr_eq(&memory.current(), &previous));
    participant.resume(reservation).unwrap();
    drop(previous);
    assert!(previous_weak.upgrade().is_some());
    participant.finish(reservation);
    assert!(previous_weak.upgrade().is_none());
    assert_eq!(state.commits.load(Ordering::Relaxed), 1);
    assert_eq!(state.resumes.load(Ordering::Relaxed), 1);
}

#[test]
fn host_owns_shared() {
    let (memory, state, _, participant) = fixture();
    state.replacement_owner.store(77, Ordering::Relaxed);
    participant.freeze().unwrap();
    let section = Section::new(
        SectionKind::new(3).unwrap(),
        MEMORY_CHECKPOINT_VERSION,
        participant.snapshot().unwrap(),
    );
    participant.thaw().unwrap();
    let reservation = participant.stage(&section).unwrap();
    participant.commit(reservation).unwrap();
    participant.resume(reservation).unwrap();
    assert!(
        memory
            .current()
            .shared
            .snapshot()
            .objects
            .iter()
            .all(|object| object.owner == 77)
    );
    participant.finish(reservation);
}

#[test]
fn live_memory_freezes() {
    let (_, _, _, participant) = fixture();
    let participant = participant.with_futex_quiescence(Arc::new(LiveFutex));
    assert_eq!(participant.freeze(), Err(()));
}

#[test]
fn host_previous_memory() {
    let (memory, state, _, participant) = fixture();
    let previous = memory.current();
    participant.freeze().unwrap();
    let section = Section::new(
        SectionKind::new(3).unwrap(),
        MEMORY_CHECKPOINT_VERSION,
        participant.snapshot().unwrap(),
    );
    participant.thaw().unwrap();
    state.stage_fail.store(true, Ordering::Relaxed);
    assert!(participant.stage(&section).is_err());
    assert!(Arc::ptr_eq(&memory.current(), &previous));
    previous.shared.create(2, 1).unwrap();
}

#[test]
fn host_commit_memory() {
    let (memory, state, _, participant) = fixture();
    let previous = memory.current();
    participant.freeze().unwrap();
    let section = Section::new(
        SectionKind::new(3).unwrap(),
        MEMORY_CHECKPOINT_VERSION,
        participant.snapshot().unwrap(),
    );
    participant.thaw().unwrap();
    state.commit_fail.store(true, Ordering::Relaxed);
    let reservation = participant.stage(&section).unwrap();
    assert!(participant.commit(reservation).is_err());
    participant.rollback(reservation);
    assert!(Arc::ptr_eq(&memory.current(), &previous));
    previous.shared.create(2, 1).unwrap();
}

#[test]
fn host_resume_memory() {
    let (memory, state, _, participant) = fixture();
    let previous = memory.current();
    participant.freeze().unwrap();
    let section = Section::new(
        SectionKind::new(3).unwrap(),
        MEMORY_CHECKPOINT_VERSION,
        participant.snapshot().unwrap(),
    );
    participant.thaw().unwrap();
    state.resume_fail.store(true, Ordering::Relaxed);
    let reservation = participant.stage(&section).unwrap();
    participant.commit(reservation).unwrap();
    assert!(participant.resume(reservation).is_err());
    participant.rollback(reservation);
    assert!(Arc::ptr_eq(&memory.current(), &previous));
    previous.shared.create(2, 1).unwrap();
}

#[test]
fn swap_failure_compensated() {
    let (memory, state, codec, first) = fixture();
    let second = MemoryCheckpointParticipant::new(memory.clone(), Arc::new(Host { state: state.clone() }), codec);
    let previous = memory.current();
    first.freeze().unwrap();
    let section = Section::new(
        SectionKind::new(3).unwrap(),
        MEMORY_CHECKPOINT_VERSION,
        first.snapshot().unwrap(),
    );
    first.thaw().unwrap();
    let first_reservation = first.stage(&section).unwrap();
    let second_reservation = second.stage(&section).unwrap();
    first.commit(first_reservation).unwrap();
    assert!(second.commit(second_reservation).is_err());
    second.rollback(second_reservation);
    first.resume(first_reservation).unwrap();
    first.rollback(first_reservation);
    assert!(Arc::ptr_eq(&memory.current(), &previous));
    assert!(state.rollbacks.load(Ordering::Relaxed) >= 2);
}

#[test]
fn private_codec_roundtrip() {
    let shared = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let coordinator = MappingCoordinator::with_shared(TestMappingHost, shared.clone());
    coordinator
        .map_charged(
            MapRequest {
                placement: Placement::Fixed(hl_isa::GuestAddress::new(0x4000)),
                length: 4096,
                alignment: 4096,
                protection: Protection::READ.union(Protection::WRITE),
                backing: Backing::Anonymous {
                    identity: 17,
                    shared: false,
                },
                backing_offset: 0,
            },
            17,
        )
        .unwrap();
    coordinator.freeze_checkpoint();
    shared.freeze_checkpoint();
    let image = coordinator.checkpoint_image(&CaptureHost).unwrap();
    shared.thaw_checkpoint();
    coordinator.thaw_checkpoint();

    let codec = PortableMemoryCodec;
    let bytes = codec.encode(&image).unwrap();
    assert_eq!(u32::from_le_bytes(bytes[4..8].try_into().unwrap()), 2);
    let decoded = codec.decode(&bytes).unwrap();
    assert_eq!(decoded, image);
    assert!(decoded.ledger.regions[0].reserved());
    assert_eq!(decoded.ledger.regions[0].charge().unwrap().length(), 17);
    assert_eq!(decoded.mappings[0].bytes.len(), 4096);
    assert_eq!(&decoded.mappings[0].bytes[3..7], b"rust");

    let mut corrupt = bytes.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    assert!(codec.decode(&corrupt).is_err());
    let mut version = bytes;
    version[4..8].copy_from_slice(&1_u32.to_le_bytes());
    assert!(codec.decode(&version).is_err());
}

#[test]
fn private_restore_atomic() {
    let shared = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let coordinator = Arc::new(MappingCoordinator::with_shared(TestMappingHost, shared.clone()));
    coordinator
        .map(MapRequest {
            placement: Placement::Fixed(hl_isa::GuestAddress::new(0x4000)),
            length: 4096,
            alignment: 4096,
            protection: Protection::READ,
            backing: Backing::Anonymous {
                identity: 17,
                shared: false,
            },
            backing_offset: 0,
        })
        .unwrap();
    let memory = Arc::new(CheckpointMemoryState::new(Arc::new(CheckpointMemory::new(
        coordinator,
        shared,
    ))));
    let participant =
        MemoryCheckpointParticipant::new(memory.clone(), Arc::new(CaptureHost), Arc::new(PortableMemoryCodec));
    let previous = memory.current();
    participant.freeze().unwrap();
    let bytes = participant.snapshot().unwrap();
    participant.thaw().unwrap();

    let mut corrupt = bytes.clone();
    *corrupt.last_mut().unwrap() ^= 1;
    let bad = Section::new(SectionKind::new(3).unwrap(), MEMORY_CHECKPOINT_VERSION, corrupt);
    assert!(participant.stage(&bad).is_err());
    assert!(Arc::ptr_eq(&memory.current(), &previous));
    previous.shared.create(9, 1).unwrap();

    let section = Section::new(SectionKind::new(3).unwrap(), MEMORY_CHECKPOINT_VERSION, bytes);
    let reservation = participant.stage(&section).unwrap();
    participant.commit(reservation).unwrap();
    assert!(!Arc::ptr_eq(&memory.current(), &previous));
    participant.resume(reservation).unwrap();
    participant.finish(reservation);
}
