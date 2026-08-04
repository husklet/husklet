use super::*;
use hl_event::EpollInterest;
use hl_linux::{GuestAccess, GuestFault, SyscallFamily};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

struct Memory;
struct Buffer(Mutex<Vec<u8>>);
struct FaultBuffer {
    bytes: Buffer,
    fault_write: AtomicBool,
}
impl GuestMemory for Memory {
    fn probe(&self, address: u64, _length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        Err(GuestFault { address, access })
    }
    fn read(&self, address: u64, _output: &mut [u8]) -> Result<usize, GuestFault> {
        Err(GuestFault {
            address,
            access: GuestAccess::Read,
        })
    }
    fn write(&self, address: u64, _input: &[u8]) -> Result<usize, GuestFault> {
        Err(GuestFault {
            address,
            access: GuestAccess::Write,
        })
    }
}
impl GuestMemory for Buffer {
    fn probe(&self, address: u64, length: usize, _access: GuestAccess) -> Result<usize, GuestFault> {
        Ok(self
            .0
            .lock()
            .unwrap()
            .len()
            .saturating_sub(address as usize)
            .min(length))
    }
    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        let bytes = self.0.lock().unwrap();
        output.copy_from_slice(&bytes[address as usize..address as usize + output.len()]);
        Ok(output.len())
    }
    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        let mut bytes = self.0.lock().unwrap();
        bytes[address as usize..address as usize + input.len()].copy_from_slice(input);
        Ok(input.len())
    }
}
impl GuestMemory for FaultBuffer {
    fn probe(&self, address: u64, length: usize, access: GuestAccess) -> Result<usize, GuestFault> {
        self.bytes.probe(address, length, access)
    }
    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        self.bytes.read(address, output)
    }
    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        if self.fault_write.load(Ordering::Acquire) {
            return Err(GuestFault {
                address,
                access: GuestAccess::Write,
            });
        }
        self.bytes.write(address, input)
    }
}

impl Memory {
    fn operation(name: &'static str) -> SyscallOperation {
        SyscallOperation {
            canonical_number: 0,
            name,
            family: SyscallFamily::Event,
        }
    }
}

#[test]
fn eventfd_catalog_slot() {
    let table = Arc::new(DescriptorTable::new(4).unwrap());
    let catalog = Arc::new(EventCatalog::new(1).unwrap());
    let mut runtime = RuntimeEventSyscalls::new(table.clone(), catalog, Memory, GuestArchitecture::Aarch64);
    let result = runtime.handle(Memory::operation("eventfd2"), [7, 1 | 0x800 | 0x80000, 0, 0, 0, 0]);
    let LinuxResult::Value(fd) = result else {
        panic!("{result:?}")
    };
    assert!(table.flags(fd as i32).unwrap().closes_on_exec());
    assert_ne!(
        table.pin(fd as i32).unwrap().status().bits() & StatusFlags::NONBLOCKING,
        0,
    );
    assert_eq!(
        table.pin(fd as i32).unwrap().status().bits() & StatusFlags::ACCESS_MODE_MASK,
        2,
    );
    let lease = table.pin(fd as i32).unwrap();
    let context = hl_descriptor::OperationContext {
        actor: Some(hl_descriptor::OperationActor {
            process: 3,
            process_generation: 1,
            thread: 5,
            thread_generation: 1,
        }),
        cancellation: None,
    };
    let mut count = [0_u8; 8];
    assert_eq!(lease.read_context(&mut count, context), Ok(8));
    assert_eq!(u64::from_ne_bytes(count), 1);
    assert_eq!(lease.write_context(&2_u64.to_ne_bytes(), context), Ok(8));
    drop(lease);
    table.close(fd as i32).unwrap();
    assert_eq!(
        runtime.handle(Memory::operation("eventfd2"), [1, 0, 0, 0, 0, 0]),
        LinuxResult::Value(fd),
    );
}

#[test]
fn invalid_catalog_object() {
    let table = Arc::new(DescriptorTable::new(4).unwrap());
    let catalog = Arc::new(EventCatalog::new(1).unwrap());
    let mut runtime = RuntimeEventSyscalls::new(table.clone(), catalog, Memory, GuestArchitecture::X86_64);
    assert_eq!(
        runtime.handle(Memory::operation("eventfd2"), [0, u32::MAX as u64, 0, 0, 0, 0]),
        LinuxResult::Error(Errno::EINVAL),
    );
    assert_eq!(table.pin(0).unwrap_err(), hl_descriptor::DescriptorError::BadDescriptor);
    assert_eq!(
        runtime.handle(Memory::operation("epoll_create1"), [0, 0, 0, 0, 0, 0]),
        LinuxResult::Value(0),
    );
}

