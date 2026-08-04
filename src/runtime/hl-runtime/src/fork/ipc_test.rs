use std::sync::Arc;

use hl_ipc::{
    IpcCatalog, MessageLimits, MessageQueueNamespace, SemaphoreLimits, SemaphoreNamespace, SharedMemoryLimits,
    SharedMemoryNamespace,
};
use hl_memory::{MappingCoordinator, SharedLimits, SharedObjectStore, TestMappingHost};
use hl_task::{ForkCloneFlags, ForkEntityId, ForkRequest};

use crate::{
    ForkArtifactExchange, ForkContext, ForkParticipant, ForkParticipantRole, IpcForkParticipant, MemoryChildMapping,
    MemoryMappings,
};

fn fixture() -> (
    IpcForkParticipant<TestMappingHost>,
    Arc<MappingCoordinator<TestMappingHost>>,
) {
    let store = Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap());
    let parent_coordinator = Arc::new(MappingCoordinator::with_shared(TestMappingHost, Arc::clone(&store)));
    let parent_mappings = Arc::new(MemoryMappings::new(Arc::clone(&parent_coordinator)));
    let shared_limits = SharedMemoryLimits::default();
    let shared = Arc::new(SharedMemoryNamespace::new(store, shared_limits).unwrap());
    let message_limits = MessageLimits::default();
    let semaphore_limits = SemaphoreLimits::default();
    let catalog = Arc::new(IpcCatalog::new(
        shared,
        shared_limits,
        Vec::new(),
        Arc::new(MessageQueueNamespace::new(message_limits).unwrap()),
        message_limits,
        Arc::new(SemaphoreNamespace::new(semaphore_limits).unwrap()),
        semaphore_limits,
        Vec::new(),
    ));
    (IpcForkParticipant::new(catalog, parent_mappings), parent_coordinator)
}

struct TestFixture;

impl TestFixture {
    fn context(transaction: u64) -> ForkContext {
        ForkContext {
            transaction,
            request: ForkRequest {
                parent: ForkEntityId {
                    slot: 10,
                    generation: 1,
                },
                child: ForkEntityId {
                    slot: 20,
                    generation: 1,
                },
                flags: ForkCloneFlags::default(),
            },
        }
    }
}

#[test]
fn actor_number() {
    assert_eq!(IpcForkParticipant::<TestMappingHost>::actor(0), Ok(1));
    assert_eq!(IpcForkParticipant::<TestMappingHost>::actor(u32::MAX), Err(()),);
}

#[test]
fn ipc_memory_artifact() {
    let (participant, parent) = fixture();
    let artifacts = ForkArtifactExchange::default();
    let current = TestFixture::context(7);
    let reservation = participant.prepare(current).unwrap();
    assert!(
        participant
            .clone_with_artifacts(current, reservation, &artifacts)
            .is_err()
    );
    let stale = TestFixture::context(6);
    artifacts
        .publish(
            stale,
            ForkParticipantRole::Memory,
            1,
            Arc::new(MemoryChildMapping(Arc::clone(&parent))),
        )
        .unwrap();
    assert!(
        participant
            .clone_with_artifacts(current, reservation, &artifacts)
            .is_err()
    );
    participant.rollback(current, reservation);
}

#[test]
fn ipc_rollback_removes() {
    let (participant, parent) = fixture();
    let artifacts = ForkArtifactExchange::default();
    let current = TestFixture::context(9);
    let reservation = participant.prepare(current).unwrap();
    let child = Arc::new(parent.fork_restore(TestMappingHost).unwrap());
    artifacts
        .publish(
            current,
            ForkParticipantRole::Memory,
            3,
            Arc::new(MemoryChildMapping(Arc::clone(&child))),
        )
        .unwrap();
    participant
        .clone_with_artifacts(current, reservation, &artifacts)
        .unwrap();
    assert!(participant.child(current.transaction).is_none());
    participant.commit(current, reservation).unwrap();
    let published = participant.child(current.transaction).unwrap();
    assert!(Arc::ptr_eq(&published.memory, &child));
    participant.rollback(current, reservation);
    assert!(participant.child(current.transaction).is_none());
}
