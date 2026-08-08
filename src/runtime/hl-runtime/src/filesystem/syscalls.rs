use super::errno::FileErrno;
use crate::RuntimePathHost;
use hl_descriptor::{DescriptorError, DescriptorFlags, DescriptorTable, ExactDuplicate, ObjectError};
use hl_linux::{
    DescriptorIoSyscalls, Errno, FilesystemAbi, FilesystemSyscalls, GuestArchitecture, GuestMarshaller, GuestMemory,
    LinuxResult, SyscallOperation,
};
use std::sync::Arc;

use super::VectorTerminal;
pub struct RuntimeFilesystemSyscalls<M: GuestMemory> {
    pub(super) descriptors: Arc<DescriptorTable>,
    pub(super) memory: M,
    pub(super) architecture: GuestArchitecture,
    pub(super) path_host: Option<Arc<dyn RuntimePathHost>>,
    pub(super) memfds: Option<Arc<crate::MemfdRegistry>>,
    pub(super) pipe_signal: Option<Arc<dyn crate::PipeSignalPort>>,
    pub(super) file_size_limit: Option<Arc<dyn crate::FileSizeLimitPort>>,
    pub(super) async_signal: Option<Arc<dyn crate::AsyncSignalPort>>,
    pub(super) dnotify: Option<Arc<dyn crate::DnotifyPort>>,
    pub(super) pipe_cancellation: Option<Arc<dyn crate::PipeCancellationPort>>,
    pub(super) pipe_registry: Option<Arc<crate::IpcPipeRegistry>>,
    pub(super) backing_changes: Option<Arc<dyn crate::BackingChangePort>>,
    pub(super) socket_ioctl: Option<Arc<dyn crate::SocketIoctlPort>>,
    pub(super) vector_terminal: Option<Arc<dyn VectorTerminal>>,
    pub(super) actor: Option<hl_descriptor::OperationActor>,
    pub(super) locks: Option<Arc<crate::AdvisoryLockCoordinator>>,
    pub(super) working: Arc<crate::WorkingDirectory>,
    pub(super) terminals: Option<Arc<hl_terminal::Bindings>>,
    pub(super) terminal_tasks: Option<(Arc<hl_task::TaskRegistry>, hl_task::ProcessId)>,
    pub(super) fs_context: Arc<crate::FsContext>,
    pub(super) unix_socket_paths: Option<Arc<dyn crate::UnixSocketPathPort>>,
}
impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub fn with_terminals(mut self, bindings: Arc<hl_terminal::Bindings>) -> Self {
        self.terminals = Some(bindings);
        self
    }
    pub fn with_terminal_tasks(mut self, tasks: Arc<hl_task::TaskRegistry>, process: hl_task::ProcessId) -> Self {
        self.terminal_tasks = Some((tasks, process));
        self
    }
    pub fn with_path_host(mut self, host: Arc<dyn RuntimePathHost>) -> Self {
        self.path_host = Some(host);
        self
    }
    pub fn with_unix_socket_paths(mut self, paths: Arc<dyn crate::UnixSocketPathPort>) -> Self {
        self.unix_socket_paths = Some(paths);
        self
    }
    pub fn with_memfds(mut self, registry: Arc<crate::MemfdRegistry>) -> Self {
        self.memfds = Some(registry);
        self
    }
    pub fn with_pipe_signal(mut self, signal: Arc<dyn crate::PipeSignalPort>) -> Self {
        self.pipe_signal = Some(signal);
        self
    }
    pub fn with_file_size_limit(mut self, limit: Arc<dyn crate::FileSizeLimitPort>) -> Self {
        self.file_size_limit = Some(limit);
        self
    }
    pub fn with_async_signal(mut self, signal: Arc<dyn crate::AsyncSignalPort>) -> Self {
        self.async_signal = Some(signal);
        self
    }
    pub fn with_dnotify(mut self, port: Arc<dyn crate::DnotifyPort>) -> Self {
        self.dnotify = Some(port);
        self
    }
    pub fn with_pipe_registry(mut self, registry: Arc<crate::IpcPipeRegistry>) -> Self {
        self.pipe_registry = Some(registry);
        self
    }
    pub fn with_pipe_cancellation(mut self, cancellation: Arc<dyn crate::PipeCancellationPort>) -> Self {
        self.pipe_cancellation = Some(cancellation);
        self
    }
    pub fn with_backing_changes(mut self, changes: Arc<dyn crate::BackingChangePort>) -> Self {
        self.backing_changes = Some(changes);
        self
    }
    pub fn with_socket_ioctl(mut self, ioctl: Arc<dyn crate::SocketIoctlPort>) -> Self {
        self.socket_ioctl = Some(ioctl);
        self
    }
    pub fn with_vector_terminal(mut self, terminal: Arc<dyn VectorTerminal>) -> Self {
        self.vector_terminal = Some(terminal);
        self
    }
    pub fn with_actor(mut self, process: hl_task::ProcessId, thread: hl_task::ThreadId) -> Self {
        let (process_slot, process_generation) = process.wire_parts();
        let (thread_slot, thread_generation) = thread.wire_parts();
        self.actor = Some(hl_descriptor::OperationActor {
            process: process_slot,
            process_generation,
            thread: thread_slot,
            thread_generation,
        });
        self
    }
    pub fn with_advisory_locks(mut self, locks: Arc<crate::AdvisoryLockCoordinator>) -> Self {
        self.locks = Some(locks);
        self
    }
    pub fn with_working_directory(mut self, working: Arc<crate::WorkingDirectory>) -> Self {
        self.working = working;
        self
    }
    pub(super) fn descriptor_result(result: Result<i32, DescriptorError>) -> LinuxResult {
        match result {
            Ok(number) => LinuxResult::Value(number as u64),
            Err(error) => LinuxResult::Error(FileErrno::descriptor(error)),
        }
    }
    fn descriptor_io(&self, operation: SyscallOperation, arguments: [u64; 6]) -> LinuxResult {
        let descriptor = arguments[0] as i32;
        match operation.name {
            // Legacy x86-64 `pipe` is `pipe2` with no flags.
            "pipe" => self.pipe2(arguments[0], 0),
            "pipe2" => self.pipe2(arguments[0], arguments[1] as u32),
            "read" => self.read(descriptor, arguments[1], arguments[2]),
            "write" => self.write(descriptor, arguments[1], arguments[2]),
            "readv" => self.vector_io(descriptor, arguments[1], arguments[2], true),
            "writev" => self.vector_io(descriptor, arguments[1], arguments[2], false),
            "vmsplice" => self.vmsplice(arguments),
            "splice" => self.splice(arguments),
            "copy_file_range" => self.copy_file_range(arguments),
            "sendfile" => self.sendfile(arguments),
            "tee" => self.tee(arguments),
            "pread64" => self.positional_io(descriptor, arguments[1], arguments[2], arguments[3], true),
            "pwrite64" => self.positional_io(descriptor, arguments[1], arguments[2], arguments[3], false),
            "preadv" | "pwritev" => self.positional_vector(
                descriptor,
                arguments[1],
                arguments[2],
                if self.architecture == GuestArchitecture::X86_64 {
                    arguments[3]
                } else {
                    Self::split_offset(arguments[3], arguments[4])
                },
                operation.name == "preadv",
                None,
            ),
            "preadv2" | "pwritev2" => self.positional_vector(
                descriptor,
                arguments[1],
                arguments[2],
                if self.architecture == GuestArchitecture::X86_64 {
                    arguments[3]
                } else {
                    Self::split_offset(arguments[3], arguments[4])
                },
                operation.name == "preadv2",
                Some(arguments[5]),
            ),
            "lseek" => self.seek(descriptor, arguments[1], arguments[2] as u32),
            "close" => {
                let closed = self.descriptors.close(descriptor);
                hl_log::hl_debug!(
                    hl_log::tag::FS,
                    "close descriptor={descriptor} closed={}",
                    closed.is_ok()
                );
                match closed {
                    Ok(()) => LinuxResult::Value(0),
                    Err(error) => LinuxResult::Error(FileErrno::descriptor(error)),
                }
            }
            "dup" => {
                let result =
                    Self::descriptor_result(self.descriptors.duplicate(descriptor, 0, DescriptorFlags::default()));
                hl_log::hl_debug!(
                    hl_log::tag::FS,
                    "dup descriptor={descriptor} result={:#x}",
                    result.encode(),
                );
                result
            }
            "dup3" => {
                let flags = arguments[2] as u32;
                if flags & !0o2_000_000 != 0 {
                    return LinuxResult::Error(Errno::EINVAL);
                }
                let local = DescriptorFlags::from_bits(if flags == 0 { 0 } else { DescriptorFlags::CLOSE_ON_EXEC });
                let result = Self::descriptor_result(self.descriptors.duplicate_exact(
                    descriptor,
                    arguments[1] as i32,
                    ExactDuplicate::Dup3(local),
                ));
                hl_log::hl_debug!(
                    hl_log::tag::FS,
                    "dup3 descriptor={descriptor} target={} flags={flags:#x} result={:#x}",
                    arguments[1] as i32,
                    result.encode(),
                );
                result
            }
            "fcntl" => self.fcntl(descriptor, arguments[1] as u32, arguments[2]),
            "ioctl" => self.ioctl(descriptor, arguments[1] as u32, arguments[2]),
            "ftruncate" => self.ftruncate(descriptor, arguments[1]),
            "fsync" => self.synchronize(descriptor, false),
            "fdatasync" => self.synchronize(descriptor, true),
            "sync_file_range" => self.synchronize(descriptor, false),
            "syncfs" => self.synchronize(descriptor, false),
            "readahead" => self.readahead(descriptor, arguments[1]),
            _ => LinuxResult::Error(Errno::ENOSYS),
        }
    }

    pub(super) fn write(&self, descriptor: i32, address: u64, raw_length: u64) -> LinuxResult {
        let Ok(length) = usize::try_from(raw_length) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        if raw_length > i32::MAX as u64 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        if Self::access_rejects(&lease, false) {
            return LinuxResult::Error(Errno::EBADF);
        }
        if length == 0 {
            return LinuxResult::Value(0);
        }
        if let Some(result) = self.terminal_access(&lease, super::job::TerminalAccess::Write) {
            return result;
        }
        let length = match self.limit_write(&lease, None, length) {
            Ok(length) => length,
            Err(error) => return error,
        };
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let mut bytes = vec![0; length];
        let progress = marshaller.copy_from(address, &mut bytes);
        if length <= lease.atomic_write_limit().unwrap_or(0) && progress.fault.is_some() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        if progress.copied == 0 && progress.fault.is_some() {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let cancellation = self.pipe_cancellation.as_ref().map(|port| port.observation());
        let context = hl_descriptor::OperationContext {
            actor: self.actor,
            cancellation,
        };
        let result = lease.write_context(&bytes[..progress.copied], context);
        match result {
            Ok(count) => LinuxResult::Value(count.min(progress.copied) as u64),
            Err(ObjectError::BrokenPipe) => {
                if let Some(signal) = &self.pipe_signal {
                    let _ = signal.queue_sigpipe();
                }
                LinuxResult::Error(Errno::EPIPE)
            }
            Err(error) => LinuxResult::Error(FileErrno::object(error)),
        }
    }

    pub(super) fn limit_write(
        &self,
        lease: &hl_descriptor::OperationLease,
        position: Option<u64>,
        count: usize,
    ) -> Result<usize, LinuxResult> {
        let Some(port) = &self.file_size_limit else {
            return Ok(count);
        };
        let limit = port.soft_limit().map_err(|()| LinuxResult::Error(Errno::EIO))?;
        if limit == u64::MAX || count == 0 {
            return Ok(count);
        }
        let metadata = lease
            .metadata()
            .map_err(|error| LinuxResult::Error(FileErrno::object(error)))?;
        if metadata.kind != 8 {
            return Ok(count);
        }
        let position = match position {
            Some(position) => position,
            None if lease.status().bits() & hl_descriptor::StatusFlags::APPEND != 0 => metadata.size,
            None => lease
                .seek(hl_descriptor::SeekPosition::Current(0))
                .map_err(|error| LinuxResult::Error(FileErrno::object(error)))?,
        };
        if position >= limit {
            let _ = port.queue_sigxfsz();
            return Err(LinuxResult::Error(Errno::EFBIG));
        }
        Ok(count.min(usize::try_from(limit - position).unwrap_or(usize::MAX)))
    }
}
impl<M: GuestMemory> DescriptorIoSyscalls for RuntimeFilesystemSyscalls<M> {
    fn may_block(&self, operation: SyscallOperation, arguments: [u64; 6]) -> bool {
        let reading = match operation.name {
            "read" => true,
            "write" => false,
            _ => return true,
        };
        let length = match usize::try_from(arguments[2]) {
            Ok(0) | Err(_) => return false,
            Ok(length) => length,
        };
        let Ok(lease) = self.descriptors.pin(arguments[0] as i32) else {
            return false;
        };
        if Self::access_rejects(&lease, reading) || lease.status().bits() & hl_descriptor::StatusFlags::NONBLOCKING != 0
        {
            return false;
        }
        if lease.object().kind() != hl_descriptor::ObjectKind::Pipe {
            return true;
        }
        let interest = if reading {
            hl_descriptor::Readiness::READ
        } else {
            hl_descriptor::Readiness::WRITE
        };
        let ready = lease.readiness(hl_descriptor::Readiness::from_bits(interest));
        if reading {
            !ready.contains(hl_descriptor::Readiness::READ) && !ready.contains(hl_descriptor::Readiness::HANGUP)
        } else {
            length > hl_ipc::PIPE_BUF
                || (!ready.contains(hl_descriptor::Readiness::WRITE)
                    && !ready.contains(hl_descriptor::Readiness::ERROR))
        }
    }

