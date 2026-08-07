use hl_ipc::{SharedMemoryError as CatalogError, SharedMemoryId};
use hl_isa::GuestAddress;
use hl_linux::{Errno, GuestMemory, LinuxResult, SysvAbi};

use crate::{MappingError, ipc::error_projection::ErrorProjection, ipc::syscalls::RuntimeIpcSyscalls};

impl<M: GuestMemory> RuntimeIpcSyscalls<M> {
    pub(in crate::ipc) fn shmat(&self, arguments: [u64; 6]) -> LinuxResult {
        let plan = SysvAbi::<M>::shmat(arguments[0], arguments[1], arguments[2] as u32);
        let Some(id) = SharedMemoryId::from_linux_id(plan.identifier.0) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let (actor, pid, now) = match self.context() {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let attach = match self
            .catalog
            .with_shared_memory(|namespace| namespace.shmat_plan(id, actor, plan.flags))
        {
            Ok(value) => value,
            Err(CatalogError::Removed) => {
                return LinuxResult::Error(Errno::EIDRM);
            }
            Err(CatalogError::Permission) => {
                return LinuxResult::Error(Errno::EACCES);
            }
            Err(CatalogError::NotFound) => {
                return LinuxResult::Error(Errno::EINVAL);
            }
            Err(error) => {
                return LinuxResult::Error(ErrorProjection::shared_get(error));
            }
        };
        let Some(port) = &self.shared_memory else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let address = match port.map(attach, GuestAddress::new(plan.address)) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let token = match self
            .catalog
            .with_shared_memory(|namespace| namespace.commit_attach(attach, pid, now))
        {
            Ok(value) => value,
            Err(error) => {
                return if port.rollback(address).is_err() {
                    LinuxResult::Error(Errno::EIO)
                } else if error == CatalogError::Removed {
                    LinuxResult::Error(Errno::EIDRM)
                } else {
                    LinuxResult::Error(ErrorProjection::shared_get(error))
                };
            }
        };
        if port.bind(address, token).is_err() {
            let detached = self
                .catalog
                .with_shared_memory(|namespace| namespace.shmdt(token, pid, now));
            let rolled_back = port.rollback(address);
            if detached.is_err() || rolled_back.is_err() {
                return LinuxResult::Error(Errno::EIO);
            }
            return LinuxResult::Error(Errno::ENOMEM);
        }
        LinuxResult::Value(address.get())
    }

    pub(in crate::ipc) fn shmdt(&self, arguments: [u64; 6]) -> LinuxResult {
        let address = GuestAddress::new(SysvAbi::<M>::shmdt(arguments[0]));
        let (_, pid, now) = match self.context() {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let Some(port) = &self.shared_memory else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let token = match port.unmap(address) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        match self
            .catalog
            .with_shared_memory(|namespace| namespace.shmdt(token, pid, now))
        {
            Ok(()) => LinuxResult::Value(0),
            Err(_) => LinuxResult::Error(Errno::EIO),
        }
    }
}

impl MappingError {
    const fn errno(self) -> Errno {
        match self {
            Self::Invalid => Errno::EINVAL,
            Self::NoMemory => Errno::ENOMEM,
            Self::Invariant => Errno::EIO,
            Self::Unsupported => Errno::ENOSYS,
        }
    }
}
