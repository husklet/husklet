use super::errno::FileErrno;
use crate::RuntimePathHost;
use hl_descriptor::{DescriptorError, DescriptorFlags, DescriptorTable, ExactDuplicate, ObjectError};
use hl_linux::{
    DescriptorIoSyscalls, DirectoryRecord, Errno, FilesystemAbi, FilesystemSyscalls, GuestAccess, GuestArchitecture,
    GuestMarshaller, GuestMemory, IovecPlan, LinuxResult, StatOutputKind, SyscallOperation, VectorTransfer,
};
use hl_vfs::{FileIdentity, FileKind, FileMetadata, FileTimestamp, Permissions};
use std::sync::Arc;

use super::{VectorDirection, VectorError, VectorPosition, VectorRequest, VectorTerminal};
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
            "close" => match self.descriptors.close(descriptor) {
                Ok(()) => LinuxResult::Value(0),
                Err(error) => LinuxResult::Error(FileErrno::descriptor(error)),
            },
            "dup" => Self::descriptor_result(self.descriptors.duplicate(descriptor, 0, DescriptorFlags::default())),
            "dup3" => {
                let flags = arguments[2] as u32;
                if flags & !0o2000000 != 0 {
                    return LinuxResult::Error(Errno::EINVAL);
                }
                let local = DescriptorFlags::from_bits(if flags == 0 { 0 } else { DescriptorFlags::CLOSE_ON_EXEC });
                Self::descriptor_result(self.descriptors.duplicate_exact(
                    descriptor,
                    arguments[1] as i32,
                    ExactDuplicate::Dup3(local),
                ))
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
        let limit = port.soft_limit().map_err(|_| LinuxResult::Error(Errno::EIO))?;
        if limit == u64::MAX || count == 0 {
            return Ok(count);
        }
        let metadata = lease.metadata().map_err(|error| LinuxResult::Error(FileErrno::object(error)))?;
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
    pub(super) fn vector_io(&self, descriptor: i32, address: u64, raw_count: u64, reading: bool) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let Ok(count) = usize::try_from(raw_count) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let access = if reading { GuestAccess::Write } else { GuestAccess::Read };
        let plan = match marshaller.io_vector_records(address, count, access) {
            Ok(plan) => plan,
            Err(error) => {
                if matches!(error, hl_linux::MarshalError::Fault(_)) && Self::access_rejects(&lease, reading) {
                    return LinuxResult::Error(Errno::EBADF);
                }
                return LinuxResult::Error(FileErrno::vector(error));
            }
        };
        self.execute_vector(&lease, &marshaller, plan, reading, None, None)
    }
    pub(super) fn execute_vector(
        &self,
        lease: &hl_descriptor::OperationLease,
        marshaller: &GuestMarshaller<'_, M>,
        mut plan: IovecPlan,
        reading: bool,
        offset: Option<u64>,
        flags: Option<u32>,
    ) -> LinuxResult {
        if plan.vectors.is_empty() {
            return if Self::access_rejects(lease, reading) {
                LinuxResult::Error(Errno::EBADF)
            } else if flags.is_some_and(|value| value != 0) {
                LinuxResult::Error(Errno::EOPNOTSUPP)
            } else {
                LinuxResult::Value(0)
            };
        }
        if !reading {
            let maximum = match usize::try_from(plan.total_length) {
                Ok(value) => value,
                Err(_) => return LinuxResult::Error(Errno::EINVAL),
            };
            let allowed = match self.limit_write(lease, offset, maximum) {
                Ok(value) => value as u64,
                Err(error) => return error,
            };
            if allowed < plan.total_length {
                let mut remaining = allowed;
                for vector in &mut plan.vectors {
                    vector.length = vector.length.min(remaining);
                    remaining -= vector.length;
                }
                plan.total_length = allowed;
            }
        }
        let cancellation = self.pipe_cancellation.as_ref().map(|port| port.observation());
        let context = hl_descriptor::OperationContext {
            actor: self.actor,
            cancellation,
        };
        let atomic_write = !reading
            && lease
                .atomic_write_limit()
                .is_some_and(|limit| plan.total_length <= limit as u64);
        if !atomic_write && let Some(terminal) = &self.vector_terminal {
            let request = VectorRequest {
                vectors: &plan.vectors,
                direction: if reading {
                    VectorDirection::Read
                } else {
                    VectorDirection::Write
                },
                position: offset.map_or(VectorPosition::Shared, VectorPosition::At),
                flags,
            };
            match terminal.execute(lease, request) {
                Ok(count) => return LinuxResult::Value(count as u64),
                Err(VectorError::Unsupported) => {}
                Err(VectorError::Fault) if reading => return Self::failed_read_probe(lease),
                Err(VectorError::Fault) => return LinuxResult::Error(Errno::EFAULT),
                Err(VectorError::Errno(Errno::EFAULT)) if reading => return Self::failed_read_probe(lease),
                Err(VectorError::Errno(errno)) => return LinuxResult::Error(errno),
                Err(VectorError::Object(ObjectError::BrokenPipe)) => {
                    if let Some(signal) = &self.pipe_signal {
                        let _ = signal.queue_sigpipe();
                    }
                    return LinuxResult::Error(Errno::EPIPE);
                }
                Err(VectorError::Object(error)) => return LinuxResult::Error(FileErrno::object(error)),
            }
        }
        let access = if reading { GuestAccess::Write } else { GuestAccess::Read };
        let plan = match plan.validate_io(access) {
            Ok(plan) => plan,
            Err(hl_linux::MarshalError::Fault(_)) if reading => return Self::failed_read_probe(lease),
            Err(error) => return LinuxResult::Error(FileErrno::vector(error)),
        };
        if flags.is_some_and(|value| value != 0) {
            return LinuxResult::Error(Errno::EOPNOTSUPP);
        }
        if reading {
            return Self::read_vector(lease, marshaller, plan, offset, context);
        }
        self.write_vector(lease, marshaller, plan, offset, context, atomic_write)
    }

    pub(super) fn access_rejects(lease: &hl_descriptor::OperationLease, reading: bool) -> bool {
        if lease.status().contains(hl_descriptor::StatusFlags::PATH_ONLY) {
            return true;
        }
        let mode = lease.status().bits() & hl_descriptor::StatusFlags::ACCESS_MODE_MASK;
        if reading { mode == 1 } else { mode == 0 }
    }

    fn read_vector(
        lease: &hl_descriptor::OperationLease,
        marshaller: &GuestMarshaller<'_, M>,
        plan: IovecPlan,
        offset: Option<u64>,
        context: hl_descriptor::OperationContext<'_>,
    ) -> LinuxResult {
        let mut transfer = VectorTransfer::vacant(plan);
        let result = {
            let mut output = transfer.output();
            let result = match offset {
                Some(position) => lease.read_vector_at(position, &mut output),
                None => lease.read_vector_context(&mut output, context),
            };
            match result {
                Err(ObjectError::NotSupported) => {
                    let first = output.iter_mut().find(|vector| !vector.is_empty());
                    match (offset, first) {
                        (_, None) => Ok(0),
                        (Some(position), Some(vector)) => lease.read_at(position, vector),
                        (None, Some(vector)) => lease.read_context(vector, context),
                    }
                }
                result => result,
            }
        };
        let count = match result {
            Ok(count) => count,
            Err(ObjectError::Retired) => return Self::failed_read_probe(lease),
            Err(error) => return LinuxResult::Error(FileErrno::object(error)),
        };
        let progress = transfer.publish(marshaller, count);
        if progress.copied == 0 && progress.fault.is_some() {
            LinuxResult::Error(Errno::EFAULT)
        } else {
            LinuxResult::Value(progress.copied as u64)
        }
    }

    fn write_vector(
        &self,
        lease: &hl_descriptor::OperationLease,
        marshaller: &GuestMarshaller<'_, M>,
        plan: IovecPlan,
        offset: Option<u64>,
        context: hl_descriptor::OperationContext<'_>,
        atomic: bool,
    ) -> LinuxResult {
        let maximum = match usize::try_from(plan.total_length) {
            Ok(value) => value,
            Err(_) => return LinuxResult::Error(Errno::EINVAL),
        };
        match lease.probe_write(maximum) {
            Ok(Some(count)) => return LinuxResult::Value(count as u64),
            Ok(None) => {}
            Err(error) => return LinuxResult::Error(FileErrno::object(error)),
        }
        let transfer = match if atomic {
            VectorTransfer::capture_all(marshaller, plan)
        } else {
            VectorTransfer::capture(marshaller, plan)
        } {
            Ok(transfer) => transfer,
            Err(error) => return LinuxResult::Error(FileErrno::vector(error)),
        };
        let input = transfer.input();
        let result = match offset {
            Some(position) => lease.write_vector_at(position, &input),
            None => lease.write_vector_context(&input, context),
        };
        match result {
            Ok(count) => LinuxResult::Value(count as u64),
            Err(ObjectError::BrokenPipe) => {
                if let Some(signal) = &self.pipe_signal {
                    let _ = signal.queue_sigpipe();
                }
                LinuxResult::Error(Errno::EPIPE)
            }
            Err(error) => LinuxResult::Error(FileErrno::object(error)),
        }
    }
    fn fstat(&self, descriptor: i32, output: u64) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let metadata = match Self::descriptor_metadata(&lease) {
            Ok(metadata) => metadata,
            Err(error) => return LinuxResult::Error(FileErrno::object(error)),
        };
        let abi = FilesystemAbi::new(&self.memory, self.architecture);
        let staged = match abi.stage_stat(output, &metadata, StatOutputKind::Stat) {
            Ok(staged) => staged,
            Err(_) => return LinuxResult::Error(Errno::EFAULT),
        };
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(_) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::EFAULT),
        }
    }
    fn getdents(&self, descriptor: i32, output: u64, capacity: u64) -> LinuxResult {
        let Ok(capacity) = usize::try_from(capacity) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let batch = match lease.read_directory(4096) {
            Ok(batch) => batch,
            Err(error) => return LinuxResult::Error(FileErrno::object(error)),
        };
        let records: Vec<_> = batch
            .entries
            .iter()
            .map(|entry| DirectoryRecord {
                inode: entry.inode,
                offset: entry.cookie,
                file_type: entry.file_type,
                name: entry.name.clone(),
            })
            .collect();
        if records
            .first()
            .is_some_and(|record| Self::dirent_length(record) > capacity)
        {
            return LinuxResult::Error(Errno::EINVAL);
        }
        let emitted = records
            .iter()
            .scan(0_usize, |used, record| {
                let next = used.checked_add(Self::dirent_length(record))?;
                if next > capacity {
                    return None;
                }
                *used = next;
                Some(())
            })
            .count();
        let abi = FilesystemAbi::new(&self.memory, self.architecture);
        let staged = match abi.stage_getdents(output, capacity, &records[..emitted]) {
            Ok(staged) => staged,
            Err(_) => return LinuxResult::Error(Errno::EFAULT),
        };
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let written = match staged.commit(&marshaller) {
            Ok(written) => written,
            Err(_) => return LinuxResult::Error(Errno::EFAULT),
        };
        if let Err(error) = lease.commit_directory(batch.token, emitted) {
            return LinuxResult::Error(FileErrno::object(error));
        }
        LinuxResult::Value(written as u64)
    }
    fn dirent_length(record: &DirectoryRecord) -> usize {
        (19 + record.name.len() + 1 + 7) & !7
    }
    fn descriptor_metadata(lease: &hl_descriptor::OperationLease) -> Result<FileMetadata, ObjectError> {
        let mut metadata = lease.metadata()?;
        if metadata.inode == 0 {
            metadata.inode = lease.description_identity().identity;
        }
        Self::vfs_metadata(metadata)
    }
    fn vfs_metadata(value: hl_descriptor::OfdMetadata) -> Result<FileMetadata, ObjectError> {
        let kind = match value.kind {
            1 => FileKind::Fifo,
            2 => FileKind::Character,
            4 => FileKind::Directory,
            6 => FileKind::Block,
            8 => FileKind::Regular,
            10 => FileKind::Symlink,
            12 => FileKind::Socket,
            _ => return Err(ObjectError::InvalidArgument),
        };
        Ok(FileMetadata {
            identity: FileIdentity {
                device: value.device,
                inode: value.inode,
            },
            kind,
            permissions: Permissions::from_bits(value.permissions),
            links: value.links,
            user: value.user,
            group: value.group,
            special_device: value.special_device,
            size: value.size,
            blocks_512: value.blocks_512,
            accessed: FileTimestamp {
                seconds: value.accessed.seconds,
                nanoseconds: value.accessed.nanoseconds,
            },
            modified: FileTimestamp {
                seconds: value.modified.seconds,
                nanoseconds: value.modified.nanoseconds,
            },
            changed: FileTimestamp {
                seconds: value.changed.seconds,
                nanoseconds: value.changed.nanoseconds,
            },
        })
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
        if Self::access_rejects(&lease, reading)
            || lease.status().bits() & hl_descriptor::StatusFlags::NONBLOCKING != 0
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
            !ready.contains(hl_descriptor::Readiness::READ)
                && !ready.contains(hl_descriptor::Readiness::HANGUP)
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
            "creat" => self.open_may_block(
                [(-100_i64) as u64, arguments[0], 0x241, arguments[1], 0, 0],
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
            "creat" => self.openat(
                [(-100_i64) as u64, arguments[0], 0x241, arguments[1], 0, 0],
                false,
            ),
            "openat" => self.openat(arguments, false),
            "openat2" => self.openat(arguments, true),
            "newfstatat" => self.path_stat(arguments, false),
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
                let operation = match FilesystemAbi::<M>::flock_operation(arguments[1] as u32) {
                    Ok(operation) => operation,
                    Err(_) => return LinuxResult::Error(Errno::EINVAL),
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
            "chmod" | "mkdir" | "mkdirat" | "mknodat" | "unlink" | "unlinkat" | "rmdir" | "symlink" | "symlinkat"
            | "link" | "linkat" | "rename" | "renameat" | "renameat2" | "fchmod" | "fchmodat" | "fchmodat2" | "fchown"
            | "fchownat" | "chown" | "utime" | "utimes" | "futimesat" | "utimensat" => {
                self.path_mutation(operation.name, arguments)
            }
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
