use std::sync::{Arc, Mutex};

use hl_ipc::{
    IpcCatalog, MessageLimits, MessageQueueNamespace, SemaphoreLimits, SemaphoreNamespace, SharedMemoryLimits,
    SharedMemoryNamespace,
};
use hl_isa::GuestArchitecture;
use hl_linux::{
    Errno, GuestAccess, GuestFault, GuestMemory, IPC_CREAT, IPC_INFO, IPC_SET, IPC_STAT, IpcSyscalls, LinuxResult,
    MSG_INFO, MSG_STAT, MSG_STAT_ANY, SyscallDispatcher, SyscallDisposition, SyscallOperation,
};
use hl_memory::{SharedLimits, SharedObjectStore};
use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, TaskRegistry};
use hl_time::{ClockError, MonotonicClock, MonotonicInstant, RealtimeClock, Timespec};

use super::RuntimeIpcSyscalls;

const BUFFER: usize = 128;

#[derive(Clone, Debug)]
struct Memory(Arc<Mutex<Vec<u8>>>);

impl Memory {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(vec![0; 1024])))
    }

    fn put(&self, address: usize, bytes: &[u8]) {
        self.0.lock().unwrap()[address..address + bytes.len()].copy_from_slice(bytes);
    }

    fn get(&self, address: usize, length: usize) -> Vec<u8> {
        self.0.lock().unwrap()[address..address + length].to_vec()
    }
}

impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        let end = usize::try_from(address)
            .ok()
            .and_then(|start| start.checked_add(length));
        if end.is_none_or(|end| end > self.0.lock().unwrap().len()) {
            Err(GuestFault { address, access })
        } else {
            Ok(length)
        }
    }

    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        let start = address as usize;
        let bytes = self.0.lock().unwrap();
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
        let start = address as usize;
        let mut bytes = self.0.lock().unwrap();
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
        Ok(Timespec::new(17, 0).unwrap())
    }
}

struct Fixture {
    runtime: (Arc<IpcCatalog>, Arc<TaskRegistry>, hl_task::ProcessId, Memory),
    message_limits: MessageLimits,
}

impl Fixture {
    fn new(uid: u32) -> Self {
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
            messages,
            message_limits,
            semaphores,
            semaphore_limits,
            Vec::new(),
        ));
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let credentials = ProcessCredentials::new(uid, uid, &[], 8).unwrap();
        let (process, _) = tasks.create_init(credentials, ProcessLimits::empty()).unwrap();
        Self {
            runtime: (catalog, tasks, process, Memory::new()),
            message_limits,
        }
    }

    fn call(&self, architecture: GuestArchitecture, name: &str, arguments: [u64; 6]) -> LinuxResult {
        let mut runtime = RuntimeIpcSyscalls::new(
            self.runtime.0.clone(),
            self.runtime.1.clone(),
            self.runtime.2,
            self.runtime.3.clone(),
            architecture,
            Arc::new(FixedClock),
        );
        runtime.handle(Self::operation(architecture, name), arguments)
    }

    fn operation(architecture: GuestArchitecture, name: &str) -> SyscallOperation {
        let raw = match (architecture, name) {
            (GuestArchitecture::Aarch64, "msgget") => 186,
            (GuestArchitecture::Aarch64, "msgctl") => 187,
            (GuestArchitecture::X86_64, "msgget") => 68,
            (GuestArchitecture::X86_64, "msgctl") => 71,
            _ => panic!("unknown IPC operation"),
        };
        let route = SyscallDispatcher::route(architecture, raw);
        let SyscallDisposition::Operation(operation) = route.disposition else {
            panic!("IPC operation did not route");
        };
        operation
    }

    fn create_queue(&self, architecture: GuestArchitecture, mode: u32) -> u64 {
        let LinuxResult::Value(id) = self.call(architecture, "msgget", [0, u64::from(IPC_CREAT | mode), 0, 0, 0, 0])
        else {
            panic!("queue creation failed");
        };
        id
    }

    fn set_record(&self, uid: u32, gid: u32, mode: u32, qbytes: u64) {
        let mut record = vec![0; 120];
        record[4..8].copy_from_slice(&uid.to_le_bytes());
        record[8..12].copy_from_slice(&gid.to_le_bytes());
        record[20..24].copy_from_slice(&mode.to_le_bytes());
        record[88..96].copy_from_slice(&qbytes.to_le_bytes());
        self.runtime.3.put(BUFFER, &record);
    }
}