#[test]
fn legacy_epoll_create_validates_size() {
    let table = Arc::new(DescriptorTable::new(4).unwrap());
    let catalog = Arc::new(EventCatalog::new(4).unwrap());
    let mut runtime = RuntimeEventSyscalls::new(table, catalog, Memory, GuestArchitecture::X86_64);
    assert_eq!(
        runtime.handle(Memory::operation("epoll_create"), [0; 6]),
        LinuxResult::Error(Errno::EINVAL),
    );
    assert_eq!(
        runtime.handle(Memory::operation("epoll_create"), [u64::MAX, 0, 0, 0, 0, 0]),
        LinuxResult::Error(Errno::EINVAL),
    );
    assert!(matches!(
        runtime.handle(Memory::operation("epoll_create"), [1, 0, 0, 0, 0, 0]),
        LinuxResult::Value(_)
    ));
}

#[test]
fn epoll_event_bytes() {
    let table = Arc::new(DescriptorTable::new(8).unwrap());
    let (control, runtime_table) = Control::attach(table.clone(), 8, 32).unwrap();
    let mut bytes = vec![0; 128];
    bytes[0..4].copy_from_slice(&1_u32.to_le_bytes());
    bytes[8..16].copy_from_slice(&9_u64.to_le_bytes());
    let mut runtime = RuntimeEventSyscalls::new(
        table.clone(),
        Arc::new(EventCatalog::new(8).unwrap()),
        Buffer(Mutex::new(bytes)),
        GuestArchitecture::Aarch64,
    )
    .with_epoll_control(Arc::new(control), Arc::new(runtime_table));
    let LinuxResult::Value(epoll) = runtime.handle(Memory::operation("epoll_create1"), [0; 6]) else {
        panic!()
    };
    let LinuxResult::Value(eventfd) = runtime.handle(Memory::operation("eventfd2"), [0; 6]) else {
        panic!()
    };
    assert_eq!(
        runtime.handle(Memory::operation("epoll_ctl"), [epoll, 1, eventfd, 0, 0, 0],),
        LinuxResult::Value(0),
    );
    table.pin(eventfd as i32).unwrap().write(&1_u64.to_le_bytes()).unwrap();
    assert_eq!(
        runtime.handle(Memory::operation("epoll_wait"), [epoll, 32, 4, 0, 0, 0],),
        LinuxResult::Value(1),
    );
    let bytes = runtime.memory.0.lock().unwrap();
    assert_eq!(u32::from_le_bytes(bytes[32..36].try_into().unwrap()), 1);
    assert_eq!(u64::from_le_bytes(bytes[40..48].try_into().unwrap()), 9);
}

