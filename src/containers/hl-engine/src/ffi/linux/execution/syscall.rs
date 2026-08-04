//! Production syscall-family composition.

use std::sync::{Arc, Mutex};

use hl_descriptor::{DescriptorFlags, ExactDuplicate};
use hl_runtime::{RuntimeEventSyscalls, RuntimeFilesystemSyscalls, RuntimeMemorySyscalls};

use super::{MappingHostAdapter, descriptor, ports, process_memory::ProcessMemory, readiness};

pub(super) struct Unshare {
    process: std::sync::Weak<super::routing::ProcessContext>,
    thread: hl_task::ThreadId,
    cancellation: Arc<readiness::Cancellation>,
}

impl Unshare {
    pub(super) fn new(
        process: std::sync::Weak<super::routing::ProcessContext>,
        thread: hl_task::ThreadId,
        cancellation: Arc<readiness::Cancellation>,
    ) -> Self {
        Self {
            process,
            thread,
            cancellation,
        }
    }

    fn publish(&self, first: u32, last: u32, close_on_exec: bool) -> Result<(), hl_runtime::ControlError> {
        self.process
            .upgrade()
            .ok_or(hl_runtime::ControlError::Descriptor(
                hl_descriptor::DescriptorError::Corrupt,
            ))?
            .publish_unshare(self.thread, Arc::clone(&self.cancellation), first, last, close_on_exec)
    }
}

pub(super) type MemoryRuntime = RuntimeMemorySyscalls<MappingHostAdapter, ProcessMemory>;
pub(super) type FilesystemRuntime = RuntimeFilesystemSyscalls<ProcessMemory>;

pub(super) struct MemoryPort(pub(super) Arc<Mutex<MemoryRuntime>>);
pub(super) struct FilesystemPort(pub(super) Arc<Mutex<FilesystemRuntime>>);

impl hl_linux::FilesystemSyscalls for FilesystemPort {
    fn may_block(&self, operation: hl_linux::SyscallOperation, arguments: [u64; 6]) -> bool {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .may_block(operation, arguments)
    }

    fn handle(&mut self, operation: hl_linux::SyscallOperation, arguments: [u64; 6]) -> hl_linux::LinuxResult {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .handle(operation, arguments)
    }
}

pub(super) struct DescriptorPort {
    pub(super) standard: ports::DescriptorPort,
    pub(super) filesystem: Arc<Mutex<FilesystemRuntime>>,
    pub(super) epoll: Arc<hl_runtime::Control>,
    pub(super) epoll_table: Arc<hl_runtime::RuntimeDescriptorTable>,
    pub(super) unshare: Arc<Unshare>,
    pub(super) locks: Arc<hl_runtime::AdvisoryLockCoordinator>,
    pub(super) process: hl_task::ProcessId,
}

impl hl_linux::DescriptorIoSyscalls for DescriptorPort {
    fn may_block(&self, operation: hl_linux::SyscallOperation, arguments: [u64; 6]) -> bool {
        self.filesystem
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .may_block(operation, arguments)
    }

    fn handle(&mut self, operation: hl_linux::SyscallOperation, arguments: [u64; 6]) -> hl_linux::LinuxResult {
        if operation.name == "close" {
            return self.close(arguments[0] as i32);
        }
        if operation.name == "close_range" {
            return self.close_range(arguments[0], arguments[1], arguments[2]);
        }
        if operation.name == "dup" {
            return self.duplicate(arguments[0] as i32, 0, DescriptorFlags::default());
        }
        if operation.name == "dup2" {
            return self.duplicate_exact(arguments[0] as i32, arguments[1] as i32, ExactDuplicate::Dup2);
        }
        if operation.name == "dup3" {
            let flags = arguments[2] as u32;
            if flags & !0o2000000 != 0 {
                return hl_linux::LinuxResult::Error(hl_linux::Errno::EINVAL);
            }
            let local = DescriptorFlags::from_bits(if flags == 0 { 0 } else { DescriptorFlags::CLOSE_ON_EXEC });
            return self.duplicate_exact(arguments[0] as i32, arguments[1] as i32, ExactDuplicate::Dup3(local));
        }
        if operation.name == "fcntl" && matches!(arguments[1], 0 | 1030) {
            let flags = if arguments[1] == 1030 {
                DescriptorFlags::from_bits(DescriptorFlags::CLOSE_ON_EXEC)
            } else {
                DescriptorFlags::default()
            };
            return self.duplicate(arguments[0] as i32, arguments[2] as i32, flags);
        }
        if matches!(
            operation.name,
            "pipe2"
                | "copy_file_range"
                | "sendfile"
                | "splice"
                | "tee"
                | "vmsplice"
                | "pread64"
                | "preadv"
                | "preadv2"
                | "pwrite64"
                | "pwritev"
                | "pwritev2"
                | "readahead"
                | "read"
                | "write"
                | "readv"
                | "writev"
                | "fcntl"
                | "fsync"
                | "fdatasync"
                | "sync_file_range"
                | "syncfs"
                | "ioctl"
                | "lseek"
        ) {
            self.filesystem
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .handle(operation, arguments)
        } else {
            self.standard.handle(operation, arguments)
        }
    }
}