fn architectures() -> [GuestArchitecture; 2] {
    [GuestArchitecture::Aarch64, GuestArchitecture::X86_64]
}

#[test]
fn message_transactional_isas() {
    for architecture in architectures() {
        let fixture = Fixture::new(1000);
        let id = fixture.create_queue(architecture, 0o600);
        assert_eq!(
            fixture.call(architecture, "msgctl", [id, u64::from(IPC_SET), 960, 0, 0, 0],),
            LinuxResult::Error(Errno::EFAULT),
        );
        assert_eq!(
            fixture.call(
                architecture,
                "msgctl",
                [id, u64::from(IPC_STAT), BUFFER as u64, 0, 0, 0],
            ),
            LinuxResult::Value(0),
        );
        let status = fixture.runtime.3.get(BUFFER, 120);
        assert_eq!(
            u64::from_le_bytes(status[88..96].try_into().unwrap()),
            fixture.message_limits.queue_bytes as u64,
        );
        assert_eq!(u32::from_le_bytes(status[20..24].try_into().unwrap()) & 0o777, 0o600);
    }
}

#[test]
fn owner_through_stat() {
    for architecture in architectures() {
        let owner = Fixture::new(1000);
        let id = owner.create_queue(architecture, 0o600);
        owner.set_record(1000, 1000, 0o640, 8192);
        assert_eq!(
            owner.call(architecture, "msgctl", [id, u64::from(IPC_SET), BUFFER as u64, 0, 0, 0],),
            LinuxResult::Value(0),
        );
        owner.set_record(1000, 1000, 0o600, 32768);
        assert_eq!(
            owner.call(architecture, "msgctl", [id, u64::from(IPC_SET), BUFFER as u64, 0, 0, 0],),
            LinuxResult::Error(Errno::EPERM),
        );

        let root = Fixture::new(0);
        let root_id = root.create_queue(architecture, 0o600);
        root.set_record(0, 0, 0o600, 32768);
        assert_eq!(
            root.call(
                architecture,
                "msgctl",
                [root_id, u64::from(IPC_SET), BUFFER as u64, 0, 0, 0],
            ),
            LinuxResult::Value(0),
        );
        assert_eq!(
            root.call(
                architecture,
                "msgctl",
                [root_id, u64::from(IPC_STAT), BUFFER as u64, 0, 0, 0],
            ),
            LinuxResult::Value(0),
        );
        let status = root.runtime.3.get(BUFFER, 120);
        assert_eq!(u64::from_le_bytes(status[88..96].try_into().unwrap()), 32768);
    }
}

#[test]
fn raw_public_id() {
    for architecture in architectures() {
        let fixture = Fixture::new(1000);
        let id = fixture.create_queue(architecture, 0);
        assert_eq!(
            fixture.call(architecture, "msgctl", [0, u64::from(MSG_STAT), BUFFER as u64, 0, 0, 0],),
            LinuxResult::Error(Errno::EACCES),
        );
        assert_eq!(
            fixture.call(
                architecture,
                "msgctl",
                [0, u64::from(MSG_STAT_ANY), BUFFER as u64, 0, 0, 0],
            ),
            LinuxResult::Value(id),
        );
        let status = fixture.runtime.3.get(BUFFER, 120);
        assert_eq!(u64::from_le_bytes(status[88..96].try_into().unwrap()), 16384);
    }
}

#[test]
fn message_usage_isas() {
    for architecture in architectures() {
        let fixture = Fixture::new(1000);
        fixture.create_queue(architecture, 0o600);
        assert_eq!(
            fixture.call(architecture, "msgctl", [0, u64::from(IPC_INFO), BUFFER as u64, 0, 0, 0],),
            LinuxResult::Value(0),
        );
        let configured = fixture.runtime.3.get(BUFFER, 32);
        assert_eq!(
            i32::from_le_bytes(configured[4..8].try_into().unwrap()),
            fixture.message_limits.message_bytes as i32,
        );
        assert_eq!(
            fixture.call(architecture, "msgctl", [0, u64::from(MSG_INFO), BUFFER as u64, 0, 0, 0],),
            LinuxResult::Value(0),
        );
        let usage = fixture.runtime.3.get(BUFFER, 32);
        assert_eq!(i32::from_le_bytes(usage[12..16].try_into().unwrap()), 1);
    }
}
