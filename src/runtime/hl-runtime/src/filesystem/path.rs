use hl_linux::{Errno, FilesystemAbi, GuestMarshaller, GuestMemory, LinuxResult, StatOutputKind, StatxExtensions};

use super::errno::FileErrno;
use crate::{DirectoryBaseLease, RuntimeFilesystemSyscalls, RuntimePathHost};

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn name_to_handle(&self, arguments: [u64; 6]) -> LinuxResult {
        let Some(host) = &self.path_host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let mut capacity = [0_u8; 4];
        if self.memory.read(arguments[2], &mut capacity) != Ok(4) {
            return LinuxResult::Error(Errno::EFAULT);
        }
        let capacity = u32::from_le_bytes(capacity);
        let required = 16_u32;
        if capacity < required {
            return if self.memory.write(arguments[2], &required.to_le_bytes()) == Ok(4) {
                LinuxResult::Error(Errno::EOVERFLOW)
            } else {
                LinuxResult::Error(Errno::EFAULT)
            };
        }
        let flags = arguments[4] as u32;
        let abi = FilesystemAbi::new(&self.memory, self.architecture);
        let operand = match abi.path_operand(
            arguments[0] as i32,
            arguments[1],
            flags & 0x1000 != 0,
            flags & 0x400 == 0,
        ) {
            Ok(operand) => operand,
            Err(error) => return LinuxResult::Error(FileErrno::marshal(error)),
        };
        let node = match self.stat_node(host.as_ref(), &operand) {
            Ok(node) => node,
            Err(error) => return LinuxResult::Error(error),
        };
        let metadata = match node.metadata() {
            Ok(metadata) => metadata,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let mut handle = [0_u8; 24];
        handle[0..4].copy_from_slice(&required.to_le_bytes());
        handle[4..8].copy_from_slice(&1_i32.to_le_bytes());
        handle[8..16].copy_from_slice(&metadata.identity.device.to_le_bytes());
        handle[16..24].copy_from_slice(&metadata.identity.inode.to_le_bytes());
        if self.memory.write(arguments[2], &handle) != Ok(handle.len()) {
            return LinuxResult::Error(Errno::EFAULT);
        }
        if arguments[3] != 0
            && self
                .memory
                .write(arguments[3], &(metadata.identity.device as i32).to_le_bytes())
                != Ok(4)
        {
            return LinuxResult::Error(Errno::EFAULT);
        }
        LinuxResult::Value(0)
    }

    pub(super) fn legacy_readlink(&self, arguments: [u64; 6]) -> LinuxResult {
        self.path_readlink([(-100_i64) as u64, arguments[0], arguments[1], arguments[2], 0, 0])
    }

    pub(super) fn path_truncate(&self, pointer: u64, size: u64) -> LinuxResult {
        let Some(host) = &self.path_host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let abi = FilesystemAbi::new(&self.memory, self.architecture);
        let operand = match abi.stat(pointer) {
            Ok(hl_linux::FilesystemTarget::Path(operand)) => operand,
            Ok(_) => return LinuxResult::Error(Errno::EINVAL),
            Err(error) => return LinuxResult::Error(FileErrno::marshal(error)),
        };
        let node = match self.stat_node(host.as_ref(), &operand) {
            Ok(node) => node,
            Err(error) => return LinuxResult::Error(error),
        };
        if size > i64::MAX as u64 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if let Some(limit) = &self.file_size_limit
            && limit.soft_limit().is_ok_and(|soft| soft != u64::MAX && size > soft)
        {
            let _ = limit.queue_sigxfsz();
            return LinuxResult::Error(Errno::EFBIG);
        }
        let before = node.metadata().ok();
        match node.truncate(size) {
            Ok(()) => {
                if let (Some(before), Some(changes)) = (before, &self.backing_changes) {
                    let _ = changes.changed(hl_memory::BackingChange {
                        identity: hl_memory::FileIdentity {
                            device: before.identity.device,
                            object: before.identity.inode,
                        },
                        old_size: before.size,
                        new_size: size,
                        flags: hl_memory::BackingChangeFlags::SIZE,
                    });
                }
                LinuxResult::Value(0)
            }
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    pub(super) fn stat_node(
        &self,
        host: &dyn RuntimePathHost,
        operand: &hl_linux::PathOperand,
    ) -> Result<Box<dyn crate::ResolvedPathLease>, Errno> {
        if operand.allow_empty && operand.path.as_bytes().is_empty() {
            let lease = self
                .descriptors
                .pin(operand.directory.raw() as i32)
                .map_err(FileErrno::descriptor)?;
            return host
                .descriptor_node(lease)
                .map_err(super::super::path_host::RuntimePathError::errno);
        }
        let base = self.path_base(host, operand)?;
        if !operand.nofollow
            && let Some(descriptor) =
                super::proc::DescriptorLink::resolve_at(base.path().as_str().as_bytes(), operand.path.as_bytes())
        {
            let lease = self.descriptors.pin(descriptor).map_err(|error| match error {
                hl_descriptor::DescriptorError::BadDescriptor => Errno::ENOENT,
                other => FileErrno::descriptor(other),
            })?;
            return host
                .descriptor_node(lease)
                .map_err(super::super::path_host::RuntimePathError::errno);
        }
        host.resolve(&base, operand)
            .map_err(super::super::path_host::RuntimePathError::errno)
    }

    pub(super) fn path_base(
        &self,
        host: &dyn RuntimePathHost,
        operand: &hl_linux::PathOperand,
    ) -> Result<DirectoryBaseLease, Errno> {
        if operand.path.is_absolute() {
            let root = self.fs_context.root();
            return if root.as_str() == "/" {
                host.root_base()
                    .map_err(super::super::path_host::RuntimePathError::errno)
            } else {
                host.working_base(root)
                    .map(|base| DirectoryBaseLease::confined_root(base.path().clone()))
                    .map_err(super::super::path_host::RuntimePathError::errno)
            };
        }
        if operand.directory.raw() == (-100_i64) as u64 {
            let snapshot = self.working.snapshot();
            if snapshot.deleted {
                return Err(Errno::ENOENT);
            }
            let rooted = self
                .fs_context
                .rooted(&snapshot.path)
                .map_err(|()| Errno::ENAMETOOLONG)?;
            return host
                .working_base(rooted)
                .map(|base| match self.fs_context.root().as_str() {
                    "/" => base,
                    _ => DirectoryBaseLease::confined_root(base.path().clone()),
                })
                .map_err(super::super::path_host::RuntimePathError::errno);
        }
        let lease = self
            .descriptors
            .pin(operand.directory.raw() as i32)
            .map_err(FileErrno::descriptor)?;
        host.descriptor_base(lease)
            .map_err(super::super::path_host::RuntimePathError::errno)
    }

    pub(super) fn path_stat(&self, arguments: [u64; 6], statx: bool) -> LinuxResult {
        let Some(host) = &self.path_host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let abi = FilesystemAbi::new(&self.memory, self.architecture);
        let (operand, output, kind) = if statx {
            let (operand, _mask) = match abi.statx_operand(
                arguments[0] as i32,
                arguments[1],
                arguments[2] as u32,
                arguments[3] as u32,
            ) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(FileErrno::marshal(error)),
            };
            (operand, arguments[4], None)
        } else {
            let operand = match abi.stat_operand(arguments[0] as i32, arguments[1], arguments[3] as u32) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(FileErrno::marshal(error)),
            };
            (operand, arguments[2], Some(StatOutputKind::Stat))
        };
        let node = match self.stat_node(host.as_ref(), &operand) {
            Ok(node) => node,
            Err(error) => return LinuxResult::Error(error),
        };
        let (metadata, kind) = if let Some(kind) = kind {
            let metadata = match node.metadata() {
                Ok(metadata) => metadata,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            (metadata, kind)
        } else {
            let resolved = match node.resolved_metadata() {
                Ok(metadata) => metadata,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
            let kind = StatOutputKind::Statx {
                extensions: StatxExtensions {
                    birth: resolved.birth,
                    mount_id: resolved.mount,
                },
            };
            (resolved.file, kind)
        };
        let staged = match abi.stage_stat(output, &metadata, kind) {
            Ok(staged) => staged,
            Err(error) => return LinuxResult::Error(FileErrno::marshal(error)),
        };
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(_) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(FileErrno::marshal(error)),
        }
    }

    pub(super) fn path_access(&self, arguments: [u64; 6], accepts_flags: bool) -> LinuxResult {
        let Some(host) = &self.path_host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let abi = FilesystemAbi::new(&self.memory, self.architecture);
        let flags = if accepts_flags { arguments[3] as u32 } else { 0 };
        let plan = match abi.access(arguments[0] as i32, arguments[1], arguments[2] as u32, flags) {
            Ok(plan) => plan,
            Err(error) => return LinuxResult::Error(FileErrno::marshal(error)),
        };
        let node = match self.stat_node(host.as_ref(), &plan.operand) {
            Ok(node) => node,
            Err(error) => return LinuxResult::Error(error),
        };
        let metadata = match node.metadata() {
            Ok(metadata) => metadata,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let identity = match host.access_identity_for(plan.effective_ids) {
            Ok(identity) => identity,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        match identity.check_access(&metadata, plan.access) {
            Ok(()) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::EACCES),
        }
    }

    pub(super) fn path_readlink(&self, arguments: [u64; 6]) -> LinuxResult {
        let Some(host) = &self.path_host else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let abi = FilesystemAbi::new(&self.memory, self.architecture);
        let capacity = match i64::try_from(arguments[3]) {
            Ok(capacity) if capacity > 0 => capacity as usize,
            _ => return LinuxResult::Error(Errno::EINVAL),
        };
        if let Err(error) = abi.probe_readlink_output(arguments[2], capacity) {
            return LinuxResult::Error(FileErrno::marshal(error));
        }
        let directory = arguments[0] as i32;
        let operand = match abi.path_operand(directory, arguments[1], directory >= 0, true) {
            Ok(operand) => operand,
            Err(error) => return LinuxResult::Error(FileErrno::marshal(error)),
        };
        let node = if operand.path.as_bytes().is_empty() {
            match self.descriptors.pin(directory) {
                Ok(lease) => host.descriptor_node(lease),
                Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
            }
        } else {
            let base = match self.path_base(host.as_ref(), &operand) {
                Ok(base) => base,
                Err(error) => return LinuxResult::Error(error),
            };
            host.resolve(&base, &operand)
        };
        let target = match node.and_then(|node| node.read_link()) {
            Ok(target) => target,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let staged = match abi.stage_readlink(arguments[2], capacity, &target) {
            Ok(staged) => staged,
            Err(error) => return LinuxResult::Error(FileErrno::marshal(error)),
        };
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(count) => LinuxResult::Value(count as u64),
            Err(error) => LinuxResult::Error(FileErrno::marshal(error)),
        }
    }
}
