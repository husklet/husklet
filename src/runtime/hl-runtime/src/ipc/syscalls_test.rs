use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use hl_descriptor::DescriptorTable;
use hl_ipc::{
    Credentials, IpcCatalog, IpcKey, MessageLimits, MessageQueueId, MessageQueueNamespace, MqLimits, MqNamespace,
    SemaphoreLimits, SemaphoreNamespace, SharedMemoryId, SharedMemoryLimits, SharedMemoryNamespace, ShmGetRequest,
};
use hl_isa::GuestArchitecture;
use hl_linux::{
    Errno, GuestAccess, GuestFault, GuestMemory, IPC_CREAT, IpcSyscalls, LinuxResult, MSG_NOWAIT, SyscallDispatcher,
    SyscallDisposition, SyscallOperation,
};
use hl_memory::{SharedLimits, SharedObjectStore};
use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};
use hl_time::{ClockError, MonotonicClock, MonotonicInstant, RealtimeClock, Timespec};

use super::RuntimeIpcSyscalls;

#[path = "blocking_test.rs"]
mod blocking_test;

#[derive(Clone, Debug)]
pub(super) struct Memory {
    bytes: Arc<Mutex<Vec<u8>>>,
    fail_write: Arc<AtomicBool>,
}

impl Memory {
    fn new() -> Self {
        Self {
            bytes: Arc::new(Mutex::new(vec![0; 512])),
            fail_write: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn put(&self, address: usize, bytes: &[u8]) {
        self.bytes.lock().unwrap()[address..address + bytes.len()].copy_from_slice(bytes);
    }

    fn get(&self, address: usize, length: usize) -> Vec<u8> {
        self.bytes.lock().unwrap()[address..address + length].to_vec()
    }
}

impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        let end = (address as usize).checked_add(length);
        if end.is_none_or(|end| end > self.bytes.lock().unwrap().len()) {
            Err(GuestFault { address, access })
        } else {
            Ok(length)
        }
    }

    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        let start = address as usize;
        let bytes = self.bytes.lock().unwrap();
        let Some(source) = bytes.get(start..start.saturating_add(output.len())) else {
            return Err(GuestFault {
                address,
                access: GuestAccess::Read,
            });
        };
        output.copy_from_slice(source);
        Ok(output.len())
    }

    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        if self.fail_write.load(Ordering::Acquire) {
            return Err(GuestFault {
                address,
                access: GuestAccess::Write,
            });
        }
        let start = address as usize;
        let mut bytes = self.bytes.lock().unwrap();
        let Some(destination) = bytes.get_mut(start..start.saturating_add(input.len())) else {
            return Err(GuestFault {
                address,
                access: GuestAccess::Write,
            });
        };
        destination.copy_from_slice(input);
        Ok(input.len())
    }
}

#[derive(Debug)]
struct FixedClock;

impl MonotonicClock for FixedClock {
    fn monotonic_now(&self) -> Result<MonotonicInstant, ClockError> {
        Ok(MonotonicInstant::from_nanoseconds(5))
    }
}

impl RealtimeClock for FixedClock {
    fn realtime_now(&self) -> Result<Timespec, ClockError> {
        Ok(Timespec::new(11, 0).unwrap())
    }
}

pub(super) struct Fixture {
    pub(super) runtime: (Arc<IpcCatalog>, Arc<TaskRegistry>, hl_task::ProcessId, Memory),
    pub(super) messages: Arc<MessageQueueNamespace>,
    pub(super) semaphores: Arc<SemaphoreNamespace>,
}

impl Fixture {
    pub(super) fn new() -> Self {
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
        let messages = Arc::new(MessageQueueNamespace::new(message_limits).unwrap());
        let semaphores = Arc::new(SemaphoreNamespace::new(semaphore_limits).unwrap());
        let catalog = Arc::new(IpcCatalog::new(
            shared,
            shared_limits,
            Vec::new(),
            messages.clone(),
            message_limits,
            semaphores.clone(),
            semaphore_limits,
            Vec::new(),
        ));
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let credentials = ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
        let (process, _) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
        Self {
            runtime: (catalog, tasks, process, Memory::new()),
            messages,
            semaphores,
        }
    }

