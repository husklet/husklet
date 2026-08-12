use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use crate::cpu::{CpuState, EXECUTION_SNAPSHOT_VERSION, ExecutionCpuSnapshot, ExecutionMachine, ExecutionSnapshot};
use hl_event::EventCatalog;
use hl_ipc::{
    IpcCatalog, MessageLimits, MessageQueueNamespace, SemaphoreLimits, SemaphoreNamespace, SharedMemoryLimits,
    SharedMemoryNamespace,
};
use hl_linux::ClonePlan;
use hl_memory::{MappingCoordinator, SharedLimits, SharedObjectStore, TestMappingHost};
use hl_network::{NetworkCatalog, NetworkConfiguration};
use hl_provider::HandleNamespace;
use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};

use crate::{
    Control, ForkChildResourceCatalog, ForkChildResources, ForkContext, ForkParticipantRole, ForkPhase, ForkRuntime,
    ForkRuntimeDependencies, IpcForkChild, MemoryForkHost, MemoryMappings, PrivateFutexReset, RuntimeForkError,
    RuntimeForkPort,
};

struct Host {
    fail: AtomicBool,
}

impl MemoryForkHost<TestMappingHost> for Host {
    fn child_host(&self, _: ForkContext) -> Result<TestMappingHost, ()> {
        if self.fail.load(Ordering::Relaxed) {
            Err(())
        } else {
            Ok(TestMappingHost)
        }
    }
}

#[derive(Default)]
struct Reset(AtomicUsize);

impl PrivateFutexReset for Reset {
    fn reset_private_futexes(&self, _: ForkContext) -> Result<(), ()> {
        self.0.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

struct Fixture {
    runtime: ForkRuntime<TestMappingHost, Host, Reset>,
    resources: Arc<ForkChildResourceCatalog<TestMappingHost>>,
    subject: Subject,
    host: Arc<Host>,
    initial: Arc<ForkChildResources<TestMappingHost>>,
}

struct Subject {
    tasks: Arc<TaskRegistry>,
    process: hl_task::ProcessId,
    thread: hl_task::ThreadId,
}

impl Fixture {
    fn new(capacity: usize) -> Self {
        Self::try_new(capacity, false, false).unwrap()
    }

    fn try_new(capacity: usize, task_mismatch: bool, memory_mismatch: bool) -> Result<Self, RuntimeForkError> {
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let credentials = ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
        let (process, thread) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
        let (epoll, descriptors) = Control::new(64, 64).unwrap();
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
        let ipc_memory = if memory_mismatch {
            Arc::new(MappingCoordinator::new(TestMappingHost))
        } else {
            Arc::clone(&memory)
        };
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
            memory: Arc::clone(&ipc_memory),
            mappings: Arc::new(MemoryMappings::new(ipc_memory)),
        });
        let (resource_process, resource_thread) = if task_mismatch {
            let plan = tasks.begin_fork_process(thread).unwrap();
            let identity = (plan.process(), plan.thread());
            tasks.rollback_fork_process(plan).unwrap();
            identity
        } else {
            (process, thread)
        };
        let initial = Arc::new(ForkChildResources {
            process: resource_process,
            thread: resource_thread,
            descriptors: Arc::new(descriptors),
            memory,
            providers: Arc::new(HandleNamespace::new(64).unwrap()),
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
            event: Arc::new(EventCatalog::new(64).unwrap()),
            ipc,
        });
        let resources = Arc::new(ForkChildResourceCatalog::new(capacity).unwrap());
        let host = Arc::new(Host {
            fail: AtomicBool::new(false),
        });
        let runtime = ForkRuntime::new(ForkRuntimeDependencies {
            tasks: Arc::clone(&tasks),
            epoll: Arc::new(epoll),
            resources: Arc::clone(&resources),
            initial: Arc::clone(&initial),
            memory_host: Arc::clone(&host),
            futex_reset: Arc::new(Reset::default()),
        })?;
        Ok(Self {
            runtime,
            resources,
            subject: Subject { tasks, process, thread },
            host,
            initial,
        })
    }

    fn plan(flags: u64) -> ClonePlan {
        ClonePlan {
            flags,
            stack: 0,
            stack_size: 0,
            parent_tid: 0,
            child_tid: 0,
            tls: 0,
            exit_signal: 17,
            pidfd: 0,
            set_tid: 0,
            set_tid_count: 0,
            cgroup: 0,
        }
    }
}

