//! fstat and getdents metadata reporting for the filesystem syscall surface.

use hl_descriptor::ObjectError;
use hl_linux::{DirectoryRecord, Errno, FilesystemAbi, GuestMarshaller, GuestMemory, LinuxResult, StatOutputKind};
use hl_vfs::{FileIdentity, FileKind, FileMetadata, FileTimestamp, Permissions};

use super::errno::FileErrno;
use super::syscalls::RuntimeFilesystemSyscalls;

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn fstat(&self, descriptor: i32, output: u64) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        let metadata = match Self::descriptor_metadata(&lease) {
            Ok(metadata) => metadata,
            Err(error) => return LinuxResult::Error(FileErrno::object(error)),
        };
        let abi = FilesystemAbi::new(&self.memory, self.architecture);
        let Ok(staged) = abi.stage_stat(output, &metadata, StatOutputKind::Stat) else {
            return LinuxResult::Error(Errno::EFAULT);
        };
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(_) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::EFAULT),
        }
    }
    pub(super) fn getdents(&self, descriptor: i32, output: u64, capacity: u64) -> LinuxResult {
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
        let Ok(staged) = abi.stage_getdents(output, capacity, &records[..emitted]) else {
            return LinuxResult::Error(Errno::EFAULT);
        };
        let marshaller = GuestMarshaller::new(&self.memory, self.architecture);
        let Ok(written) = staged.commit(&marshaller) else {
            return LinuxResult::Error(Errno::EFAULT);
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
            block_size: value.block_size,
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
