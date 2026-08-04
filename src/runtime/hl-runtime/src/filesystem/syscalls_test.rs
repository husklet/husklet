use super::*;
use hl_descriptor::{
    DescriptorFlags, DirectoryBatch, DirectoryBatchToken, OfdDirectoryEntry, OpenFileDescription, OperationContext,
    OperationLease, PreparedAtomicRead, StatusFlags,
};
use hl_ipc::PIPE_BUF;
use hl_linux::{GuestAccess, GuestFault};
use std::io::IoSlice;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct TerminalCall {
    direction: VectorDirection,
    position: VectorPosition,
    flags: Option<u32>,
    count: usize,
    total: u64,
}

#[derive(Default)]
struct RecordingTerminal(Mutex<Vec<TerminalCall>>);

impl VectorTerminal for RecordingTerminal {
    fn execute(&self, _descriptor: &OperationLease, request: VectorRequest<'_>) -> Result<usize, VectorError> {
        let total = request.vectors.iter().map(|vector| vector.length).sum();
        self.0.lock().unwrap().push(TerminalCall {
            direction: request.direction,
            position: request.position,
            flags: request.flags,
            count: request.vectors.len(),
            total,
        });
        Ok(total as usize)
    }
}
struct Memory {
    bytes: Mutex<Vec<u8>>,
    fail_write: bool,
}
struct PipeSignal(AtomicBool);

#[derive(Debug)]
struct PrefixFile {
    requested: AtomicUsize,
}

impl OpenFileDescription for PrefixFile {
    fn read(&self, output: &mut [u8]) -> Result<usize, ObjectError> {
        self.requested.store(output.len(), Ordering::Release);
        output.fill(0xa5);
        Ok(output.len())
    }
}

#[derive(Debug)]
struct VectorFile;

impl OpenFileDescription for VectorFile {
    fn write(&self, input: &[u8]) -> Result<usize, ObjectError> {
        Ok(input.len())
    }

    fn write_vector_context(
        &self,
        input: &[IoSlice<'_>],
        _context: OperationContext<'_>,
    ) -> Result<usize, ObjectError> {
        Ok(input.iter().map(|segment| segment.len()).sum())
    }
}

#[derive(Debug)]
struct RegularFile;

impl OpenFileDescription for RegularFile {
    fn kind(&self) -> hl_descriptor::ObjectKind {
        hl_descriptor::ObjectKind::File
    }
}

#[derive(Debug, Default)]
struct SealFile(AtomicU8);

impl OpenFileDescription for SealFile {
    fn kind(&self) -> hl_descriptor::ObjectKind {
        hl_descriptor::ObjectKind::File
    }

    fn add_seals(&self, seals: u8) -> Result<u8, ObjectError> {
        let current = self.0.load(Ordering::Acquire);
        if current & 1 != 0 {
            return Err(ObjectError::PermissionDenied);
        }
        let updated = current | seals;
        self.0.store(updated, Ordering::Release);
        Ok(updated)
    }

    fn seals(&self) -> Result<u8, ObjectError> {
        Ok(self.0.load(Ordering::Acquire))
    }
}

#[test]
fn fcntl_seals_forward_to_description_and_duplicates() {
    let table = Arc::new(DescriptorTable::new(4).unwrap());
    let descriptor = table
        .commit(
            table.reserve(0).unwrap(),
            Arc::new(SealFile::default()),
            StatusFlags::default(),
            DescriptorFlags::default(),
        )
        .unwrap();
    let runtime = RuntimeFilesystemSyscalls::new(
        table,
        Memory {
            bytes: Mutex::new(Vec::new()),
            fail_write: false,
        },
        GuestArchitecture::Aarch64,
    );
    let duplicate = match runtime.fcntl(descriptor, 0, 0) {
        LinuxResult::Value(value) => value as i32,
        result => panic!("duplicate failed: {result:?}"),
    };

    assert_eq!(runtime.fcntl(descriptor, 1034, 0), LinuxResult::Value(0));
    assert_eq!(runtime.fcntl(duplicate, 1033, 2), LinuxResult::Value(0));
    assert_eq!(runtime.fcntl(descriptor, 1034, 0), LinuxResult::Value(2));
    assert_eq!(runtime.fcntl(descriptor, 1033, 0x20), LinuxResult::Error(Errno::EINVAL));
    assert_eq!(runtime.fcntl(99, 1033, 0x20), LinuxResult::Error(Errno::EBADF));
    assert_eq!(runtime.fcntl(descriptor, 1033, 1), LinuxResult::Value(0));
    assert_eq!(runtime.fcntl(duplicate, 1033, 2), LinuxResult::Error(Errno::EPERM));
}

#[test]
fn fcntl_seals_reject_plain_descriptions_as_invalid() {
    let table = Arc::new(DescriptorTable::new(1).unwrap());
    let descriptor = table
        .commit(
            table.reserve(0).unwrap(),
            Arc::new(RegularFile),
            StatusFlags::default(),
            DescriptorFlags::default(),
        )
        .unwrap();
    let runtime = RuntimeFilesystemSyscalls::new(
        table,
        Memory {
            bytes: Mutex::new(Vec::new()),
            fail_write: false,
        },
        GuestArchitecture::X86_64,
    );
    assert_eq!(runtime.fcntl(descriptor, 1034, 0), LinuxResult::Error(Errno::EINVAL));
    assert_eq!(runtime.fcntl(descriptor, 1033, 2), LinuxResult::Error(Errno::EINVAL));
}

#[test]
fn fcntl_getfl_projects_largefile_by_guest_abi() {
    for (architecture, largefile) in [(GuestArchitecture::Aarch64, 0), (GuestArchitecture::X86_64, 0x8000)] {
        let table = Arc::new(DescriptorTable::new(1).unwrap());
        let descriptor = table
            .commit(
                table.reserve(0).unwrap(),
                Arc::new(RegularFile),
                StatusFlags::from_bits(2),
                DescriptorFlags::default(),
            )
            .unwrap();
        let runtime = RuntimeFilesystemSyscalls::new(
            table,
            Memory {
                bytes: Mutex::new(Vec::new()),
                fail_write: false,
            },
            architecture,
        );

        assert_eq!(runtime.fcntl(descriptor, 3, 0), LinuxResult::Value(2 | largefile));
    }
}

#[test]
fn fcntl_direct_uses_each_guest_abi_bit() {
    for (architecture, direct) in [
        (GuestArchitecture::Aarch64, 0x1_0000),
        (GuestArchitecture::X86_64, StatusFlags::DIRECT),
    ] {
        let descriptors = Arc::new(DescriptorTable::new(16).unwrap());
        let descriptor = descriptors
            .commit(
                descriptors.reserve(0).unwrap(),
                Arc::new(RegularFile),
                StatusFlags::from_bits(2),
                DescriptorFlags::default(),
            )
            .unwrap();
        let runtime = RuntimeFilesystemSyscalls::new(
            descriptors,
            Memory {
                bytes: Mutex::new(Vec::new()),
                fail_write: false,
            },
            architecture,
        );
        assert_eq!(runtime.fcntl(descriptor, 4, u64::from(direct)), LinuxResult::Value(0));
        let largefile = if architecture == GuestArchitecture::X86_64 {
            0x8000
        } else {
            0
        };
        assert_eq!(
            runtime.fcntl(descriptor, 3, 0),
            LinuxResult::Value(u64::from(2 | direct | largefile)),
        );
    }
}

#[test]
fn fcntl_controls_share_open_description() {
    let table = Arc::new(DescriptorTable::new(4).unwrap());
    let descriptor = table
        .commit(
            table.reserve(0).unwrap(),
            Arc::new(IoctlFile {
                status: Mutex::new(StatusFlags::from_bits(2)),
                size: 0,
            }),
            StatusFlags::from_bits(2),
            DescriptorFlags::default(),
        )
        .unwrap();
    let duplicate = table.duplicate(descriptor, 0, DescriptorFlags::default()).unwrap();
    let runtime = RuntimeFilesystemSyscalls::new(
        table,
        Memory {
            bytes: Mutex::new(vec![0; 16]),
            fail_write: false,
        },
        GuestArchitecture::Aarch64,
    );

    assert_eq!(runtime.fcntl(descriptor, 8, 42), LinuxResult::Value(0));
    assert_eq!(runtime.fcntl(duplicate, 9, 0), LinuxResult::Value(42));
    assert_eq!(runtime.fcntl(descriptor, 16, 0), LinuxResult::Value(0));
    assert_eq!(&runtime.memory.bytes.lock().unwrap()[0..8], &[1, 0, 0, 0, 42, 0, 0, 0]);
    runtime.memory.bytes.lock().unwrap()[0..8].copy_from_slice(&[0, 0, 0, 0, 77, 0, 0, 0]);
    assert_eq!(runtime.fcntl(duplicate, 15, 0), LinuxResult::Value(0));
    assert_eq!(runtime.fcntl(descriptor, 16, 8), LinuxResult::Value(0));
    assert_eq!(&runtime.memory.bytes.lock().unwrap()[8..16], &[0, 0, 0, 0, 77, 0, 0, 0]);
    assert_eq!(runtime.fcntl(descriptor, 16, 12), LinuxResult::Error(Errno::EFAULT));
    assert_eq!(runtime.fcntl(duplicate, 10, 12), LinuxResult::Value(0));
    assert_eq!(runtime.fcntl(descriptor, 11, 0), LinuxResult::Value(12));
    assert_eq!(runtime.fcntl(descriptor, 1024, 0), LinuxResult::Error(Errno::EAGAIN));
    assert_eq!(runtime.fcntl(descriptor, 1025, 0), LinuxResult::Value(2));

    runtime.memory.bytes.lock().unwrap()[0..8].fill(0);
    assert_eq!(runtime.fcntl(descriptor, 1036, 0), LinuxResult::Value(0));
    assert_eq!(runtime.fcntl(duplicate, 1035, 8), LinuxResult::Value(0));
    assert_eq!(&runtime.memory.bytes.lock().unwrap()[8..16], &[0; 8]);
    assert_eq!(runtime.fcntl(99, 1035, 0), LinuxResult::Error(Errno::EBADF));
    assert_eq!(runtime.fcntl(descriptor, 1035, 12), LinuxResult::Error(Errno::EFAULT));
}

#[derive(Debug)]
struct AsyncFile {
    readiness: hl_descriptor::ReadinessRegistry,
}

impl OpenFileDescription for AsyncFile {
    fn set_status_flags(&self, _flags: StatusFlags) -> Result<(), hl_descriptor::ObjectError> {
        Ok(())
    }

