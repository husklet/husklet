use std::sync::Arc;

use crate::cpu::{CpuState, EXECUTION_SNAPSHOT_VERSION, ExecutionCpuSnapshot, ExecutionMachine, ExecutionSnapshot};
use hl_event::EventCatalog;
use hl_ipc::{
    IpcCatalog, MessageLimits, MessageQueueNamespace, SemaphoreLimits, SemaphoreNamespace, SharedMemoryLimits,
    SharedMemoryNamespace,
};
use hl_memory::{MappingCoordinator, SharedLimits, SharedObjectStore, TestMappingHost};
use hl_network::{NetworkCatalog, NetworkConfiguration};
use hl_provider::HandleNamespace;
use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};

use crate::{
    Control, ForkChildResourceCatalog, ForkChildResourceError, ForkChildResources, IpcForkChild, MemoryMappings,
};

struct Fixture;

impl Fixture {
    fn child() -> (hl_task::ProcessId, hl_task::ThreadId) {
        Self::children(1)[0]
    }

    fn children(count: usize) -> Vec<(hl_task::ProcessId, hl_task::ThreadId)> {
        let tasks = TaskRegistry::new(RegistryConfig::default()).unwrap();
        let credentials = ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
        let (_, source) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
        (0..count)
            .map(|_| {
                let plan = tasks.begin_fork_process(source).unwrap();
                tasks.commit_fork_process(plan).unwrap()
            })
            .collect()
    }

    fn resources(process: hl_task::ProcessId, thread: hl_task::ThreadId) -> ForkChildResources<TestMappingHost> {
        let (_, descriptors) = Control::new(8, 8).unwrap();
        let memory = Arc::new(MappingCoordinator::new(TestMappingHost));
        let shared_limits = SharedMemoryLimits::default();
        let message_limits = MessageLimits::default();
        let semaphore_limits = SemaphoreLimits::default();
        let shared = Arc::new(
            SharedMemoryNamespace::new(
                Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap()),
                shared_limits,
            )
            .unwrap(),
        );
        let ipc = Arc::new(IpcForkChild {
            catalog: Arc::new(IpcCatalog::new(
                shared,
                shared_limits,
                Vec::new(),
                Arc::new(MessageQueueNamespace::new(message_limits).unwrap()),
                message_limits,
                Arc::new(SemaphoreNamespace::new(semaphore_limits).unwrap()),
                semaphore_limits,
                Vec::new(),
            )),
            memory: Arc::clone(&memory),
            mappings: Arc::new(MemoryMappings::new(Arc::clone(&memory))),
        });
        ForkChildResources {
            process,
            thread,
            descriptors: Arc::new(descriptors),
            memory,
            providers: Arc::new(HandleNamespace::new(8).unwrap()),
            execution: Arc::new(
                ExecutionMachine::new(ExecutionSnapshot {
                    version: EXECUTION_SNAPSHOT_VERSION,
                    cpu: ExecutionCpuSnapshot::X86_64(CpuState::default()),
                    cache_epoch: 1,
                    fault: None,
                })
                .unwrap(),
            ),
            network: Arc::new(NetworkCatalog::new(
                NetworkConfiguration::new(Vec::new(), Vec::new(), Vec::new()).unwrap(),
            )),
            event: Arc::new(EventCatalog::new(8).unwrap()),
            ipc,
        }
    }
}

#[test]
fn reservation_release_capacity() {
    let catalog = ForkChildResourceCatalog::<TestMappingHost>::new(1).unwrap();
    let children = Fixture::children(2);
    let (process, thread) = children[0];
    let (other, other_thread) = children[1];
    let reservation = catalog.prepare(process).unwrap();
    assert_eq!(
        catalog.prepare(process).map(|_| ()),
        Err(ForkChildResourceError::Exists),
    );
    assert_eq!(
        catalog.prepare(other).map(|_| ()),
        Err(ForkChildResourceError::Capacity),
    );
    drop(reservation);
    assert_eq!(catalog.len(), 0);
    let reservation = catalog.prepare(process).unwrap();
    assert_eq!(
        reservation.publish(Fixture::resources(other, other_thread)),
        Err(ForkChildResourceError::Identity),
    );
    assert_eq!(catalog.len(), 0);
    catalog
        .prepare(process)
        .unwrap()
        .publish(Fixture::resources(process, thread))
        .unwrap();
    assert_eq!(catalog.len(), 1);
}

#[test]
fn published_generation_slot() {
    let catalog = ForkChildResourceCatalog::<TestMappingHost>::new(2).unwrap();
    let (process, thread) = Fixture::child();
    catalog
        .prepare(process)
        .unwrap()
        .publish(Fixture::resources(process, thread))
        .unwrap();
    assert_eq!(
        catalog.prepare(process).map(|_| ()),
        Err(ForkChildResourceError::Exists),
    );
    let published = catalog.take(process).unwrap();
    assert_eq!(published.process, process);
    assert_eq!(published.thread, thread);
    assert!(Arc::strong_count(&published.network) >= 1);
    assert!(Arc::strong_count(&published.event) >= 1);
    assert_eq!(catalog.len(), 0);
}
