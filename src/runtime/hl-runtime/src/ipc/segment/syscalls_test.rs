use std::sync::{Arc, Mutex};

use hl_ipc::{
    AttachPlan, Credentials, IPC_PRIVATE, IpcCatalog, MessageLimits, MessageQueueNamespace, SemaphoreLimits,
    SemaphoreNamespace, SharedMemoryLimits, SharedMemoryNamespace, ShmGetRequest,
};
use hl_isa::{GuestAddress, GuestArchitecture};
use hl_linux::{
    Errno, GuestAccess, GuestFault, GuestMemory, IpcSyscalls, LinuxResult, SyscallDispatcher, SyscallDisposition,
    SyscallOperation,
};
use hl_memory::{SharedLimits, SharedObjectStore};
use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};
use hl_time::{ClockError, MonotonicClock, MonotonicInstant, RealtimeClock, Timespec};

use crate::{MappingError, MemoryBinding, MemoryLifecycle, MemoryPort, RuntimeIpcSyscalls};

#[derive(Clone, Copy, Debug)]
struct Memory;

impl GuestMemory for Memory {
    fn probe(&self, address: u64, _: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        Err(GuestFault { address, access })
    }

    fn read(&self, address: u64, _: &mut [u8]) -> Result<usize, GuestFault> {
        Err(GuestFault {
            address,
            access: GuestAccess::Read,
        })
    }

    fn write(&self, address: u64, _: &[u8]) -> Result<usize, GuestFault> {
        Err(GuestFault {
            address,
            access: GuestAccess::Write,
        })
    }
}

#[derive(Debug)]
struct FixedClock;

impl MonotonicClock for FixedClock {
    fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError> {
        Ok(MonotonicInstant::from_nanoseconds(1))
    }
}

impl RealtimeClock for FixedClock {
    fn realtime_now(&self) -> Result<Timespec, ClockError> {
        Ok(Timespec::new(7, 0).unwrap())
    }
}

#[derive(Debug)]
struct PortState {
    mapped: Option<AttachPlan>,
    attachment: Option<u64>,
    fail_rollback: bool,
    fail_unmap: bool,
    fail_restore: bool,
    rollbacks: usize,
}

#[derive(Debug)]
struct Port(Mutex<PortState>);

impl Port {
    fn new() -> Self {
        Self(Mutex::new(PortState {
            mapped: None,
            attachment: None,
            fail_rollback: false,
            fail_unmap: false,
            fail_restore: false,
            rollbacks: 0,
        }))
    }
}

impl MemoryPort for Port {
    fn map(&self, plan: AttachPlan, _: GuestAddress) -> Result<GuestAddress, MappingError> {
        self.0.lock().unwrap().mapped = Some(plan);
        Ok(GuestAddress::new(0x4000))
    }

    fn bind(&self, _: GuestAddress, attachment: u64) -> Result<(), MappingError> {
        self.0.lock().unwrap().attachment = Some(attachment);
        Ok(())
    }

    fn rollback(&self, _: GuestAddress) -> Result<(), MappingError> {
        let mut state = self.0.lock().unwrap();
        state.rollbacks += 1;
        if state.fail_rollback {
            Err(MappingError::Invariant)
        } else {
            state.mapped = None;
            Ok(())
        }
    }

    fn unmap(&self, _: GuestAddress) -> Result<u64, MappingError> {
        let mut state = self.0.lock().unwrap();
        if state.fail_unmap {
            return Err(MappingError::Invariant);
        }
        state.mapped = None;
        state.attachment.take().ok_or(MappingError::Invalid)
    }

    fn bindings(&self) -> Result<Vec<MemoryBinding>, MappingError> {
        let state = self.0.lock().unwrap();
        match (state.mapped, state.attachment) {
            (Some(plan), Some(attachment)) => Ok(vec![MemoryBinding {
                address: GuestAddress::new(0x4000),
                length: plan.backing.length,
                attachment,
            }]),
            (None, Some(attachment)) => Ok(vec![MemoryBinding {
                address: GuestAddress::new(0x4000),
                length: 4096,
                attachment,
            }]),
            (None, None) => Ok(Vec::new()),
            _ => Err(MappingError::Invariant),
        }
    }

    fn restore_bindings(&self, bindings: &[MemoryBinding]) -> Result<(), MappingError> {
        if self.0.lock().unwrap().fail_restore {
            return Err(MappingError::Invariant);
        }
        let [binding] = bindings else {
            return Err(MappingError::Invalid);
        };
        self.0.lock().unwrap().attachment = Some(binding.attachment);
        Ok(())
    }