#[test]
fn epoll_oneshot_readiness() {
    let table = Arc::new(DescriptorTable::new(8).unwrap());
    let (control, runtime_table) = Control::attach(table.clone(), 8, 32).unwrap();
    let mut bytes = vec![0; 128];
    bytes[0..4].copy_from_slice(&(1_u32 | EpollInterest::ONESHOT).to_le_bytes());
    bytes[8..16].copy_from_slice(&77_u64.to_le_bytes());
    let memory = FaultBuffer {
        bytes: Buffer(Mutex::new(bytes)),
        fault_write: AtomicBool::new(true),
    };
    let mut runtime = RuntimeEventSyscalls::new(
        table.clone(),
        Arc::new(EventCatalog::new(8).unwrap()),
        memory,
        GuestArchitecture::Aarch64,
    )
    .with_epoll_control(Arc::new(control), Arc::new(runtime_table));
    let LinuxResult::Value(epoll) = runtime.handle(Memory::operation("epoll_create1"), [0; 6]) else {
        panic!()
    };
    let LinuxResult::Value(eventfd) = runtime.handle(Memory::operation("eventfd2"), [0; 6]) else {
        panic!()
    };
    assert_eq!(
        runtime.handle(Memory::operation("epoll_ctl"), [epoll, 1, eventfd, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    table.pin(eventfd as i32).unwrap().write(&1_u64.to_le_bytes()).unwrap();
    assert_eq!(
        runtime.handle(Memory::operation("epoll_wait"), [epoll, 32, 1, 0, 0, 0]),
        LinuxResult::Error(Errno::EFAULT),
    );
    runtime.memory.fault_write.store(false, Ordering::Release);
    assert_eq!(
        runtime.handle(Memory::operation("epoll_wait"), [epoll, 32, 1, 0, 0, 0]),
        LinuxResult::Value(1),
    );
    assert_eq!(
        runtime.handle(Memory::operation("epoll_wait"), [epoll, 32, 1, 0, 0, 0]),
        LinuxResult::Value(0),
    );
}

#[test]
fn control_errno_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let table = Arc::new(DescriptorTable::new(8).unwrap());
        let (control, runtime_table) = Control::attach(table.clone(), 8, 32).unwrap();
        let mut bytes = vec![0; 128];
        bytes[0..4].copy_from_slice(&1_u32.to_le_bytes());
        let mut runtime = RuntimeEventSyscalls::new(
            table,
            Arc::new(EventCatalog::new(8).unwrap()),
            Buffer(Mutex::new(bytes)),
            architecture,
        )
        .with_epoll_control(Arc::new(control), Arc::new(runtime_table));
        let LinuxResult::Value(epoll) = runtime.handle(Memory::operation("epoll_create1"), [0; 6]) else {
            panic!()
        };
        let LinuxResult::Value(eventfd) = runtime.handle(Memory::operation("eventfd2"), [0; 6]) else {
            panic!()
        };
        let arguments = [epoll, 1, eventfd, 0, 0, 0];
        assert_eq!(
            runtime.handle(Memory::operation("epoll_ctl"), arguments),
            LinuxResult::Value(0),
        );
        assert_eq!(
            runtime.handle(Memory::operation("epoll_ctl"), arguments),
            LinuxResult::Error(Errno::EEXIST),
        );
        assert_eq!(
            runtime.handle(Memory::operation("epoll_ctl"), [epoll, 2, eventfd, 0, 0, 0],),
            LinuxResult::Value(0),
        );
        assert_eq!(
            runtime.handle(Memory::operation("epoll_ctl"), [epoll, 2, eventfd, 0, 0, 0],),
            LinuxResult::Error(Errno::ENOENT),
        );
    }
}

#[test]
fn pwait_mask_isas() {
    use hl_task::{ProcessCredentials, ProcessLimits, RegistryConfig, SignalMask, TaskRegistry};

    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let tasks = Arc::new(TaskRegistry::new(RegistryConfig::default()).unwrap());
        let (_, thread) = tasks
            .create_init(ProcessCredentials::new(1, 1, &[], 1).unwrap(), ProcessLimits::empty())
            .unwrap();
        let original = SignalMask::from_bits(1_u64 << 10);
        tasks.set_signal_mask(thread, original).unwrap();
        let table = Arc::new(DescriptorTable::new(8).unwrap());
        let (control, runtime_table) = Control::attach(table.clone(), 8, 32).unwrap();
        let mut bytes = vec![0; 128];
        bytes[16..24].copy_from_slice(&(1_u64 << 12).to_le_bytes());
        let interruption = Arc::new(hl_sync::Interruption::new());
        interruption.interrupt();
        let cancellation: Arc<dyn hl_descriptor::OperationCancellation> =
            Arc::new(crate::RuntimePipeCancellation::new(interruption));
        let mut runtime = RuntimeEventSyscalls::new(
            table,
            Arc::new(EventCatalog::new(8).unwrap()),
            Buffer(Mutex::new(bytes)),
            architecture,
        )
        .with_epoll_control(Arc::new(control), Arc::new(runtime_table))
        .with_epoll_wait(tasks.clone(), thread, cancellation);
        let LinuxResult::Value(epoll) = runtime.handle(Memory::operation("epoll_create1"), [0; 6]) else {
            panic!()
        };
        assert_eq!(
            runtime.handle(Memory::operation("epoll_pwait"), [epoll, 32, 1, u64::MAX, 16, 8],),
            LinuxResult::Error(Errno::EINTR),
        );
        assert_eq!(tasks.deliver_thread_state(thread).unwrap().mask, original);
    }
}

#[test]
fn inotify_errno_oracle() {
    let cases = [
        (hl_event::InotifyError::NotFound, Errno::ENOENT),
        (hl_event::InotifyError::NotDirectory, Errno::ENOTDIR),
        (hl_event::InotifyError::NameTooLong, Errno::ENAMETOOLONG),
        (hl_event::InotifyError::PermissionDenied, Errno::EACCES),
        (hl_event::InotifyError::AlreadyExists, Errno::EEXIST),
        (hl_event::InotifyError::ResourceLimit, Errno::ENOMEM),
        (hl_event::InotifyError::Interrupted, Errno::EINTR),
        (hl_event::InotifyError::NotSupported, Errno::ENOSYS),
        (hl_event::InotifyError::SourceFailed, Errno::EIO),
    ];
    for (error, errno) in cases {
        assert_eq!(RuntimeEventSyscalls::<Memory>::inotify_errno(error), errno);
    }
}