    pub(super) fn runtime(&self, architecture: GuestArchitecture) -> RuntimeIpcSyscalls<Memory> {
        RuntimeIpcSyscalls::new(
            self.runtime.0.clone(),
            self.runtime.1.clone(),
            self.runtime.2,
            self.runtime.3.clone(),
            architecture,
            Arc::new(FixedClock),
        )
    }

    pub(super) fn call(&self, architecture: GuestArchitecture, name: &str, arguments: [u64; 6]) -> LinuxResult {
        let mut runtime = self.runtime(architecture);
        runtime.handle(Self::operation(architecture, name), arguments)
    }

    pub(super) fn operation(architecture: GuestArchitecture, name: &str) -> SyscallOperation {
        let raw = match (architecture, name) {
            (GuestArchitecture::Aarch64, "msgget") => 186,
            (GuestArchitecture::Aarch64, "msgctl") => 187,
            (GuestArchitecture::Aarch64, "msgrcv") => 188,
            (GuestArchitecture::Aarch64, "msgsnd") => 189,
            (GuestArchitecture::Aarch64, "semget") => 190,
            (GuestArchitecture::Aarch64, "semctl") => 191,
            (GuestArchitecture::Aarch64, "semtimedop") => 192,
            (GuestArchitecture::Aarch64, "semop") => 193,
            (GuestArchitecture::Aarch64, "shmget") => 194,
            (GuestArchitecture::Aarch64, "mq_open") => 180,
            (GuestArchitecture::Aarch64, "mq_unlink") => 181,
            (GuestArchitecture::Aarch64, "shmctl") => 195,
            (GuestArchitecture::Aarch64, "shmat") => 196,
            (GuestArchitecture::X86_64, "msgget") => 68,
            (GuestArchitecture::X86_64, "msgctl") => 71,
            (GuestArchitecture::X86_64, "msgrcv") => 70,
            (GuestArchitecture::X86_64, "msgsnd") => 69,
            (GuestArchitecture::X86_64, "semget") => 64,
            (GuestArchitecture::X86_64, "semctl") => 66,
            (GuestArchitecture::X86_64, "semop") => 65,
            (GuestArchitecture::X86_64, "semtimedop") => 220,
            (GuestArchitecture::X86_64, "shmget") => 29,
            (GuestArchitecture::X86_64, "shmctl") => 31,
            (GuestArchitecture::X86_64, "shmat") => 30,
            (GuestArchitecture::X86_64, "mq_open") => 240,
            (GuestArchitecture::X86_64, "mq_unlink") => 241,
            _ => panic!("unknown fixture operation"),
        };
        let route = SyscallDispatcher::route(architecture, raw);
        let SyscallDisposition::Operation(operation) = route.disposition else {
            panic!("IPC operation did not route");
        };
        assert_eq!(operation.name, name);
        operation
    }
}