    fn subscribe_readiness(
        &self,
        observer: Arc<dyn hl_descriptor::ReadinessObserver>,
    ) -> Result<Box<dyn hl_descriptor::ReadinessSubscription>, hl_descriptor::ObjectError> {
        self.readiness.subscribe(observer)
    }

    fn retire(&self) {
        self.readiness.close();
    }
}

#[derive(Default)]
struct AsyncPort(AtomicUsize);

impl crate::AsyncSignalPort for AsyncPort {
    fn deliver(&self, source: hl_descriptor::SignalSource) -> Result<(), ()> {
        if source.delivery().is_some() {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    }
}

#[test]
fn async_subscription_lifecycle() {
    let readiness = hl_descriptor::ReadinessRegistry::new();
    let table = Arc::new(DescriptorTable::new(4).unwrap());
    let descriptor = table
        .commit(
            table.reserve(0).unwrap(),
            Arc::new(AsyncFile {
                readiness: readiness.clone(),
            }),
            StatusFlags::from_bits(2),
            DescriptorFlags::default(),
        )
        .unwrap();
    let duplicate = table.duplicate(descriptor, 0, DescriptorFlags::default()).unwrap();
    let port = Arc::new(AsyncPort::default());
    let runtime = RuntimeFilesystemSyscalls::new(
        table,
        Memory {
            bytes: Mutex::new(vec![]),
            fail_write: false,
        },
        GuestArchitecture::Aarch64,
    )
    .with_async_signal(port.clone());

    assert_eq!(
        runtime.fcntl(descriptor, 4, u64::from(StatusFlags::ASYNC | 2)),
        LinuxResult::Value(0)
    );
    assert_eq!(
        runtime.fcntl(duplicate, 3, 0),
        LinuxResult::Value(u64::from(StatusFlags::ASYNC | 2))
    );
    assert_eq!(
        runtime.fcntl(duplicate, 4, u64::from(StatusFlags::ASYNC | 2)),
        LinuxResult::Value(0)
    );
    readiness.notify();
    assert_eq!(port.0.load(Ordering::SeqCst), 1);
    assert_eq!(runtime.fcntl(duplicate, 4, 2), LinuxResult::Value(0));
    readiness.notify();
    assert_eq!(port.0.load(Ordering::SeqCst), 1);
}

#[derive(Debug)]
struct DirectoryFile;

impl OpenFileDescription for DirectoryFile {
    fn kind(&self) -> hl_descriptor::ObjectKind {
        hl_descriptor::ObjectKind::Directory
    }
}

#[test]
fn scalar_io_rejects_access_mode_before_count_and_pointer() {
    let table = Arc::new(DescriptorTable::new(2).unwrap());
    let write_only = table
        .commit(
            table.reserve(0).unwrap(),
            Arc::new(PrefixFile {
                requested: AtomicUsize::new(0),
            }),
            StatusFlags::from_bits(1),
            DescriptorFlags::default(),
        )
        .unwrap();
    let read_only = table
        .commit(
            table.reserve(0).unwrap(),
            Arc::new(VectorFile),
            StatusFlags::default(),
            DescriptorFlags::default(),
        )
        .unwrap();
    let runtime = RuntimeFilesystemSyscalls::new(
        table,
        Memory {
            bytes: Mutex::new(vec![0; 16]),
            fail_write: false,
        },
        GuestArchitecture::Aarch64,
    );
    assert_eq!(runtime.read(write_only, u64::MAX, 0), LinuxResult::Error(Errno::EBADF));
    assert_eq!(runtime.read(write_only, u64::MAX, 1), LinuxResult::Error(Errno::EBADF));
    assert_eq!(runtime.write(read_only, u64::MAX, 0), LinuxResult::Error(Errno::EBADF));
    assert_eq!(runtime.write(read_only, u64::MAX, 1), LinuxResult::Error(Errno::EBADF));
}

fn pipe_write_adapter(memory_length: usize) -> (RuntimeFilesystemSyscalls<Memory>, OperationLease, i32) {
    registered_pipe_adapter(vec![0xa5; 8 + memory_length], GuestArchitecture::Aarch64)
}

fn registered_pipe_adapter(
    bytes: Vec<u8>,
    architecture: GuestArchitecture,
) -> (RuntimeFilesystemSyscalls<Memory>, OperationLease, i32) {
    let table = Arc::new(DescriptorTable::new(2).unwrap());
    let assembly = crate::RuntimeAssembly::new(Default::default()).unwrap();
    let shared = Arc::new(hl_memory::SharedObjectStore::new(hl_memory::SharedLimits::default()).unwrap());
    assembly.install_ipc(shared).unwrap();
    let runtime = RuntimeFilesystemSyscalls::new(
        table.clone(),
        Memory {
            bytes: Mutex::new(bytes),
            fail_write: false,
        },
        architecture,
    )
    .with_pipe_registry(assembly.ipc_pipes().unwrap());
    assert_eq!(runtime.pipe2(0, 0o00004000), LinuxResult::Value(0));
    (runtime, table.pin(0).unwrap(), 1)
}

#[test]
fn scalar_pipe_write_rolls_back_on_atomic_source_fault() {
    let (runtime, reader, descriptor) = pipe_write_adapter(PIPE_BUF - 1);

    assert_eq!(
        runtime.write(descriptor, 8, PIPE_BUF as u64),
        LinuxResult::Error(Errno::EFAULT)
    );
    assert_eq!(reader.read(&mut [0_u8; 1]), Err(ObjectError::WouldBlock));
}

#[test]
fn scalar_pipe_write_keeps_large_partial_progress() {
    let (runtime, reader, descriptor) = pipe_write_adapter(PIPE_BUF);

    assert_eq!(
        runtime.write(descriptor, 8, (PIPE_BUF + 1) as u64),
        LinuxResult::Value(PIPE_BUF as u64),
    );
    let mut output = vec![0; PIPE_BUF];
    assert_eq!(reader.read(&mut output), Ok(PIPE_BUF));
    assert!(output.iter().all(|byte| *byte == 0xa5));
}

fn write_iovec(bytes: &mut [u8], record: usize, base: u64, length: u64) {
    bytes[record..record + 8].copy_from_slice(&base.to_le_bytes());
    bytes[record + 8..record + 16].copy_from_slice(&length.to_le_bytes());
}

#[test]
fn atomic_pipe_vector_rolls_back_source_fault() {
    const PAYLOAD: usize = 40;
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let mut bytes = vec![0xa5; PAYLOAD + 16];
        write_iovec(&mut bytes, 8, PAYLOAD as u64, 8);
        write_iovec(&mut bytes, 24, (PAYLOAD + 8) as u64, 32);
        let (runtime, reader, descriptor) = registered_pipe_adapter(bytes, architecture);

        assert_eq!(
            runtime.vector_io(descriptor, 8, 2, false),
            LinuxResult::Error(Errno::EFAULT)
        );
        assert_eq!(reader.read(&mut [0_u8; 1]), Err(ObjectError::WouldBlock));
    }
}

#[test]
fn atomic_pipe_vector_rejects_source_overflow() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let mut bytes = vec![0; 24];
        write_iovec(&mut bytes, 8, u64::MAX - 3, 8);
        let (runtime, reader, descriptor) = registered_pipe_adapter(bytes, architecture);

        assert_eq!(
            runtime.vector_io(descriptor, 8, 1, false),
            LinuxResult::Error(Errno::EFAULT)
        );
        assert_eq!(reader.read(&mut [0_u8; 1]), Err(ObjectError::WouldBlock));
    }
}