    fn unmap_all(&self) -> Result<Vec<u64>, MappingError> {
        let mut state = self.0.lock().unwrap();
        if state.fail_unmap {
            return Err(MappingError::Invariant);
        }
        state.mapped = None;
        Ok(state.attachment.take().into_iter().collect())
    }
}

#[test]
fn lifecycle_domain_inheritance() {
    let fixture = Fixture::new(4);
    let id = fixture.segment(4096);
    assert_eq!(
        fixture.call(
            GuestArchitecture::X86_64,
            "shmat",
            [id.linux_id().unwrap() as u64, 0, 0, 0, 0, 0],
            true,
        ),
        LinuxResult::Value(0x4000),
    );
    let child = Arc::new(Port::new());
    child.0.lock().unwrap().fail_restore = true;
    let lifecycle = MemoryLifecycle::new(fixture.catalog.clone(), fixture.port.clone());
    assert_eq!(
        lifecycle.fork(fixture.process.number(), 200, 8, child.as_ref()),
        Err(MappingError::Invariant),
    );
    assert_eq!(fixture.shared.metadata(id).unwrap().attaches, 1);
}

struct Fixture {
    catalog: Arc<IpcCatalog>,
    shared: Arc<SharedMemoryNamespace>,
    tasks: Arc<TaskRegistry>,
    process: hl_task::ProcessId,
    port: Arc<Port>,
}

impl Fixture {
    fn new(attachments: usize) -> Self {
        let shared_limits = SharedMemoryLimits {
            attachments,
            ..SharedMemoryLimits::default()
        };
        let shared = Arc::new(
            SharedMemoryNamespace::new(
                Arc::new(SharedObjectStore::new(SharedLimits::default()).unwrap()),
                shared_limits,
            )
            .unwrap(),
        );
        let message_limits = MessageLimits::default();
        let semaphore_limits = SemaphoreLimits::default();
        let catalog = Arc::new(IpcCatalog::new(
            shared.clone(),
            shared_limits,
            Vec::new(),
            Arc::new(MessageQueueNamespace::new(message_limits).unwrap()),
            message_limits,
            Arc::new(SemaphoreNamespace::new(semaphore_limits).unwrap()),
            semaphore_limits,
            Vec::new(),
        ));
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let credentials = ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
        let (process, _) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
        Self {
            catalog,
            shared,
            tasks,
            process,
            port: Arc::new(Port::new()),
        }
    }

    fn segment(&self, size: usize) -> hl_ipc::SharedMemoryId {
        self.shared
            .shmget(ShmGetRequest {
                key: IPC_PRIVATE,
                size,
                create: true,
                exclusive: false,
                mode: 0o600,
                actor: Credentials { uid: 1000, gid: 1000 },
                pid: self.process.number(),
                now: 1,
            })
            .unwrap()
    }

    fn call(&self, architecture: GuestArchitecture, name: &str, arguments: [u64; 6], with_port: bool) -> LinuxResult {
        let runtime = RuntimeIpcSyscalls::new(
            self.catalog.clone(),
            self.tasks.clone(),
            self.process,
            Memory,
            architecture,
            Arc::new(FixedClock),
        );
        let mut runtime = if with_port {
            runtime.with_memory_port(self.port.clone())
        } else {
            runtime
        };
        runtime.handle(Self::operation(architecture, name), arguments)
    }

    fn operation(architecture: GuestArchitecture, name: &str) -> SyscallOperation {
        let raw = match (architecture, name) {
            (GuestArchitecture::Aarch64, "shmat") => 196,
            (GuestArchitecture::Aarch64, "shmdt") => 197,
            (GuestArchitecture::X86_64, "shmat") => 30,
            (GuestArchitecture::X86_64, "shmdt") => 67,
            _ => panic!("unknown SysV shared-memory operation"),
        };
        let route = SyscallDispatcher::route(architecture, raw);
        let SyscallDisposition::Operation(operation) = route.disposition else {
            panic!("operation did not route");
        };
        operation
    }
}

fn architectures() -> [GuestArchitecture; 2] {
    [GuestArchitecture::Aarch64, GuestArchitecture::X86_64]
}