impl DescriptorPort {
    fn control_result(result: Result<i32, hl_runtime::ControlError>) -> hl_linux::LinuxResult {
        match result {
            Ok(number) => hl_linux::LinuxResult::Value(number as u64),
            Err(hl_runtime::ControlError::Descriptor(error)) => {
                hl_linux::LinuxResult::Error(descriptor::Set::errno(error))
            }
            Err(_) => hl_linux::LinuxResult::Error(hl_linux::Errno::EINVAL),
        }
    }

    fn close(&self, number: i32) -> hl_linux::LinuxResult {
        let locked_file = self
            .epoll_table
            .descriptor_table()
            .pin(number)
            .ok()
            .and_then(|lease| lease.metadata().ok())
            .map(|metadata| hl_runtime::FileIdentity {
                device: metadata.device,
                inode: metadata.inode,
            });
        match self.epoll.close(&self.epoll_table, number) {
            Ok(()) => {
                if let Some(file) = locked_file {
                    let (identity, generation) = self.process.wire_parts();
                    let _ = self.locks.close_process_file(
                        file,
                        hl_runtime::ProcessLockOwner {
                            identity: u64::from(identity),
                            generation: u32::from(generation),
                        },
                    );
                }
                hl_linux::LinuxResult::Value(0)
            }
            Err(hl_runtime::ControlError::Descriptor(error)) => {
                hl_linux::LinuxResult::Error(descriptor::Set::errno(error))
            }
            Err(_) => hl_linux::LinuxResult::Error(hl_linux::Errno::EINVAL),
        }
    }

    fn duplicate(&self, source: i32, minimum: i32, flags: DescriptorFlags) -> hl_linux::LinuxResult {
        Self::control_result(self.epoll.duplicate(&self.epoll_table, source, minimum, flags))
    }

    fn duplicate_exact(&self, source: i32, destination: i32, operation: ExactDuplicate) -> hl_linux::LinuxResult {
        Self::control_result(
            self.epoll
                .duplicate_exact(&self.epoll_table, source, destination, operation),
        )
    }

    fn close_range(&self, first: u64, last: u64, flags: u64) -> hl_linux::LinuxResult {
        const UNSHARE: u64 = 2;
        const CLOEXEC: u64 = 4;
        if flags & !(UNSHARE | CLOEXEC) != 0 || first > last || first > u64::from(u32::MAX) {
            return hl_linux::LinuxResult::Error(hl_linux::Errno::EINVAL);
        }
        let last = last.min(u64::from(u32::MAX));
        let result = if flags & UNSHARE != 0 {
            self.unshare.publish(first as u32, last as u32, flags & CLOEXEC != 0)
        } else {
            self.epoll
                .close_range(&self.epoll_table, first as u32, last as u32, flags & CLOEXEC != 0)
        };
        match result {
            Ok(()) => hl_linux::LinuxResult::Value(0),
            Err(hl_runtime::ControlError::Capacity) => hl_linux::LinuxResult::Error(hl_linux::Errno::ENOMEM),
            Err(hl_runtime::ControlError::Descriptor(error)) => {
                hl_linux::LinuxResult::Error(descriptor::Set::errno(error))
            }
            Err(_) => hl_linux::LinuxResult::Error(hl_linux::Errno::EINVAL),
        }
    }
}

impl hl_linux::MemorySyscalls for MemoryPort {
    fn handle(&mut self, operation: hl_linux::SyscallOperation, arguments: [u64; 6]) -> hl_linux::LinuxResult {
        self.0
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .handle(operation, arguments)
    }
}

pub(super) struct EventPort {
    pub(super) readiness: readiness::EventPort,
    pub(super) objects: RuntimeEventSyscalls<ProcessMemory>,
}

impl hl_linux::EventSyscalls for EventPort {
    fn handle(&mut self, operation: hl_linux::SyscallOperation, arguments: [u64; 6]) -> hl_linux::LinuxResult {
        if matches!(operation.name, "poll" | "ppoll" | "pselect6" | "select") {
            self.readiness.handle(operation, arguments)
        } else {
            self.objects.handle(operation, arguments)
        }
    }
}