#[test]
fn atomic_pipe_vector_keeps_large_partial_progress() {
    const PAYLOAD: usize = 40;
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let mut bytes = vec![0xa5; PAYLOAD + PIPE_BUF];
        write_iovec(&mut bytes, 8, PAYLOAD as u64, PIPE_BUF as u64);
        write_iovec(&mut bytes, 24, (PAYLOAD + PIPE_BUF) as u64, 1);
        let (runtime, reader, descriptor) = registered_pipe_adapter(bytes, architecture);

        assert_eq!(
            runtime.vector_io(descriptor, 8, 2, false),
            LinuxResult::Value(PIPE_BUF as u64)
        );
        let mut output = vec![0; PIPE_BUF];
        assert_eq!(reader.read(&mut output), Ok(PIPE_BUF));
        assert!(output.iter().all(|byte| *byte == 0xa5));
    }
}

#[test]
fn scalar_read_rejects_directory_before_count_and_pointer() {
    let table = Arc::new(DescriptorTable::new(1).unwrap());
    let directory = table
        .commit(
            table.reserve(0).unwrap(),
            Arc::new(DirectoryFile),
            StatusFlags::default(),
            DescriptorFlags::default(),
        )
        .unwrap();
    let runtime = RuntimeFilesystemSyscalls::new(
        table,
        Memory {
            bytes: Mutex::new(vec![0; 16]),
            fail_write: false,
        },
        GuestArchitecture::Aarch64,
    );
    assert_eq!(runtime.read(directory, u64::MAX, 0), LinuxResult::Error(Errno::EISDIR));
    assert_eq!(runtime.read(directory, u64::MAX, 1), LinuxResult::Error(Errno::EISDIR));
}

#[test]
fn readahead_admission() {
    let table = Arc::new(DescriptorTable::new(2).unwrap());
    let descriptor = table
        .commit(
            table.reserve(0).unwrap(),
            Arc::new(RegularFile),
            StatusFlags::default(),
            DescriptorFlags::default(),
        )
        .unwrap();
    let mut adapter = RuntimeFilesystemSyscalls::new(
        table,
        Memory {
            bytes: Mutex::new(Vec::new()),
            fail_write: false,
        },
        GuestArchitecture::Aarch64,
    );
    let operation = SyscallOperation {
        canonical_number: 213,
        name: "readahead",
        family: hl_linux::SyscallFamily::DescriptorIo,
    };
    assert_eq!(
        DescriptorIoSyscalls::handle(&mut adapter, operation, [descriptor as u64, 0, 4096, 0, 0, 0]),
        LinuxResult::Value(0),
    );
    assert_eq!(
        DescriptorIoSyscalls::handle(&mut adapter, operation, [descriptor as u64, u64::MAX, 1, 0, 0, 0]),
        LinuxResult::Error(Errno::EINVAL),
    );
    assert_eq!(
        DescriptorIoSyscalls::handle(&mut adapter, operation, [i32::MAX as u64, u64::MAX, 1, 0, 0, 0]),
        LinuxResult::Error(Errno::EBADF),
    );
}

fn vector_adapter(capacity: usize) -> (RuntimeFilesystemSyscalls<Memory>, i32) {
    let table = Arc::new(DescriptorTable::new(2).unwrap());
    let descriptor = table
        .commit(
            table.reserve(0).unwrap(),
            Arc::new(VectorFile),
            StatusFlags::from_bits(2),
            DescriptorFlags::default(),
        )
        .unwrap();
    let memory = Memory {
        bytes: Mutex::new(vec![0; capacity]),
        fail_write: false,
    };
    (
        RuntimeFilesystemSyscalls::new(table, memory, GuestArchitecture::Aarch64),
        descriptor,
    )
}

#[test]
fn fadvise_admission() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let mut adapter = RuntimeFilesystemSyscalls::new(
            Arc::new(DescriptorTable::new(1).unwrap()),
            Memory {
                bytes: Mutex::new(Vec::new()),
                fail_write: false,
            },
            architecture,
        );
        let operation = SyscallOperation {
            canonical_number: 223,
            name: "fadvise64",
            family: hl_linux::SyscallFamily::Filesystem,
        };
        for advice in 0..=5 {
            assert_eq!(
                FilesystemSyscalls::handle(&mut adapter, operation, [i32::MAX as u64, 7, 11, advice, 0, 0]),
                LinuxResult::Value(0),
            );
        }
        assert_eq!(
            FilesystemSyscalls::handle(&mut adapter, operation, [0, 0, 0, 6, 0, 0]),
            LinuxResult::Error(Errno::EINVAL),
        );
    }
}

#[test]
fn fallocate_validation() {
    let (mut adapter, descriptor) = vector_adapter(0);
    let operation = SyscallOperation {
        canonical_number: 47,
        name: "fallocate",
        family: hl_linux::SyscallFamily::Filesystem,
    };
    let call = |adapter: &mut RuntimeFilesystemSyscalls<Memory>, arguments| {
        FilesystemSyscalls::handle(adapter, operation, arguments)
    };
    assert_eq!(
        call(&mut adapter, [descriptor as u64, 0, 0, 0, 0, 0]),
        LinuxResult::Error(Errno::EINVAL)
    );
    assert_eq!(
        call(&mut adapter, [descriptor as u64, 4, 0, 1, 0, 0]),
        LinuxResult::Error(Errno::EOPNOTSUPP)
    );
    assert_eq!(
        call(&mut adapter, [descriptor as u64, 2, 0, 1, 0, 0]),
        LinuxResult::Error(Errno::EINVAL)
    );
    assert_eq!(
        call(&mut adapter, [descriptor as u64, 0, i64::MAX as u64, 1, 0, 0]),
        LinuxResult::Error(Errno::EFBIG),
    );
    assert_eq!(
        call(&mut adapter, [descriptor as u64, 0, 0, 1, 0, 0]),
        LinuxResult::Error(Errno::EOPNOTSUPP),
    );
    assert_eq!(
        call(&mut adapter, [99, 0, 0, 1, 0, 0]),
        LinuxResult::Error(Errno::EBADF)
    );
}

#[test]
fn vector_terminal_contract() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let table = Arc::new(DescriptorTable::new(2).unwrap());
        let descriptor = table
            .commit(
                table.reserve(0).unwrap(),
                Arc::new(VectorFile),
                StatusFlags::default(),
                DescriptorFlags::default(),
            )
            .unwrap();
        let mut bytes = vec![0_u8; 96];
        bytes[0..8].copy_from_slice(&64_u64.to_le_bytes());
        bytes[8..16].copy_from_slice(&2_u64.to_le_bytes());
        bytes[16..24].copy_from_slice(&80_u64.to_le_bytes());
        bytes[24..32].copy_from_slice(&3_u64.to_le_bytes());
        let terminal = Arc::new(RecordingTerminal::default());
        let adapter = RuntimeFilesystemSyscalls::new(
            table,
            Memory {
                bytes: Mutex::new(bytes),
                fail_write: false,
            },
            architecture,
        )
        .with_vector_terminal(terminal.clone());

        assert_eq!(adapter.vector_io(descriptor, 0, 2, false), LinuxResult::Value(5));
        assert_eq!(
            adapter.positional_vector(descriptor, 0, 2, 19, true, Some(0x10)),
            LinuxResult::Value(5),
        );
        assert_eq!(
            adapter.positional_vector(descriptor, 0, 2, u64::MAX, false, Some(0)),
            LinuxResult::Value(5),
        );
        assert_eq!(
            *terminal.0.lock().unwrap(),
            [
                TerminalCall {
                    direction: VectorDirection::Write,
                    position: VectorPosition::Shared,
                    flags: None,
                    count: 2,
                    total: 5,
                },
                TerminalCall {
                    direction: VectorDirection::Read,
                    position: VectorPosition::At(19),
                    flags: Some(0x10),
                    count: 2,
                    total: 5,
                },
                TerminalCall {
                    direction: VectorDirection::Write,
                    position: VectorPosition::Shared,
                    flags: Some(0),
                    count: 2,
                    total: 5,
                },
            ],
        );
    }
}

#[test]
fn x86_vector_v2_uses_sixth_argument_for_rwf_flags() {
    let table = Arc::new(DescriptorTable::new(2).unwrap());
    let descriptor = table
        .commit(
            table.reserve(0).unwrap(),
            Arc::new(VectorFile),
            StatusFlags::from_bits(2),
            DescriptorFlags::default(),
        )
        .unwrap();
    let mut bytes = vec![0_u8; 32];
    bytes[0..8].copy_from_slice(&16_u64.to_le_bytes());
    bytes[8..16].copy_from_slice(&3_u64.to_le_bytes());
    let terminal = Arc::new(RecordingTerminal::default());
    let mut adapter = RuntimeFilesystemSyscalls::new(
        table,
        Memory {
            bytes: Mutex::new(bytes),
            fail_write: false,
        },
        GuestArchitecture::X86_64,
    )
    .with_vector_terminal(terminal.clone());
    let operation = SyscallOperation {
        canonical_number: 287,
        name: "pwritev2",
        family: hl_linux::SyscallFamily::DescriptorIo,
    };
    assert_eq!(
        DescriptorIoSyscalls::handle(&mut adapter, operation, [descriptor as u64, 0, 1, 7, 0, 0x10],),
        LinuxResult::Value(3),
    );
    assert_eq!(
        terminal.0.lock().unwrap().as_slice(),
        &[TerminalCall {
            direction: VectorDirection::Write,
            position: VectorPosition::At(7),
            flags: Some(0x10),
            count: 1,
            total: 3,
        }],
    );
}
impl crate::PipeSignalPort for PipeSignal {
    fn queue_sigpipe(&self) -> Result<(), ()> {
        self.0.store(true, Ordering::Release);
        Ok(())
    }
}
impl GuestMemory for Memory {
    fn probe(&self, address: u64, length: usize, _access: GuestAccess) -> Result<usize, GuestFault> {
        Ok(self
            .bytes
            .lock()
            .unwrap()
            .len()
            .saturating_sub(address as usize)
            .min(length))
    }
    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        let bytes = self.bytes.lock().unwrap();
        let count = output.len().min(bytes.len().saturating_sub(address as usize));
        output[..count].copy_from_slice(&bytes[address as usize..address as usize + count]);
        Ok(count)
    }
    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        if self.fail_write {
            return Err(GuestFault {
                address,
                access: GuestAccess::Write,
            });
        }
        let mut bytes = self.bytes.lock().unwrap();
        let count = input.len().min(bytes.len().saturating_sub(address as usize));
        bytes[address as usize..address as usize + count].copy_from_slice(&input[..count]);
        Ok(count)
    }
}
#[derive(Debug)]
struct Directory {
    cursor: Mutex<usize>,
}

