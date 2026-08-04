use std::sync::Arc;

use hl_ipc::{Credentials, IPC_PRIVATE, IpcKey, SEM_UNDO, SemGetRequest, SemaphoreOperation, ShmGetRequest};
use hl_isa::GuestAddress;
use hl_memory::{MappingCoordinator, TestMappingHost};
use hl_task::{ProcessCredentials, ProcessId, ProcessLimits, RegistryConfig, TaskRegistry, ThreadId};

use crate::{MemoryMappings, MemoryPort};

const OWNER: Credentials = Credentials { uid: 7, gid: 8 };

pub(crate) struct ProcessFixture {
    pub(crate) ipc: super::lifecycle_test::Fixture,
    pub(crate) mappings: Arc<MemoryMappings<TestMappingHost>>,
    pub(crate) process: ProcessId,
    pub(crate) thread: ThreadId,
    pub(crate) tasks: Arc<TaskRegistry>,
    pub(crate) tokens: Vec<u64>,
    pub(crate) semaphore: hl_ipc::SemaphoreId,
}

impl ProcessFixture {
    pub(crate) fn new() -> Self {
        let ipc = super::lifecycle_test::Fixture::new();
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let credentials = ProcessCredentials::new(7, 8, &[], 8).unwrap();
        let (process, thread) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
        let coordinator = Arc::new(MappingCoordinator::with_shared(TestMappingHost, ipc.memory.clone()));
        let mappings = Arc::new(MemoryMappings::new(coordinator));
        let mut tokens = Vec::new();
        for (index, size) in [4096, 8192].into_iter().enumerate() {
            let id = ipc
                .shared
                .shmget(ShmGetRequest {
                    key: IPC_PRIVATE,
                    size,
                    create: true,
                    exclusive: false,
                    mode: 0o600,
                    actor: OWNER,
                    pid: process.number(),
                    now: 1,
                })
                .unwrap();
            let plan = ipc.shared.shmat_plan(id, OWNER, 0).unwrap();
            let address = mappings
                .map(plan, GuestAddress::new(0x4000 + index as u64 * 0x4000))
                .unwrap();
            let token = ipc.shared.commit_attach(plan, process.number(), 2).unwrap();
            mappings.bind(address, token).unwrap();
            tokens.push(token);
        }
        let semaphore = ipc
            .semaphores
            .semget(SemGetRequest {
                key: IpcKey(3),
                semaphores: 2,
                create: true,
                exclusive: false,
                mode: 0o600,
                actor: OWNER,
                pid: process.number(),
                now: 1,
            })
            .unwrap();
        ipc.semaphores
            .operate(
                semaphore,
                OWNER,
                process.number(),
                &[
                    SemaphoreOperation {
                        index: 0,
                        delta: 1,
                        flags: SEM_UNDO,
                    },
                    SemaphoreOperation {
                        index: 1,
                        delta: 2,
                        flags: SEM_UNDO,
                    },
                ],
                2,
            )
            .unwrap();
        Self {
            ipc,
            mappings,
            process,
            thread,
            tasks,
            tokens,
            semaphore,
        }
    }
}