    fn handle(&mut self, operation: SyscallOperation, arguments: [u64; 6]) -> LinuxResult {
        self.descriptor_io(operation, arguments)
    }
}
impl<M: GuestMemory> FilesystemSyscalls for RuntimeFilesystemSyscalls<M> {
    fn may_block(&self, operation: SyscallOperation, arguments: [u64; 6]) -> bool {
        match operation.name {
            "creat" => self.open_may_block([(-100_i64) as u64, arguments[0], 0x241, arguments[1], 0, 0], false),
            "open" => self.open_may_block(
                [(-100_i64) as u64, arguments[0], arguments[1], arguments[2], 0, 0],
                false,
            ),
            "openat" => self.open_may_block(arguments, false),
            "openat2" => self.open_may_block(arguments, true),
            _ => false,
        }
    }

    fn handle(&mut self, operation: SyscallOperation, arguments: [u64; 6]) -> LinuxResult {
        match operation.name {
            "getcwd" => self.getcwd(arguments[0], arguments[1]),
            "chdir" => self.chdir(arguments[0]),
            "fchdir" => self.fchdir(arguments[0] as i32),
            "chroot" => self.chroot(arguments[0]),
            "creat" => self.openat([(-100_i64) as u64, arguments[0], 0x241, arguments[1], 0, 0], false),
            // Legacy x86-64 `open` carries (path, flags, mode); it is `openat` against AT_FDCWD.
            "open" => self.openat(
                [(-100_i64) as u64, arguments[0], arguments[1], arguments[2], 0, 0],
                false,
            ),
            "openat" => self.openat(arguments, false),
            "openat2" => self.openat(arguments, true),
            "newfstatat" => self.path_stat(arguments, false),
            // Legacy x86-64 `stat`/`lstat` carry (path, buf); `lstat` is `newfstatat` with AT_SYMLINK_NOFOLLOW.
            "stat" => self.path_stat([(-100_i64) as u64, arguments[0], arguments[1], 0, 0, 0], false),
            "lstat" => self.path_stat([(-100_i64) as u64, arguments[0], arguments[1], 0x100, 0, 0], false),
            "statx" => self.path_stat(arguments, true),
            "statfs" => self.statfs(arguments, false),
            "fstatfs" => self.statfs(arguments, true),
            "name_to_handle_at" => self.name_to_handle(arguments),
            "open_by_handle_at" => LinuxResult::Error(Errno::EPERM),
            "fadvise64" => {
                if arguments[3] <= 5 {
                    LinuxResult::Value(0)
                } else {
                    LinuxResult::Error(Errno::EINVAL)
                }
            }
            "flock" => {
                let Ok(operation) = FilesystemAbi::<M>::flock_operation(arguments[1] as u32) else {
                    return LinuxResult::Error(Errno::EINVAL);
                };
                let lease = match self.descriptors.pin(arguments[0] as i32) {
                    Ok(lease) => lease,
                    Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
                };
                let Some(cancellation) = self.pipe_cancellation.as_ref().map(|port| port.observation()) else {
                    return LinuxResult::Error(Errno::ENOSYS);
                };
                match lease.flock(operation, cancellation) {
                    Ok(()) => LinuxResult::Value(0),
                    Err(error) => LinuxResult::Error(FileErrno::object(error)),
                }
            }
            "fallocate" => self.fallocate(arguments),
            "truncate" => self.path_truncate(arguments[0], arguments[1]),
            "access" => self.path_access([(-100_i64) as u64, arguments[0], arguments[1], 0, 0, 0], false),
            "faccessat" => self.path_access(arguments, false),
            "faccessat2" => self.path_access(arguments, true),
            "readlinkat" => self.path_readlink(arguments),
            "readlink" => self.legacy_readlink(arguments),
            "chmod" | "mkdir" | "mkdirat" | "mknod" | "mknodat" | "unlink" | "unlinkat" | "rmdir" | "symlink"
            | "symlinkat" | "link" | "linkat" | "rename" | "renameat" | "renameat2" | "fchmod" | "fchmodat"
            | "fchmodat2" | "fchown" | "fchownat" | "chown" | "lchown" | "utime" | "utimes" | "futimesat"
            | "utimensat" => self.path_mutation(operation.name, arguments),
            "setxattr" | "lsetxattr" | "fsetxattr" | "getxattr" | "lgetxattr" | "fgetxattr" | "listxattr"
            | "llistxattr" | "flistxattr" | "removexattr" | "lremovexattr" | "fremovexattr" => {
                self.path_xattr(operation.name, arguments)
            }
            "fstat" => self.fstat(arguments[0] as i32, arguments[1]),
            "ftruncate" => self.ftruncate(arguments[0] as i32, arguments[1]),
            "getdents64" => self.getdents(arguments[0] as i32, arguments[1], arguments[2]),
            _ => LinuxResult::Error(Errno::ENOSYS),
        }
    }
}
#[cfg(test)]
#[path = "path_test.rs"]
mod path_tests;
#[cfg(test)]
#[path = "syscalls_test.rs"]
mod tests;