#[test]
fn constructor_memory_identity() {
    assert!(matches!(
        Fixture::try_new(1, true, false),
        Err(RuntimeForkError::Invalid),
    ));
    assert!(matches!(
        Fixture::try_new(1, false, true),
        Err(RuntimeForkError::Invalid),
    ));
}

#[test]
fn aggregate_can_fork() {
    let fixture = Fixture::new(4);
    let first = fixture
        .runtime
        .fork(fixture.subject.process, fixture.subject.thread, Fixture::plan(0))
        .unwrap();
    let child = fixture.resources.child(first.process).unwrap();
    assert_eq!(child.thread, first.thread);
    assert!(Arc::ptr_eq(&child.network, &fixture.initial.network));
    assert!(Arc::ptr_eq(&child.event, &fixture.initial.event));
    assert!(Arc::ptr_eq(&child.ipc.catalog, &fixture.initial.ipc.catalog));
    let second = fixture
        .runtime
        .fork(first.process, first.thread, Fixture::plan(0))
        .unwrap();
    assert!(fixture.resources.child(second.process).is_some());
    assert_eq!(fixture.subject.tasks.snapshot().processes.len(), 3);
}

#[test]
fn unsupported_nothing_retry() {
    let fixture = Fixture::new(1);
    let before = fixture.subject.tasks.snapshot();
    assert_eq!(
        fixture
            .runtime
            .fork(fixture.subject.process, fixture.subject.thread, Fixture::plan(0x200),),
        Err(RuntimeForkError::Unsupported),
    );
    assert_eq!(fixture.subject.tasks.snapshot().processes, before.processes);
    assert_eq!(fixture.resources.len(), 0);
    fixture.host.fail.store(true, Ordering::Relaxed);
    assert_eq!(
        fixture
            .runtime
            .fork(fixture.subject.process, fixture.subject.thread, Fixture::plan(0),),
        Err(RuntimeForkError::Failed),
    );
    assert_eq!(fixture.subject.tasks.snapshot().processes, before.processes);
    assert_eq!(fixture.resources.len(), 0);
    fixture.host.fail.store(false, Ordering::Relaxed);
    let child = fixture
        .runtime
        .fork(fixture.subject.process, fixture.subject.thread, Fixture::plan(0))
        .unwrap();
    assert!(fixture.resources.child(child.process).is_some());
    assert_eq!(
        fixture
            .runtime
            .fork(fixture.subject.process, fixture.subject.thread, Fixture::plan(0),),
        Err(RuntimeForkError::Again),
    );
    assert_eq!(fixture.resources.len(), 1);
}

#[test]
fn real_cleans_retries() {
    let roles = [
        ForkParticipantRole::Task,
        ForkParticipantRole::Descriptors,
        ForkParticipantRole::Memory,
        ForkParticipantRole::Provider,
        ForkParticipantRole::Execution,
        ForkParticipantRole::Network,
        ForkParticipantRole::Event,
        ForkParticipantRole::Ipc,
    ];
    let phases = [
        ForkPhase::Prepare,
        ForkPhase::Freeze,
        ForkPhase::CloneParent,
        ForkPhase::CloneChild,
        ForkPhase::RepairParent,
        ForkPhase::RepairChild,
        ForkPhase::Commit,
    ];
    for role in roles {
        for phase in phases {
            let fixture = Fixture::new(1);
            let before = fixture.subject.tasks.snapshot().processes;
            fixture.runtime.inject_fault(role, phase);
            assert_eq!(
                fixture
                    .runtime
                    .fork(fixture.subject.process, fixture.subject.thread, Fixture::plan(0),),
                Err(RuntimeForkError::Failed),
                "{role:?} {phase:?}",
            );
            assert_eq!(fixture.subject.tasks.snapshot().processes, before);
            assert_eq!(fixture.resources.len(), 0);
            fixture.runtime.clear_fault();
            let child = fixture
                .runtime
                .fork(fixture.subject.process, fixture.subject.thread, Fixture::plan(0))
                .unwrap();
            assert!(fixture.resources.child(child.process).is_some());
        }
    }
}
