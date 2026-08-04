use hl_linux::{Errno, GuestMemory, LinuxResult};

use super::errno::FileErrno;
use crate::RuntimeFilesystemSyscalls;

impl<M: GuestMemory> RuntimeFilesystemSyscalls<M> {
    pub(super) fn ftruncate(&self, descriptor: i32, size: u64) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
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
        if let Some(memfds) = &self.memfds {
            match memfds.resize(lease.description_identity(), size) {
                Ok(()) => return LinuxResult::Value(0),
                Err(hl_memory::SharedError::NotFound) => {}
                Err(error) => return LinuxResult::Error(Self::shared_errno(error)),
            }
        }
        let before = lease.metadata().ok();
        match lease.truncate(size) {
            Ok(()) => {
                if let (Some(before), Some(changes)) = (before, &self.backing_changes) {
                    let _ = changes.changed(hl_memory::BackingChange {
                        identity: hl_memory::FileIdentity {
                            device: before.device,
                            object: before.inode,
                        },
                        old_size: before.size,
                        new_size: size,
                        flags: hl_memory::BackingChangeFlags::SIZE,
                    });
                }
                LinuxResult::Value(0)
            }
            Err(error) => LinuxResult::Error(FileErrno::object(error)),
        }
    }

    pub(super) fn synchronize(&self, descriptor: i32, data_only: bool) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(lease) => lease,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        if self
            .memfds
            .as_ref()
            .is_some_and(|memfds| memfds.seals(lease.description_identity()).is_ok())
        {
            return LinuxResult::Value(0);
        }
        match lease.synchronize(data_only) {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(FileErrno::object(error)),
        }
    }

    pub(super) fn add_seals(&self, descriptor: i32, bits: u64) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        if bits & !0x1f != 0 {
            return LinuxResult::Error(Errno::EINVAL);
        }
        if let Some(memfds) = &self.memfds {
            match memfds.add_seals(lease.description_identity(), bits as u8) {
                Ok(_) => return LinuxResult::Value(0),
                Err(hl_memory::SharedError::NotFound) => {}
                Err(error) => return LinuxResult::Error(Self::shared_errno(error)),
            }
        }
        match lease.add_seals(bits as u8) {
            Ok(_) => LinuxResult::Value(0),
            Err(hl_descriptor::ObjectError::NotSupported) => LinuxResult::Error(Errno::EINVAL),
            Err(error) => LinuxResult::Error(FileErrno::object(error)),
        }
    }

    pub(super) fn get_seals(&self, descriptor: i32) -> LinuxResult {
        let lease = match self.descriptors.pin(descriptor) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(FileErrno::descriptor(error)),
        };
        if let Some(memfds) = &self.memfds {
            match memfds.seals(lease.description_identity()) {
                Ok(bits) => return LinuxResult::Value(bits as u64),
                Err(hl_memory::SharedError::NotFound) => {}
                Err(error) => return LinuxResult::Error(Self::shared_errno(error)),
            }
        }
        match lease.seals() {
            Ok(bits) => LinuxResult::Value(bits as u64),
            Err(hl_descriptor::ObjectError::NotSupported) => LinuxResult::Error(Errno::EINVAL),
            Err(error) => LinuxResult::Error(FileErrno::object(error)),
        }
    }

    fn shared_errno(error: hl_memory::SharedError) -> Errno {
        match error {
            hl_memory::SharedError::Sealed => Errno::EPERM,
            hl_memory::SharedError::Busy => Errno::EBUSY,
            hl_memory::SharedError::ResourceLimit => Errno::ENOMEM,
            hl_memory::SharedError::NotFound => Errno::EINVAL,
            _ => Errno::EINVAL,
        }
    }
}