#[derive(Debug)]
struct AtomicEvent {
    records: Arc<Mutex<Vec<Vec<u8>>>>,
}

#[derive(Debug)]
struct IoctlFile {
    status: Mutex<StatusFlags>,
    size: u64,
}

impl OpenFileDescription for IoctlFile {
    fn kind(&self) -> hl_descriptor::ObjectKind {
        hl_descriptor::ObjectKind::File
    }

    fn metadata(&self) -> Result<hl_descriptor::OfdMetadata, ObjectError> {
        let timestamp = hl_descriptor::OfdTimestamp {
            seconds: 0,
            nanoseconds: 0,
        };
        Ok(hl_descriptor::OfdMetadata {
            device: 1,
            inode: 2,
            kind: 8,
            permissions: 0o600,
            links: 1,
            user: 0,
            group: 0,
            special_device: 0,
            size: self.size,
            blocks_512: 0,
            accessed: timestamp,
            modified: timestamp,
            changed: timestamp,
        })
    }

    fn set_status_flags(&self, flags: StatusFlags) -> Result<(), ObjectError> {
        *self.status.lock().unwrap() = flags;
        Ok(())
    }
}

struct PreparedEvent {
    records: Arc<Mutex<Vec<Vec<u8>>>>,
    bytes: Vec<u8>,
}

impl PreparedAtomicRead for PreparedEvent {
    fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    fn commit(self: Box<Self>) -> Result<(), ObjectError> {
        let mut records = self.records.lock().unwrap();
        if records.first() != Some(&self.bytes) {
            return Err(ObjectError::Interrupted);
        }
        records.remove(0);
        Ok(())
    }
}

impl OpenFileDescription for AtomicEvent {
    fn prepare_atomic_read(&self, maximum: usize) -> Result<Option<Box<dyn PreparedAtomicRead>>, ObjectError> {
        let records = self.records.lock().unwrap();
        let Some(bytes) = records.first() else {
            return Err(ObjectError::WouldBlock);
        };
        if bytes.len() > maximum {
            return Err(ObjectError::InvalidArgument);
        }
        Ok(Some(Box::new(PreparedEvent {
            records: self.records.clone(),
            bytes: bytes.clone(),
        })))
    }
}

struct ToggleMemory {
    bytes: Mutex<Vec<u8>>,
    fault: AtomicBool,
}

impl GuestMemory for ToggleMemory {
    fn probe(&self, _address: u64, length: usize, _access: GuestAccess) -> Result<usize, GuestFault> {
        Ok(length)
    }
    fn read(&self, address: u64, output: &mut [u8]) -> Result<usize, GuestFault> {
        let bytes = self.bytes.lock().unwrap();
        output.copy_from_slice(&bytes[address as usize..address as usize + output.len()]);
        Ok(output.len())
    }
    fn write(&self, address: u64, input: &[u8]) -> Result<usize, GuestFault> {
        if self.fault.load(Ordering::Acquire) {
            return Err(GuestFault {
                address,
                access: GuestAccess::Write,
            });
        }
        self.bytes.lock().unwrap()[address as usize..address as usize + input.len()].copy_from_slice(input);
        Ok(input.len())
    }
}

#[test]
fn atomic_descriptor_reuse() {
    for size in [8, 16, 128] {
        let table = Arc::new(DescriptorTable::new(4).unwrap());
        let records = Arc::new(Mutex::new(vec![vec![size as u8; size]]));
        let object = Arc::new(AtomicEvent {
            records: records.clone(),
        });
        let fd = table
            .commit(
                table.reserve(0).unwrap(),
                object,
                StatusFlags::default(),
                DescriptorFlags::default(),
            )
            .unwrap();
        let runtime = RuntimeFilesystemSyscalls::new(
            table.clone(),
            ToggleMemory {
                bytes: Mutex::new(vec![0; 256]),
                fault: AtomicBool::new(true),
            },
            GuestArchitecture::Aarch64,
        );
        assert_eq!(runtime.read(fd, 0, size as u64), LinuxResult::Error(Errno::EFAULT));
        assert_eq!(records.lock().unwrap().len(), 1);
        records.lock().unwrap().push(vec![0xaa; size]);
        runtime.memory.fault.store(false, Ordering::Release);
        assert_eq!(runtime.read(fd, 0, size as u64), LinuxResult::Value(size as u64));
        assert_eq!(records.lock().unwrap().as_slice(), &[vec![0xaa; size]]);

        let stale = table.pin(fd).unwrap().prepare_atomic_read(size).unwrap().unwrap();
        table.close(fd).unwrap();
        let replacement_records = Arc::new(Mutex::new(vec![vec![0x55; size]]));
        let replacement = Arc::new(AtomicEvent {
            records: replacement_records.clone(),
        });
        assert_eq!(
            table
                .commit(
                    table.reserve(fd).unwrap(),
                    replacement,
                    StatusFlags::default(),
                    DescriptorFlags::default(),
                )
                .unwrap(),
            fd
        );
        stale.commit().unwrap();
        assert_eq!(replacement_records.lock().unwrap().as_slice(), &[vec![0x55; size]]);
    }
}

#[test]
fn read_writable_prefix() {
    let table = Arc::new(DescriptorTable::new(2).unwrap());
    let object = Arc::new(PrefixFile {
        requested: AtomicUsize::new(0),
    });
    let descriptor = table
        .commit(
            table.reserve(0).unwrap(),
            object.clone(),
            StatusFlags::default(),
            DescriptorFlags::default(),
        )
        .unwrap();
    let runtime = RuntimeFilesystemSyscalls::new(
        table,
        Memory {
            bytes: Mutex::new(vec![0; 4096]),
            fail_write: false,
        },
        GuestArchitecture::Aarch64,
    );

    assert_eq!(runtime.read(descriptor, 0, 8192), LinuxResult::Value(4096));
    assert_eq!(object.requested.load(Ordering::Acquire), 4096);
    assert!(runtime.memory.bytes.lock().unwrap().iter().all(|byte| *byte == 0xa5));
}

#[test]
fn vectors_reject_excess() {
    let (adapter, descriptor) = vector_adapter(16);
    assert_eq!(
        adapter.vector_io(descriptor, 0, 1025, false),
        LinuxResult::Error(Errno::EINVAL),
    );
}

#[test]
fn vectors_ignore_wild() {
    let (adapter, descriptor) = vector_adapter(0);
    assert_eq!(
        adapter.vector_io(descriptor, u64::MAX - 7, 1025, false),
        LinuxResult::Error(Errno::EINVAL),
    );
}

#[test]
fn vectors_bad_fd() {
    let (adapter, _) = vector_adapter(0);
    assert_eq!(
        adapter.vector_io(1, u64::MAX, 1025, false),
        LinuxResult::Error(Errno::EBADF),
    );
}

#[test]
fn vectors_accept_empty_after_descriptor_validation() {
    let (adapter, descriptor) = vector_adapter(0);
    assert_eq!(adapter.vector_io(descriptor, u64::MAX, 0, false), LinuxResult::Value(0));
    assert_eq!(
        adapter.vector_io(1, u64::MAX, 0, false),
        LinuxResult::Error(Errno::EBADF)
    );
}

#[test]
fn vectors_accept_boundary() {
    let (adapter, descriptor) = vector_adapter(1024 * 16);
    assert_eq!(adapter.vector_io(descriptor, 0, 1024, false), LinuxResult::Value(0),);
}

#[test]
fn vectors_fault_array() {
    let (adapter, descriptor) = vector_adapter(1024 * 16 - 1);
    assert_eq!(
        adapter.vector_io(descriptor, 0, 1024, false),
        LinuxResult::Error(Errno::EFAULT),
    );
}