#[test]
fn isas_exact_token() {
    for architecture in architectures() {
        let fixture = Fixture::new(4);
        let id = fixture.segment(1);
        assert_eq!(
            fixture.call(
                architecture,
                "shmat",
                [id.linux_id().unwrap() as u64, 0, 0, 0, 0, 0],
                true,
            ),
            LinuxResult::Value(0x4000),
        );
        let state = fixture.port.0.lock().unwrap();
        assert_eq!(state.mapped.unwrap().backing.length, 4096);
        drop(state);
        assert_eq!(fixture.shared.metadata(id).unwrap().size, 1);
        assert_eq!(fixture.shared.metadata(id).unwrap().attaches, 1);
        assert_eq!(
            fixture.call(architecture, "shmdt", [0x4000, 0, 0, 0, 0, 0], true,),
            LinuxResult::Value(0),
        );
        assert_eq!(fixture.shared.metadata(id).unwrap().attaches, 0);
    }
}

#[test]
fn absent_flag_validation() {
    for architecture in architectures() {
        let fixture = Fixture::new(4);
        let id = fixture.segment(4096);
        assert_eq!(
            fixture.call(architecture, "shmat", [i32::MAX as u64, 0, 0, 0, 0, 0], false),
            LinuxResult::Error(Errno::EINVAL),
        );
        assert_eq!(
            fixture.call(
                architecture,
                "shmat",
                [id.linux_id().unwrap() as u64, 0, 1, 0, 0, 0],
                false,
            ),
            LinuxResult::Error(Errno::EINVAL),
        );
        assert_eq!(
            fixture.call(
                architecture,
                "shmat",
                [id.linux_id().unwrap() as u64, 0, 0, 0, 0, 0],
                false,
            ),
            LinuxResult::Error(Errno::ENOSYS),
        );
    }
}

#[test]
fn commit_compensation_eio() {
    for architecture in architectures() {
        let fixture = Fixture::new(1);
        let occupied = fixture.segment(4096);
        let occupied_plan = fixture
            .shared
            .shmat_plan(occupied, Credentials { uid: 1000, gid: 1000 }, 0)
            .unwrap();
        fixture
            .shared
            .commit_attach(occupied_plan, fixture.process.number(), 2)
            .unwrap();
        let candidate = fixture.segment(4096);
        assert_eq!(
            fixture.call(
                architecture,
                "shmat",
                [candidate.linux_id().unwrap() as u64, 0, 0, 0, 0, 0],
                true,
            ),
            LinuxResult::Error(Errno::ENOSPC),
        );
        assert_eq!(fixture.port.0.lock().unwrap().rollbacks, 1);
        fixture.port.0.lock().unwrap().fail_rollback = true;
        assert_eq!(
            fixture.call(
                architecture,
                "shmat",
                [candidate.linux_id().unwrap() as u64, 0, 0, 0, 0, 0],
                true,
            ),
            LinuxResult::Error(Errno::EIO),
        );
    }
}

#[test]
fn unmap_attachment_accounting() {
    for architecture in architectures() {
        let fixture = Fixture::new(4);
        let id = fixture.segment(4096);
        assert_eq!(
            fixture.call(
                architecture,
                "shmat",
                [id.linux_id().unwrap() as u64, 0, 0, 0, 0, 0],
                true,
            ),
            LinuxResult::Value(0x4000),
        );
        fixture.port.0.lock().unwrap().fail_unmap = true;
        assert_eq!(
            fixture.call(architecture, "shmdt", [0x4000, 0, 0, 0, 0, 0], true,),
            LinuxResult::Error(Errno::EIO),
        );
        assert_eq!(fixture.shared.metadata(id).unwrap().attaches, 1);
    }
}

#[test]
fn lifecycle_detaches_parent() {
    let fixture = Fixture::new(4);
    let id = fixture.segment(4096);
    assert_eq!(
        fixture.call(
            GuestArchitecture::Aarch64,
            "shmat",
            [id.linux_id().unwrap() as u64, 0, 0, 0, 0, 0],
            true,
        ),
        LinuxResult::Value(0x4000),
    );
    let child = Arc::new(Port::new());
    let lifecycle = MemoryLifecycle::new(fixture.catalog.clone(), fixture.port.clone());
    lifecycle
        .fork(fixture.process.number(), 200, 8, child.as_ref())
        .unwrap();
    let parent_token = fixture.port.bindings().unwrap()[0].attachment;
    let child_token = child.bindings().unwrap()[0].attachment;
    assert_ne!(parent_token, child_token);
    assert_eq!(fixture.shared.metadata(id).unwrap().attaches, 2);
    lifecycle.detach_process(fixture.process.number(), 9).unwrap();
    assert_eq!(fixture.shared.metadata(id).unwrap().attaches, 1);
}