#[test]
fn posix_name_lifetime() {
    for architecture in architectures() {
        let fixture = Fixture::new();
        fixture.runtime.3.put(8, b"queue\0");
        let namespace = Arc::new(MqNamespace::new(MqLimits::default()));
        let descriptors = Arc::new(DescriptorTable::new(8).unwrap());
        let mut runtime = fixture
            .runtime(architecture)
            .with_posix_queues(namespace, Arc::clone(&descriptors));
        let open = Fixture::operation(architecture, "mq_open");
        let unlink = Fixture::operation(architecture, "mq_unlink");
        assert_eq!(runtime.handle(open, [8, 0x42, 0o600, 0, 0, 0]), LinuxResult::Value(0));
        assert_eq!(runtime.handle(unlink, [8, 0, 0, 0, 0, 0]), LinuxResult::Value(0));
        assert_eq!(
            runtime.handle(open, [8, 2, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::ENOENT)
        );
        assert_eq!(runtime.handle(open, [8, 0x42, 0o600, 0, 0, 0]), LinuxResult::Value(1));
        descriptors.close(0).unwrap();
        descriptors.close(1).unwrap();
    }
}

#[test]
fn existing_ignores_attr() {
    let fixture = Fixture::new();
    fixture.runtime.3.put(8, b"existing\0");
    let namespace = Arc::new(MqNamespace::new(MqLimits::default()));
    let descriptors = Arc::new(DescriptorTable::new(8).unwrap());
    let mut runtime = fixture
        .runtime(GuestArchitecture::Aarch64)
        .with_posix_queues(namespace, descriptors);
    let open = Fixture::operation(GuestArchitecture::Aarch64, "mq_open");
    assert_eq!(runtime.handle(open, [8, 0x42, 0, 0, 0, 0]), LinuxResult::Value(0));
    assert_eq!(
        runtime.handle(open, [8, 0x42, 0, u64::MAX, 0, 0]),
        LinuxResult::Value(1)
    );
}

#[test]
fn open_transaction() {
    let fixture = Fixture::new();
    fixture.runtime.3.put(8, b"first\0second\0");
    let namespace = Arc::new(MqNamespace::new(MqLimits::default()));
    let descriptors = Arc::new(DescriptorTable::new(1).unwrap());
    let mut runtime = fixture
        .runtime(GuestArchitecture::Aarch64)
        .with_posix_queues(namespace, descriptors);
    let open = Fixture::operation(GuestArchitecture::Aarch64, "mq_open");
    let unlink = Fixture::operation(GuestArchitecture::Aarch64, "mq_unlink");
    assert_eq!(runtime.handle(open, [8, 0x42, 0, 0, 0, 0]), LinuxResult::Value(0));
    assert_eq!(
        runtime.handle(open, [14, 0x42, 0, 0, 0, 0]),
        LinuxResult::Error(Errno::EMFILE)
    );
    assert_eq!(
        runtime.handle(unlink, [14, 0, 0, 0, 0, 0]),
        LinuxResult::Error(Errno::ENOENT)
    );
}

fn architectures() -> [GuestArchitecture; 2] {
    [GuestArchitecture::Aarch64, GuestArchitecture::X86_64]
}

#[test]
fn isa_sysv_objects() {
    for architecture in architectures() {
        let fixture = Fixture::new();
        assert!(matches!(
            fixture.call(architecture, "shmget", [0, 4096, u64::from(IPC_CREAT | 0o600), 0, 0, 0],),
            LinuxResult::Value(_),
        ));
        assert!(matches!(
            fixture.call(architecture, "msgget", [0, u64::from(IPC_CREAT | 0o600), 0, 0, 0, 0],),
            LinuxResult::Value(_),
        ));
        assert!(matches!(
            fixture.call(architecture, "semget", [0, 1, u64::from(IPC_CREAT | 0o600), 0, 0, 0],),
            LinuxResult::Value(_),
        ));
    }
}

#[test]
fn failed_either_isa() {
    for architecture in architectures() {
        let fixture = Fixture::new();
        let LinuxResult::Value(id) =
            fixture.call(architecture, "msgget", [0, u64::from(IPC_CREAT | 0o600), 0, 0, 0, 0])
        else {
            panic!("message queue creation failed");
        };
        fixture.runtime.3.put(32, &7_i64.to_le_bytes());
        fixture.runtime.3.put(40, b"rust");
        assert_eq!(
            fixture.call(architecture, "msgsnd", [id, 32, 4, 0, 0, 0]),
            LinuxResult::Value(0),
        );

        fixture.runtime.3.fail_write.store(true, Ordering::Release);
        assert_eq!(
            fixture.call(architecture, "msgrcv", [id, 96, 4, 0, u64::from(MSG_NOWAIT), 0],),
            LinuxResult::Error(Errno::EFAULT),
        );
        fixture.runtime.3.fail_write.store(false, Ordering::Release);
        assert_eq!(
            fixture.call(architecture, "msgrcv", [id, 96, 4, 0, u64::from(MSG_NOWAIT), 0],),
            LinuxResult::Value(4),
        );
        assert_eq!(fixture.runtime.3.get(96, 8), 7_i64.to_le_bytes());
        assert_eq!(fixture.runtime.3.get(104, 4), b"rust");
    }
}

#[test]
fn removed_public_identifier() {
    for architecture in architectures() {
        let fixture = Fixture::new();
        let LinuxResult::Value(stale) =
            fixture.call(architecture, "msgget", [0, u64::from(IPC_CREAT | 0o600), 0, 0, 0, 0])
        else {
            panic!("message queue creation failed");
        };
        let id = MessageQueueId::from_linux_id(stale as i32).unwrap();
        fixture
            .messages
            .remove(id, Credentials { uid: 1000, gid: 1000 }, fixture.runtime.2.number(), 12)
            .unwrap();
        let LinuxResult::Value(reused) =
            fixture.call(architecture, "msgget", [0, u64::from(IPC_CREAT | 0o600), 0, 0, 0, 0])
        else {
            panic!("message queue recreation failed");
        };
        assert_ne!(stale, reused);
        fixture.runtime.3.put(32, &1_i64.to_le_bytes());
        assert_eq!(
            fixture.call(architecture, "msgsnd", [stale, 32, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EINVAL),
        );
    }
}

#[test]
fn invalid_abi_honestly() {
    for architecture in architectures() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture.call(architecture, "msgctl", [0, 0x7fff, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EINVAL),
        );
        assert_eq!(
            fixture.call(architecture, "shmctl", [0, 3, 128, 0, 0, 0]),
            LinuxResult::Value(0),
        );
        assert_eq!(
            fixture.call(architecture, "shmat", [0, 0, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EINVAL),
        );
    }
}

#[test]
fn shm_lock_unlock_public_matrix() {
    for architecture in architectures() {
        let fixture = Fixture::new();
        let LinuxResult::Value(identifier) =
            fixture.call(architecture, "shmget", [0, 4096, u64::from(IPC_CREAT | 0o600), 0, 0, 0])
        else {
            panic!("shared-memory creation failed");
        };
        let before = fixture.runtime.0.with_shared_memory(|namespace| namespace.snapshot());
        for (command, ignored_buffer) in [(11, u64::MAX), (12, 1)] {
            assert_eq!(
                fixture.call(architecture, "shmctl", [identifier, command, ignored_buffer, 0, 0, 0]),
                LinuxResult::Value(0)
            );
            assert_eq!(
                fixture.runtime.0.with_shared_memory(|namespace| namespace.snapshot()),
                before
            );
        }
        assert_eq!(
            fixture.call(architecture, "shmctl", [u64::MAX, 11, u64::MAX, 0, 0, 0]),
            LinuxResult::Error(Errno::EINVAL)
        );

        let id = SharedMemoryId::from_linux_id(identifier as i32).unwrap();
        let attachment = fixture.runtime.0.with_shared_memory(|namespace| {
            let plan = namespace
                .shmat_plan(id, Credentials { uid: 1000, gid: 1000 }, 0)
                .unwrap();
            let attachment = namespace.commit_attach(plan, fixture.runtime.2.number(), 2).unwrap();
            namespace
                .remove(id, Credentials { uid: 1000, gid: 1000 }, fixture.runtime.2.number(), 3)
                .unwrap();
            attachment
        });
        assert_eq!(
            fixture.call(architecture, "shmctl", [identifier, 12, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EINVAL)
        );
        fixture
            .runtime
            .0
            .with_shared_memory(|namespace| namespace.shmdt(attachment, fixture.runtime.2.number(), 4).unwrap());
        let replacement = fixture.runtime.0.with_shared_memory(|namespace| {
            namespace
                .shmget(ShmGetRequest {
                    key: IpcKey(90),
                    size: 4096,
                    create: true,
                    exclusive: false,
                    mode: 0o600,
                    actor: Credentials { uid: 1000, gid: 1000 },
                    pid: fixture.runtime.2.number(),
                    now: 5,
                })
                .unwrap()
        });
        assert_ne!(replacement.generation, id.generation);
        assert_eq!(
            fixture.call(architecture, "shmctl", [identifier, 11, 0, 0, 0, 0]),
            LinuxResult::Error(Errno::EINVAL)
        );

        let unrelated = fixture.runtime.0.with_shared_memory(|namespace| {
            namespace
                .shmget(ShmGetRequest {
                    key: IpcKey(91),
                    size: 4096,
                    create: true,
                    exclusive: false,
                    mode: 0o777,
                    actor: Credentials { uid: 2000, gid: 2000 },
                    pid: 22,
                    now: 1,
                })
                .unwrap()
        });
        assert_eq!(
            fixture.call(
                architecture,
                "shmctl",
                [unrelated.linux_id().unwrap() as u64, 12, 0, 0, 0, 0]
            ),
            LinuxResult::Error(Errno::EPERM)
        );
    }
}