#[test]
fn fd_precedence() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        for (reading, status) in [(false, StatusFlags::default()), (true, StatusFlags::from_bits(1))] {
            let table = Arc::new(DescriptorTable::new(2).unwrap());
            let descriptor = table
                .commit(
                    table.reserve(0).unwrap(),
                    Arc::new(VectorFile),
                    status,
                    DescriptorFlags::default(),
                )
                .unwrap();
            let adapter = RuntimeFilesystemSyscalls::new(
                table,
                Memory {
                    bytes: Mutex::new(Vec::new()),
                    fail_write: false,
                },
                architecture,
            );
            assert_eq!(
                adapter.vector_io(descriptor, 0, 1, reading),
                LinuxResult::Error(Errno::EBADF)
            );
        }
    }
}

#[test]
fn overflowing_vector_range_fails_before_terminal() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let table = Arc::new(DescriptorTable::new(2).unwrap());
        let descriptor = table
            .commit(
                table.reserve(0).unwrap(),
                Arc::new(VectorFile),
                StatusFlags::from_bits(2),
                DescriptorFlags::default(),
            )
            .unwrap();
        let mut bytes = vec![0_u8; 96];
        bytes[0..8].copy_from_slice(&64_u64.to_le_bytes());
        bytes[8..16].copy_from_slice(&2_u64.to_le_bytes());
        bytes[16..24].copy_from_slice(&u64::MAX.to_le_bytes());
        bytes[24..32].copy_from_slice(&2_u64.to_le_bytes());
        let terminal = Arc::new(RecordingTerminal::default());
        let adapter = RuntimeFilesystemSyscalls::new(
            table,
            Memory {
                bytes: Mutex::new(bytes),
                fail_write: false,
            },
            architecture,
        )
        .with_vector_terminal(terminal.clone());

        assert_eq!(
            adapter.vector_io(descriptor, 0, 2, false),
            LinuxResult::Error(Errno::EFAULT)
        );
        assert!(terminal.0.lock().unwrap().is_empty());
    }
}

#[test]
fn v2_rejects_rwf() {
    let (adapter, descriptor) = vector_adapter(0);
    assert_eq!(
        adapter.positional_vector(descriptor, 0, 0, 0, false, Some(0x10)),
        LinuxResult::Error(Errno::EOPNOTSUPP),
    );
}

#[test]
fn v2_shared_offset() {
    let (adapter, descriptor) = vector_adapter(0);
    assert_eq!(
        adapter.positional_vector(descriptor, 0, 0, u64::MAX, false, Some(0)),
        LinuxResult::Value(0),
    );
    assert_eq!(
        adapter.positional_vector(descriptor, 0, 0, u64::MAX, false, None),
        LinuxResult::Error(Errno::EINVAL),
    );
}
impl OpenFileDescription for Directory {
    fn metadata(&self) -> Result<hl_descriptor::OfdMetadata, ObjectError> {
        let time = hl_descriptor::OfdTimestamp {
            seconds: 4,
            nanoseconds: 5,
        };
        Ok(hl_descriptor::OfdMetadata {
            device: 11,
            inode: 22,
            kind: 8,
            permissions: 0o640,
            links: 2,
            user: 33,
            group: 44,
            special_device: 0,
            size: 55,
            blocks_512: 8,
            accessed: time,
            modified: time,
            changed: time,
        })
    }
    fn read_directory(&self, _maximum: usize) -> Result<DirectoryBatch, ObjectError> {
        let cursor = *self.cursor.lock().unwrap();
        Ok(DirectoryBatch {
            token: DirectoryBatchToken {
                generation: 1,
                cookie: cursor as i64,
            },
            entries: if cursor == 0 {
                vec![OfdDirectoryEntry {
                    inode: 7,
                    cookie: 1,
                    file_type: 8,
                    name: vec![0xff, b'x'],
                }]
            } else {
                Vec::new()
            },
        })
    }
    fn commit_directory(&self, token: DirectoryBatchToken, count: usize) -> Result<(), ObjectError> {
        let mut cursor = self.cursor.lock().unwrap();
        if token.cookie != *cursor as i64 {
            return Err(ObjectError::InvalidArgument);
        }
        *cursor += count;
        Ok(())
    }
}
impl Directory {
    fn adapter(fail_write: bool) -> (RuntimeFilesystemSyscalls<Memory>, Arc<Directory>, i32) {
        let table = Arc::new(DescriptorTable::new(8).unwrap());
        let directory = Arc::new(Directory { cursor: Mutex::new(0) });
        let fd = table
            .commit(
                table.reserve(0).unwrap(),
                directory.clone(),
                StatusFlags::default(),
                DescriptorFlags::default(),
            )
            .unwrap();
        let memory = Memory {
            bytes: Mutex::new(vec![0; 256]),
            fail_write,
        };
        (
            RuntimeFilesystemSyscalls::new(table, memory, GuestArchitecture::Aarch64),
            directory,
            fd,
        )
    }
}
#[test]
fn getdents_utf8_name() {
    let (failed, directory, fd) = Directory::adapter(true);
    assert_eq!(failed.getdents(fd, 0, 64), LinuxResult::Error(Errno::EFAULT));
    assert_eq!(*directory.cursor.lock().unwrap(), 0);
    let (retry, retry_directory, retry_fd) = Directory::adapter(false);
    assert_eq!(retry.getdents(retry_fd, 0, 64), LinuxResult::Value(24));
    assert_eq!(*retry_directory.cursor.lock().unwrap(), 1);
    assert_eq!(&retry.memory.bytes.lock().unwrap()[19..21], &[0xff, b'x']);
    assert_eq!(retry.getdents(retry_fd, 0, 64), LinuxResult::Value(0));
}
#[test]
fn getdents_record_commit() {
    let (adapter, directory, fd) = Directory::adapter(false);
    assert_eq!(adapter.getdents(fd, 0, 23), LinuxResult::Error(Errno::EINVAL));
    assert_eq!(*directory.cursor.lock().unwrap(), 0);
}
#[test]
fn fstat_size_isas() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let (mut adapter, _, fd) = Directory::adapter(false);
        adapter.architecture = architecture;
        assert_eq!(adapter.fstat(fd, 0), LinuxResult::Value(0));
        let bytes = adapter.memory.bytes.lock().unwrap();
        match architecture {
            GuestArchitecture::Aarch64 => {
                assert_eq!(u64::from_le_bytes(bytes[0..8].try_into().unwrap()), 11);
                assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), 22);
                assert_eq!(u32::from_le_bytes(bytes[16..20].try_into().unwrap()), 0o100640);
                assert_eq!(u64::from_le_bytes(bytes[48..56].try_into().unwrap()), 55);
            }
            GuestArchitecture::X86_64 => {
                assert_eq!(u64::from_le_bytes(bytes[0..8].try_into().unwrap()), 11);
                assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), 22);
                assert_eq!(u32::from_le_bytes(bytes[24..28].try_into().unwrap()), 0o100640);
                assert_eq!(u64::from_le_bytes(bytes[48..56].try_into().unwrap()), 55);
            }
        }
    }
}
#[test]
fn closed_directory_cursor() {
    let (adapter, old, fd) = Directory::adapter(false);
    let stale = adapter.descriptors.pin(fd).unwrap();
    let batch = stale.read_directory(1).unwrap();
    adapter.descriptors.close(fd).unwrap();
    let replacement = Arc::new(Directory { cursor: Mutex::new(0) });
    let reused = adapter
        .descriptors
        .commit(
            adapter.descriptors.reserve(fd).unwrap(),
            replacement.clone(),
            StatusFlags::default(),
            DescriptorFlags::default(),
        )
        .unwrap();
    assert_eq!(reused, fd);
    stale.commit_directory(batch.token, 1).unwrap();
    assert_eq!(*old.cursor.lock().unwrap(), 1);
    assert_eq!(*replacement.cursor.lock().unwrap(), 0);
}

#[test]
fn pipe2_exact_copyout() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let descriptors = Arc::new(DescriptorTable::new(8).unwrap());
        let assembly = crate::RuntimeAssembly::new(Default::default()).unwrap();
        let shared = Arc::new(hl_memory::SharedObjectStore::new(hl_memory::SharedLimits::default()).unwrap());
        assembly.install_ipc(shared).unwrap();
        let registry = assembly.ipc_pipes().unwrap();
        let memory = Memory {
            bytes: Mutex::new(vec![0; 8]),
            fail_write: false,
        };
        let adapter = RuntimeFilesystemSyscalls::new(descriptors.clone(), memory, architecture)
            .with_pipe_registry(registry.clone());
        let direct = match architecture {
            GuestArchitecture::Aarch64 => 0x1_0000,
            GuestArchitecture::X86_64 => StatusFlags::DIRECT,
        };
        assert_eq!(adapter.pipe2(0, 0o02004000 | direct), LinuxResult::Value(0));
        assert_eq!(&*adapter.memory.bytes.lock().unwrap(), &[0, 0, 0, 0, 1, 0, 0, 0]);
        for descriptor in [0, 1] {
            assert!(descriptors.flags(descriptor).unwrap().closes_on_exec());
            assert_ne!(
                descriptors.pin(descriptor).unwrap().status().bits() & StatusFlags::DIRECT,
                0
            );
        }
        assert_eq!(adapter.read(0, 0, 1), LinuxResult::Error(Errno::EAGAIN));
        assert_eq!(adapter.write(1, 0, 1), LinuxResult::Value(1));
        let catalog = assembly.ipc().unwrap();
        catalog.freeze_checkpoint();
        let image = catalog.checkpoint_image().unwrap();
        catalog.thaw_checkpoint();
        assert_eq!(image.pipes.len(), 1);
        assert_eq!(image.pipes[0].snapshot.bytes, [0]);
        descriptors.freeze_checkpoint();
        let descriptor_image = descriptors.checkpoint_image(registry.bindings().as_ref()).unwrap();
        descriptors.thaw_checkpoint();
        assert_eq!(descriptor_image.descriptions.len(), 2);
    }
}

#[test]
fn pipe_blocking_classification_tracks_live_readiness() {
    let descriptors = Arc::new(DescriptorTable::new(2).unwrap());
    let adapter = RuntimeFilesystemSyscalls::new(
        Arc::clone(&descriptors),
        Memory {
            bytes: Mutex::new(vec![0x5a; PIPE_BUF + 1]),
            fail_write: false,
        },
        GuestArchitecture::Aarch64,
    );
    assert_eq!(adapter.pipe2(0, 0), LinuxResult::Value(0));
    let read = SyscallOperation {
        canonical_number: 63,
        name: "read",
        family: hl_linux::SyscallFamily::DescriptorIo,
    };
    let write = SyscallOperation {
        canonical_number: 64,
        name: "write",
        family: hl_linux::SyscallFamily::DescriptorIo,
    };

    assert!(DescriptorIoSyscalls::may_block(&adapter, read, [0, 0, 8, 0, 0, 0]));
    assert!(!DescriptorIoSyscalls::may_block(&adapter, write, [1, 0, 8, 0, 0, 0]));
    assert!(DescriptorIoSyscalls::may_block(
        &adapter,
        write,
        [1, 0, (PIPE_BUF + 1) as u64, 0, 0, 0],
    ));
    assert_eq!(adapter.write(1, 0, 8), LinuxResult::Value(8));
    assert!(!DescriptorIoSyscalls::may_block(&adapter, read, [0, 0, 8, 0, 0, 0]));

    descriptors
        .pin(0)
        .unwrap()
        .set_status(StatusFlags::from_bits(StatusFlags::NONBLOCKING))
        .unwrap();
    assert!(!DescriptorIoSyscalls::may_block(&adapter, read, [0, 0, 8, 0, 0, 0]));
}

#[test]
fn fragmented_pipe_capacity_stays_on_waiter_lane() {
    let descriptors = Arc::new(DescriptorTable::new(2).unwrap());
    let adapter = RuntimeFilesystemSyscalls::new(
        Arc::clone(&descriptors),
        Memory {
            bytes: Mutex::new(vec![0x5a; hl_ipc::DEFAULT_PIPE_CAPACITY]),
            fail_write: false,
        },
        GuestArchitecture::Aarch64,
    );
    assert_eq!(adapter.pipe2(0, 0), LinuxResult::Value(0));
    let write = SyscallOperation {
        canonical_number: 64,
        name: "write",
        family: hl_linux::SyscallFamily::DescriptorIo,
    };
    let fill = hl_ipc::DEFAULT_PIPE_CAPACITY - PIPE_BUF;
    assert_eq!(adapter.write(1, 0, fill as u64), LinuxResult::Value(fill as u64));
    assert_eq!(adapter.read(0, 0, 1), LinuxResult::Value(1));
    assert_eq!(adapter.write(1, 0, 1), LinuxResult::Value(1));

    assert!(DescriptorIoSyscalls::may_block(
        &adapter,
        write,
        [1, 0, PIPE_BUF as u64, 0, 0, 0],
    ));

    assert_eq!(adapter.read(0, 0, fill as u64), LinuxResult::Value(fill as u64));
    assert!(!DescriptorIoSyscalls::may_block(
        &adapter,
        write,
        [1, 0, PIPE_BUF as u64, 0, 0, 0],
    ));
}

#[test]
fn pipe2_leaks_fd() {
    let descriptors = Arc::new(DescriptorTable::new(2).unwrap());
    let assembly = crate::RuntimeAssembly::new(Default::default()).unwrap();
    let shared = Arc::new(hl_memory::SharedObjectStore::new(hl_memory::SharedLimits::default()).unwrap());
    assembly.install_ipc(shared).unwrap();
    let adapter = RuntimeFilesystemSyscalls::new(
        descriptors.clone(),
        Memory {
            bytes: Mutex::new(vec![0; 8]),
            fail_write: true,
        },
        GuestArchitecture::X86_64,
    )
    .with_pipe_registry(assembly.ipc_pipes().unwrap());
    assert_eq!(adapter.pipe2(0, 4), LinuxResult::Error(Errno::EINVAL));
    assert_eq!(adapter.pipe2(0, 0o40000000), LinuxResult::Error(Errno::ENOSYS));
    assert_eq!(adapter.pipe2(0, 0), LinuxResult::Error(Errno::EFAULT));
    assert_eq!(descriptors.reserve(0).unwrap().number(), 0);
    let catalog = assembly.ipc().unwrap();
    catalog.freeze_checkpoint();
    assert!(catalog.checkpoint_image().unwrap().pipes.is_empty());
    catalog.thaw_checkpoint();
}

#[test]
fn ioctl_owned_commands() {
    let descriptors = Arc::new(DescriptorTable::new(4).unwrap());
    let object = Arc::new(IoctlFile {
        status: Mutex::new(StatusFlags::from_bits(2)),
        size: i32::MAX as u64 + 100,
    });
    let descriptor = descriptors
        .commit(
            descriptors.reserve(0).unwrap(),
            object.clone(),
            StatusFlags::from_bits(2),
            DescriptorFlags::default(),
        )
        .unwrap();
    let adapter = RuntimeFilesystemSyscalls::new(
        descriptors.clone(),
        Memory {
            bytes: Mutex::new(vec![0; 16]),
            fail_write: false,
        },
        GuestArchitecture::Aarch64,
    );

    adapter.memory.bytes.lock().unwrap()[0..4].copy_from_slice(&1_i32.to_le_bytes());
    assert_eq!(adapter.ioctl(descriptor, 0x5421, 0), LinuxResult::Value(0));
    assert_eq!(object.status.lock().unwrap().bits(), 2 | StatusFlags::NONBLOCKING);
    assert_eq!(adapter.ioctl(descriptor, 0x541b, 4), LinuxResult::Value(0));
    assert_eq!(
        i32::from_le_bytes(adapter.memory.bytes.lock().unwrap()[4..8].try_into().unwrap()),
        i32::MAX,
    );
    assert_eq!(adapter.ioctl(descriptor, 0x5451, 0), LinuxResult::Value(0));
    assert_eq!(
        descriptors.flags(descriptor).unwrap().bits(),
        DescriptorFlags::CLOSE_ON_EXEC
    );
    assert_eq!(adapter.ioctl(descriptor, 0x5450, 0), LinuxResult::Value(0));
    assert_eq!(descriptors.flags(descriptor).unwrap().bits(), 0);
}

#[test]
fn ioctl_error_precedence() {
    let descriptors = Arc::new(DescriptorTable::new(2).unwrap());
    let adapter = RuntimeFilesystemSyscalls::new(
        descriptors.clone(),
        Memory {
            bytes: Mutex::new(vec![0; 2]),
            fail_write: false,
        },
        GuestArchitecture::X86_64,
    );
    assert_eq!(adapter.ioctl(9, 0x5421, u64::MAX), LinuxResult::Error(Errno::EBADF));

    let object = Arc::new(IoctlFile {
        status: Mutex::new(StatusFlags::default()),
        size: 1,
    });
    let descriptor = descriptors
        .commit(
            descriptors.reserve(0).unwrap(),
            object,
            StatusFlags::default(),
            DescriptorFlags::default(),
        )
        .unwrap();
    assert_eq!(
        adapter.ioctl(descriptor, 0xdead, u64::MAX),
        LinuxResult::Error(Errno::ENOTTY)
    );
    assert_eq!(adapter.ioctl(descriptor, 0x5421, 0), LinuxResult::Error(Errno::EFAULT));
    assert_eq!(adapter.ioctl(descriptor, 0x541b, 0), LinuxResult::Error(Errno::EFAULT));
}

#[derive(Debug)]
struct NoopTerminalSignal;

impl hl_terminal::SignalSink for NoopTerminalSignal {
    fn publish(
        &self,
        _actor: Option<hl_descriptor::OperationActor>,
        _terminal: hl_terminal::PairId,
        _foreground: Option<hl_terminal::ForegroundGroup>,
        _signal: hl_terminal::Signal,
    ) {
    }
}

fn terminal_ioctl_fixture() -> (
    RuntimeFilesystemSyscalls<Memory>,
    Arc<hl_task::TaskRegistry>,
    hl_task::ProcessId,
    hl_task::ProcessId,
    hl_task::ThreadId,
    Arc<hl_terminal::Catalog>,
    Arc<hl_terminal::Pair>,
    Arc<DescriptorTable>,
    Arc<hl_terminal::Bindings>,
    i32,
) {
    let tasks = Arc::new(hl_task::TaskRegistry::new(hl_task::RegistryConfig::default()).unwrap());
    let credentials = hl_task::ProcessCredentials::new(1000, 1000, &[], 8).unwrap();
    let (_, init_thread) = tasks.create_init(credentials, hl_task::ProcessLimits::empty()).unwrap();
    let leader_plan = tasks.begin_fork_process(init_thread).unwrap();
    let leader = leader_plan.process();
    let leader_thread = leader_plan.thread();
    tasks.commit_fork_process(leader_plan).unwrap();
    let session = tasks.create_session(leader).unwrap();
    tasks.attach_terminal(leader, session).unwrap();
    let worker_plan = tasks.begin_fork_process(leader_thread).unwrap();
    let worker = worker_plan.process();
    let worker_thread = worker_plan.thread();
    tasks.commit_fork_process(worker_plan).unwrap();
    let foreground = tasks.set_process_group(leader, worker, None).unwrap();
    tasks.set_foreground_group(leader, foreground).unwrap();
    for signal in [1, 18, 28] {
        tasks
            .set_action(
                worker,
                hl_task::SignalNumber::new(signal).unwrap(),
                hl_task::SignalAction {
                    disposition: hl_task::SignalDisposition::Handler(0x4000),
                    ..hl_task::SignalAction::DEFAULT
                },
            )
            .unwrap();
    }

    let catalog = Arc::new(hl_terminal::Catalog::default());
    let pair = catalog.allocate().unwrap();
    let bindings = Arc::new(hl_terminal::Bindings::default());
    let description = Arc::new(hl_terminal::Description::new(
        Arc::clone(&pair),
        hl_terminal::Endpoint::Slave,
        Arc::downgrade(&catalog),
        Arc::new(NoopTerminalSignal),
    ));
    let descriptors = Arc::new(DescriptorTable::new(4).unwrap());
    let descriptor = descriptors
        .commit(
            descriptors.reserve(0).unwrap(),
            description.clone(),
            StatusFlags::default(),
            DescriptorFlags::default(),
        )
        .unwrap();
    let identity = descriptors.pin(descriptor).unwrap().description_identity();
    description.bind(identity, &bindings);
    catalog.acquire(session.number(), pair.id()).unwrap();
    let (_, generation) = foreground.wire_parts();
    pair.set_foreground(hl_terminal::ForegroundGroup {
        number: foreground.number(),
        generation,
    })
    .unwrap();
    let adapter = RuntimeFilesystemSyscalls::new(
        Arc::clone(&descriptors),
        Memory {
            bytes: Mutex::new(vec![0; 16]),
            fail_write: false,
        },
        GuestArchitecture::X86_64,
    )
    .with_terminals(Arc::clone(&bindings))
    .with_terminal_tasks(Arc::clone(&tasks), leader);
    (
        adapter,
        tasks,
        leader,
        worker,
        worker_thread,
        catalog,
        pair,
        descriptors,
        bindings,
        descriptor,
    )
}

#[test]
fn tiocnotty_raced_detach_failure_does_not_publish_prepared_signals() {
    let (_adapter, tasks, leader, _worker, worker_thread, _catalog, _pair, _descriptors, _bindings, _descriptor) =
        terminal_ioctl_fixture();
    let prepared = tasks
        .prepare_terminal_transition(leader, hl_task::TerminalTransition::Detach)
        .unwrap();
    assert_eq!(
        crate::filesystem::ioctl::finish_terminal_detach(Some(prepared), Err(hl_terminal::CatalogError::NotFound),),
        LinuxResult::Error(Errno::ENOTTY)
    );
    assert_eq!(tasks.pending_signal_mask(worker_thread).unwrap().bits(), 0);
    assert!(tasks.terminal_session(leader).unwrap().is_some());
}

#[test]
fn tiocnotty_and_tiocswinsz_publish_only_after_terminal_mutation() {
    let (adapter, tasks, leader, _worker, worker_thread, catalog, pair, _descriptors, _bindings, descriptor) =
        terminal_ioctl_fixture();
    adapter.memory.bytes.lock().unwrap()[..8].copy_from_slice(&[24, 0, 80, 0, 1, 0, 2, 0]);

    assert_eq!(adapter.ioctl(descriptor, 0x5414, 0), LinuxResult::Value(0));
    assert_eq!(pair.window().rows, 24);
    assert_eq!(pair.window().columns, 80);
    assert_ne!(tasks.pending_signal_mask(worker_thread).unwrap().bits() & (1 << 27), 0);

    assert_eq!(adapter.ioctl(descriptor, 0x5422, 0), LinuxResult::Value(0));
    assert!(catalog.controlling(tasks.session_id(leader).unwrap().number()).is_err());
    let pending = tasks.pending_signal_mask(worker_thread).unwrap().bits();
    assert_ne!(pending & 1, 0);
    assert_ne!(pending & (1 << 17), 0);
}

#[test]
fn tiocnotty_nonleader_preserves_session_terminal_binding() {
    let (adapter, tasks, leader, worker, worker_thread, catalog, pair, _descriptors, _bindings, descriptor) =
        terminal_ioctl_fixture();
    let session = tasks.session_id(leader).unwrap();
    let adapter = adapter.with_terminal_tasks(Arc::clone(&tasks), worker);
    let foreground = pair.foreground().unwrap();
    pair.set_foreground(hl_terminal::ForegroundGroup {
        number: foreground.number,
        generation: foreground.generation.saturating_add(1),
    })
    .unwrap();

    assert_eq!(adapter.ioctl(descriptor, 0x5422, 0), LinuxResult::Value(0));
    assert!(catalog.controlling(session.number()).is_ok());
    assert_eq!(tasks.terminal_session(worker).unwrap(), None);
    assert_eq!(tasks.terminal_session(leader).unwrap(), Some(session));
    assert_eq!(tasks.pending_signal_mask(worker_thread).unwrap().bits(), 0);
}

#[test]
fn tiocnotty_leader_detaches_when_tty_foreground_is_stale() {
    let (adapter, tasks, leader, _worker, worker_thread, catalog, pair, _descriptors, _bindings, descriptor) =
        terminal_ioctl_fixture();
    let session = tasks.session_id(leader).unwrap();
    let foreground = pair.foreground().unwrap();
    pair.set_foreground(hl_terminal::ForegroundGroup {
        number: foreground.number,
        generation: foreground.generation.saturating_add(1),
    })
    .unwrap();

    assert_eq!(adapter.ioctl(descriptor, 0x5422, 0), LinuxResult::Value(0));
    assert!(catalog.controlling(session.number()).is_err());
    assert_eq!(tasks.pending_signal_mask(worker_thread).unwrap().bits(), 0);
}

#[test]
fn tiocsctty_attach_failure_rolls_back_new_catalog_binding() {
    let (adapter, tasks, leader, _worker, _worker_thread, catalog, pair, _descriptors, _bindings, descriptor) =
        terminal_ioctl_fixture();
    let session = tasks.session_id(leader).unwrap();
    catalog.detach(session.number(), pair.id()).unwrap();
    let leader_thread = tasks
        .snapshot()
        .processes
        .iter()
        .find(|process| process.id == leader)
        .unwrap()
        .leader;
    let _exec = tasks.prepare_exec(leader, leader_thread).unwrap();

    assert_eq!(adapter.ioctl(descriptor, 0x540e, 0), LinuxResult::Error(Errno::EPERM));
    assert!(catalog.controlling(session.number()).is_err());
}

#[test]
fn master_window_change_targets_tty_foreground_not_callers_session() {
    let (adapter, tasks, leader, worker, worker_thread, catalog, pair, descriptors, bindings, _descriptor) =
        terminal_ioctl_fixture();
    let master = Arc::new(hl_terminal::Description::new(
        Arc::clone(&pair),
        hl_terminal::Endpoint::Master,
        Arc::downgrade(&catalog),
        Arc::new(NoopTerminalSignal),
    ));
    let descriptor = descriptors
        .commit(
            descriptors.reserve(1).unwrap(),
            master.clone(),
            StatusFlags::default(),
            DescriptorFlags::default(),
        )
        .unwrap();
    master.bind(descriptors.pin(descriptor).unwrap().description_identity(), &bindings);

    assert_eq!(adapter.ioctl(descriptor, 0x5422, 0), LinuxResult::Error(Errno::ENOTTY));
    // The default zero-sized window is unchanged and must not emit SIGWINCH.
    assert_eq!(adapter.ioctl(descriptor, 0x5414, 0), LinuxResult::Value(0));
    assert_eq!(tasks.pending_signal_mask(worker_thread).unwrap().bits(), 0);

    // Diverge the task session's foreground from the tty to prove the tty's
    // generation-qualified identity is authoritative.
    let peer_plan = tasks.begin_fork_process(worker_thread).unwrap();
    let peer = peer_plan.process();
    let peer_thread = peer_plan.thread();
    tasks.commit_fork_process(peer_plan).unwrap();
    let peer_group = tasks.set_process_group(worker, peer, None).unwrap();
    tasks.set_foreground_group(leader, peer_group).unwrap();
    tasks
        .set_action(
            peer,
            hl_task::SignalNumber::new(28).unwrap(),
            hl_task::SignalAction {
                disposition: hl_task::SignalDisposition::Handler(0x5000),
                ..hl_task::SignalAction::DEFAULT
            },
        )
        .unwrap();

    let other_plan = tasks.begin_fork_process(peer_thread).unwrap();
    let other = other_plan.process();
    tasks.commit_fork_process(other_plan).unwrap();
    tasks.create_session(other).unwrap();
    let adapter = adapter.with_terminal_tasks(Arc::clone(&tasks), other);
    adapter.memory.bytes.lock().unwrap()[..8].copy_from_slice(&[40, 0, 120, 0, 0, 0, 0, 0]);

    assert_eq!(adapter.ioctl(descriptor, 0x5414, 0), LinuxResult::Value(0));
    assert_eq!(pair.window().rows, 40);
    assert_ne!(tasks.pending_signal_mask(worker_thread).unwrap().bits() & (1 << 27), 0);
    assert_eq!(tasks.pending_signal_mask(peer_thread).unwrap().bits(), 0);
}

#[test]
fn ioctl_fionread_reports_pipe_buffer() {
    let descriptors = Arc::new(DescriptorTable::new(2).unwrap());
    let adapter = RuntimeFilesystemSyscalls::new(
        descriptors,
        Memory {
            bytes: Mutex::new(vec![0; 16]),
            fail_write: false,
        },
        GuestArchitecture::X86_64,
    );
    assert_eq!(adapter.pipe2(0, 0), LinuxResult::Value(0));
    adapter.memory.bytes.lock().unwrap()[..5].copy_from_slice(b"hello");
    assert_eq!(adapter.write(1, 0, 5), LinuxResult::Value(5));
    assert_eq!(adapter.ioctl(0, 0x541b, 8), LinuxResult::Value(0));
    assert_eq!(
        i32::from_le_bytes(adapter.memory.bytes.lock().unwrap()[8..12].try_into().unwrap()),
        5,
    );
}

#[test]
fn ioctl_fionread_reports_registered_pipe_buffer() {
    let (adapter, reader, descriptor) = registered_pipe_adapter(vec![0; 32], GuestArchitecture::Aarch64);
    adapter.memory.bytes.lock().unwrap()[8..13].copy_from_slice(b"hello");
    assert_eq!(adapter.write(descriptor, 8, 5), LinuxResult::Value(5));
    assert_eq!(reader.probe_read(usize::MAX), Ok(Some(5)));
    assert_eq!(adapter.ioctl(0, 0x541b, 16), LinuxResult::Value(0));
    assert_eq!(
        i32::from_le_bytes(adapter.memory.bytes.lock().unwrap()[16..20].try_into().unwrap()),
        5,
    );
}

#[test]
fn pipe_returns_epipe() {
    let descriptors = Arc::new(DescriptorTable::new(2).unwrap());
    let signal = Arc::new(PipeSignal(AtomicBool::new(false)));
    let adapter = RuntimeFilesystemSyscalls::new(
        descriptors.clone(),
        Memory {
            bytes: Mutex::new(vec![1; 8]),
            fail_write: false,
        },
        GuestArchitecture::Aarch64,
    )
    .with_pipe_signal(signal.clone());
    assert_eq!(adapter.pipe2(0, 0), LinuxResult::Value(0));
    descriptors.close(0).unwrap();
    assert_eq!(adapter.write(1, 0, 1), LinuxResult::Error(Errno::EPIPE));
    assert!(signal.0.load(Ordering::Acquire));
}

#[test]
fn pipe_position_rejected() {
    for architecture in [GuestArchitecture::Aarch64, GuestArchitecture::X86_64] {
        let descriptors = Arc::new(DescriptorTable::new(2).unwrap());
        let adapter = RuntimeFilesystemSyscalls::new(
            descriptors,
            Memory {
                bytes: Mutex::new(vec![1; 8]),
                fail_write: false,
            },
            architecture,
        );
        assert_eq!(adapter.pipe2(0, 0), LinuxResult::Value(0));
        assert_eq!(adapter.seek(0, 0, 0), LinuxResult::Error(Errno::ESPIPE));
        assert_eq!(
            adapter.positional_io(0, 0, 1, 0, true),
            LinuxResult::Error(Errno::ESPIPE)
        );
        assert_eq!(
            adapter.positional_io(1, 0, 1, 0, false),
            LinuxResult::Error(Errno::ESPIPE)
        );
    }
}

#[test]
fn positional_mode_precedes_buffer() {
    let descriptors = Arc::new(DescriptorTable::new(2).unwrap());
    let readonly = descriptors
        .commit(
            descriptors.reserve(0).unwrap(),
            Arc::new(RegularFile),
            StatusFlags::default(),
            DescriptorFlags::default(),
        )
        .unwrap();
    let writeonly = descriptors
        .commit(
            descriptors.reserve(0).unwrap(),
            Arc::new(RegularFile),
            StatusFlags::from_bits(1),
            DescriptorFlags::default(),
        )
        .unwrap();
    let adapter = RuntimeFilesystemSyscalls::new(
        descriptors,
        Memory {
            bytes: Mutex::new(Vec::new()),
            fail_write: true,
        },
        GuestArchitecture::Aarch64,
    );
    assert_eq!(
        adapter.positional_io(readonly, 0, 1, 0, false),
        LinuxResult::Error(Errno::EBADF)
    );
    assert_eq!(
        adapter.positional_io(writeonly, 0, 1, 0, true),
        LinuxResult::Error(Errno::EBADF)
    );
}

#[test]
fn pipe_unsafe_shrink() {
    let descriptors = Arc::new(DescriptorTable::new(2).unwrap());
    let adapter = RuntimeFilesystemSyscalls::new(
        descriptors,
        Memory {
            bytes: Mutex::new(vec![1; 8192]),
            fail_write: false,
        },
        GuestArchitecture::Aarch64,
    );
    assert_eq!(adapter.pipe2(0, 0o4000), LinuxResult::Value(0));
    assert_eq!(adapter.fcntl(1, 1032, 0), LinuxResult::Value(65_536));
    assert_eq!(adapter.fcntl(1, 1031, 4097), LinuxResult::Value(8192));
    assert_eq!(adapter.write(1, 0, 5000), LinuxResult::Value(5000));
    assert_eq!(adapter.fcntl(0, 1031, 4096), LinuxResult::Error(Errno::EBUSY));
    assert_eq!(adapter.fcntl(0, 1032, 0), LinuxResult::Value(8192));
    assert_eq!(adapter.fcntl(0, 1031, u64::MAX), LinuxResult::Error(Errno::EINVAL));
    assert_eq!(adapter.fcntl(0, u32::MAX, 0), LinuxResult::Error(Errno::EINVAL));
}

#[test]
fn blocked_runtime_interruption() {
    let descriptors = Arc::new(DescriptorTable::new(2).unwrap());
    let interruption = Arc::new(hl_sync::Interruption::new());
    let cancellation = Arc::new(crate::RuntimePipeCancellation::new(interruption.clone()));
    let adapter = RuntimeFilesystemSyscalls::new(
        descriptors,
        Memory {
            bytes: Mutex::new(vec![0; 8]),
            fail_write: false,
        },
        GuestArchitecture::Aarch64,
    )
    .with_pipe_cancellation(cancellation);
    assert_eq!(adapter.pipe2(0, 0), LinuxResult::Value(0));
    let blocked = std::thread::spawn(move || adapter.read(0, 0, 1));
    std::thread::sleep(std::time::Duration::from_millis(10));
    interruption.interrupt();
    assert_eq!(blocked.join().unwrap(), LinuxResult::Error(Errno::EINTR));
}

#[test]
fn packet_discard_tails() {
    let adapter = RuntimeFilesystemSyscalls::new(
        Arc::new(DescriptorTable::new(2).unwrap()),
        Memory {
            bytes: Mutex::new(vec![0; 32]),
            fail_write: false,
        },
        GuestArchitecture::X86_64,
    );
    assert_eq!(adapter.pipe2(0, 0o40000), LinuxResult::Value(0));
    adapter.memory.bytes.lock().unwrap()[8..16].copy_from_slice(b"abc12345");
    assert_eq!(adapter.write(1, 8, 3), LinuxResult::Value(3));
    assert_eq!(adapter.write(1, 11, 5), LinuxResult::Value(5));
    assert_eq!(adapter.read(0, 16, 2), LinuxResult::Value(2));
    assert_eq!(adapter.read(0, 18, 8), LinuxResult::Value(5));
    assert_eq!(&adapter.memory.bytes.lock().unwrap()[16..25], b"ab12345\0\0");
}

#[test]
fn interrupted_partial_progress() {
    let interruption = Arc::new(hl_sync::Interruption::new());
    let adapter = RuntimeFilesystemSyscalls::new(
        Arc::new(DescriptorTable::new(2).unwrap()),
        Memory {
            bytes: Mutex::new(vec![1; 8192]),
            fail_write: false,
        },
        GuestArchitecture::Aarch64,
    )
    .with_pipe_cancellation(Arc::new(crate::RuntimePipeCancellation::new(interruption.clone())));
    assert_eq!(adapter.pipe2(0, 0), LinuxResult::Value(0));
    assert_eq!(adapter.fcntl(1, 1031, 4096), LinuxResult::Value(4096));
    let blocked = std::thread::spawn(move || adapter.write(1, 0, 8192));
    std::thread::sleep(std::time::Duration::from_millis(10));
    interruption.interrupt();
    assert_eq!(blocked.join().unwrap(), LinuxResult::Value(4096));
}
