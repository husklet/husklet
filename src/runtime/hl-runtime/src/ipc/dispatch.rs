use hl_ipc::{
    Credentials, MessageError, MessageQueueId, SemaphoreError, SemaphoreId, SharedMemoryError, SharedMemoryId,
};
use hl_linux::{
    Errno, GuestMemory, LinuxResult, MessageControlPlan, SemaphoreControlPlan, SharedMemoryControlPlan, SysvAbi,
    SysvIdentifier, SysvRawIndex,
};

use super::control::ControlProjection;
use super::syscalls::{ErrorProjection, RuntimeIpcSyscalls};

impl<M: GuestMemory> RuntimeIpcSyscalls<M> {
    pub(super) fn shmctl(&self, arguments: [u64; 6]) -> LinuxResult {
        let abi = SysvAbi::new(&self.memory, self.architecture);
        let plan = match abi.shmctl(arguments[0], arguments[1] as u32, arguments[2]) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let (actor, pid, now) = match self.context() {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        match plan {
            SharedMemoryControlPlan::Remove { identifier } => self.shared_remove(identifier, actor, pid, now),
            SharedMemoryControlPlan::Set { identifier, source } => {
                self.shared_set(&abi, identifier, source, actor, pid, now)
            }
            SharedMemoryControlPlan::Stat { identifier, output } => {
                self.shared_identifier_stat(&abi, identifier, actor, output)
            }
            SharedMemoryControlPlan::IndexStat { index, any, output } => {
                self.shared_index_stat(&abi, index, actor, any, output)
            }
            SharedMemoryControlPlan::Information { usage, output } => self.shared_info(&abi, usage, output),
            SharedMemoryControlPlan::Lock { .. } => LinuxResult::Error(Errno::ENOSYS),
        }
    }

    pub(super) fn msgctl(&self, arguments: [u64; 6]) -> LinuxResult {
        let abi = SysvAbi::new(&self.memory, self.architecture);
        let plan = match abi.msgctl(arguments[0], arguments[1] as u32, arguments[2]) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let (actor, pid, now) = match self.context() {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        match plan {
            MessageControlPlan::Remove { identifier } => self.message_remove(identifier, actor, pid, now),
            MessageControlPlan::Set { identifier, source } => self.message_set(&abi, identifier, source, actor, now),
            MessageControlPlan::Stat { identifier, output } => {
                self.message_identifier_stat(&abi, identifier, actor, output)
            }
            MessageControlPlan::IndexStat { index, any, output } => {
                self.message_index_stat(&abi, index, actor, any, output)
            }
            MessageControlPlan::Information { usage, output } => self.message_info(&abi, usage, output),
        }
    }

    pub(super) fn semctl_control(
        &self,
        abi: &SysvAbi<'_, M>,
        plan: SemaphoreControlPlan,
        actor: Credentials,
        pid: u32,
        now: u64,
    ) -> Option<LinuxResult> {
        Some(match plan {
            SemaphoreControlPlan::Remove { identifier } => self.semaphore_remove(identifier, actor, pid, now),
            SemaphoreControlPlan::Set { identifier, source } => self.semaphore_set(abi, identifier, source, actor, now),
            SemaphoreControlPlan::Stat { identifier, output } => {
                self.semaphore_identifier_stat(abi, identifier, actor, output)
            }
            SemaphoreControlPlan::IndexStat { index, any, output } => {
                self.semaphore_index_stat(abi, index, actor, any, output)
            }
            SemaphoreControlPlan::Information { usage, output } => self.semaphore_info(abi, usage, output),
            _ => return None,
        })
    }

    fn shared_remove(&self, identifier: SysvIdentifier, actor: Credentials, pid: u32, now: u64) -> LinuxResult {
        let Some(id) = SharedMemoryId::from_linux_id(identifier.0) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        match self
            .catalog
            .with_shared_memory(|namespace| namespace.remove(id, actor, pid, now))
        {
            Ok(()) => LinuxResult::Value(0),
            Err(SharedMemoryError::Permission) => LinuxResult::Error(Errno::EPERM),
            Err(SharedMemoryError::NotFound | SharedMemoryError::Removed) => LinuxResult::Error(Errno::EINVAL),
            Err(error) => LinuxResult::Error(ErrorProjection::shared_get(error)),
        }
    }

    fn shared_set(
        &self,
        abi: &SysvAbi<'_, M>,
        identifier: SysvIdentifier,
        source: u64,
        actor: Credentials,
        pid: u32,
        now: u64,
    ) -> LinuxResult {
        let imported = match abi.import_shared_status(source) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let Some(id) = SharedMemoryId::from_linux_id(identifier.0) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let owner = Credentials {
            uid: imported.permissions.uid,
            gid: imported.permissions.gid,
        };
        match self.catalog.with_shared_memory(|namespace| {
            namespace.set_permissions(id, actor, owner, imported.permissions.mode as u16, pid, now)
        }) {
            Ok(()) => LinuxResult::Value(0),
            Err(SharedMemoryError::Permission) => LinuxResult::Error(Errno::EPERM),
            Err(error) => LinuxResult::Error(ControlProjection::shared_errno(error)),
        }
    }

    fn shared_identifier_stat(
        &self,
        abi: &SysvAbi<'_, M>,
        identifier: SysvIdentifier,
        actor: Credentials,
        output: u64,
    ) -> LinuxResult {
        SharedMemoryId::from_linux_id(identifier.0).map_or(LinuxResult::Error(Errno::EINVAL), |id| {
            self.shared_stat(abi, id, actor, false, output, false)
        })
    }

    fn shared_index_stat(
        &self,
        abi: &SysvAbi<'_, M>,
        index: SysvRawIndex,
        actor: Credentials,
        any: bool,
        output: u64,
    ) -> LinuxResult {
        let Ok(index) = usize::try_from(index.0) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let metadata = self.catalog.with_shared_memory(|namespace| {
            namespace
                .snapshot()
                .segments
                .into_iter()
                .find(|value| value.id.slot as usize == index)
        });
        metadata.map_or(LinuxResult::Error(Errno::EINVAL), |value| {
            self.shared_stat(abi, value.id, actor, any, output, true)
        })
    }

    fn message_remove(&self, identifier: SysvIdentifier, actor: Credentials, pid: u32, now: u64) -> LinuxResult {
        let Some(id) = MessageQueueId::from_linux_id(identifier.0) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        match self
            .catalog
            .with_messages(|namespace| namespace.remove(id, actor, pid, now))
        {
            Ok(()) => LinuxResult::Value(0),
            Err(MessageError::Permission) => LinuxResult::Error(Errno::EPERM),
            Err(error) => LinuxResult::Error(ErrorProjection::message(error)),
        }
    }

    fn message_set(
        &self,
        abi: &SysvAbi<'_, M>,
        identifier: SysvIdentifier,
        source: u64,
        actor: Credentials,
        now: u64,
    ) -> LinuxResult {
        let imported = match abi.import_message_status(source) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let Some(id) = MessageQueueId::from_linux_id(identifier.0) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let Ok(maximum) = usize::try_from(imported.maximum_bytes) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let owner = Credentials {
            uid: imported.permissions.uid,
            gid: imported.permissions.gid,
        };
        match self.catalog.with_messages(|namespace| {
            namespace.set_control(id, actor, owner, imported.permissions.mode as u16, maximum, now)
        }) {
            Ok(()) => LinuxResult::Value(0),
            Err(MessageError::Permission) => LinuxResult::Error(Errno::EPERM),
            Err(error) => LinuxResult::Error(ControlProjection::message_errno(error)),
        }
    }

    fn message_identifier_stat(
        &self,
        abi: &SysvAbi<'_, M>,
        identifier: SysvIdentifier,
        actor: Credentials,
        output: u64,
    ) -> LinuxResult {
        MessageQueueId::from_linux_id(identifier.0).map_or(LinuxResult::Error(Errno::EINVAL), |id| {
            self.message_stat(abi, id, actor, false, output, false)
        })
    }

    fn message_index_stat(
        &self,
        abi: &SysvAbi<'_, M>,
        index: SysvRawIndex,
        actor: Credentials,
        any: bool,
        output: u64,
    ) -> LinuxResult {
        let Ok(index) = usize::try_from(index.0) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let metadata = self.catalog.with_messages(|namespace| {
            namespace
                .snapshot()
                .queues
                .into_iter()
                .find(|value| value.metadata.id.slot as usize == index)
                .map(|value| value.metadata)
        });
        metadata.map_or(LinuxResult::Error(Errno::EINVAL), |value| {
            self.message_stat(abi, value.id, actor, any, output, true)
        })
    }

    fn semaphore_remove(&self, identifier: SysvIdentifier, actor: Credentials, pid: u32, now: u64) -> LinuxResult {
        let Some(id) = SemaphoreId::from_linux_id(identifier.0) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        match self
            .catalog
            .with_semaphores(|namespace| namespace.remove(id, actor, pid, now))
        {
            Ok(()) => LinuxResult::Value(0),
            Err(SemaphoreError::Permission) => LinuxResult::Error(Errno::EPERM),
            Err(error) => LinuxResult::Error(ErrorProjection::semaphore(error)),
        }
    }

    fn semaphore_set(
        &self,
        abi: &SysvAbi<'_, M>,
        identifier: SysvIdentifier,
        source: u64,
        actor: Credentials,
        now: u64,
    ) -> LinuxResult {
        let imported = match abi.import_semaphore_status(source) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let Some(id) = SemaphoreId::from_linux_id(identifier.0) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let owner = Credentials {
            uid: imported.permissions.uid,
            gid: imported.permissions.gid,
        };
        match self.catalog.with_semaphores(|namespace| {
            namespace.set_permissions(id, actor, owner, imported.permissions.mode as u16, now)
        }) {
            Ok(()) => LinuxResult::Value(0),
            Err(SemaphoreError::Permission) => LinuxResult::Error(Errno::EPERM),
            Err(error) => LinuxResult::Error(ControlProjection::semaphore_errno(error)),
        }
    }

    fn semaphore_identifier_stat(
        &self,
        abi: &SysvAbi<'_, M>,
        identifier: SysvIdentifier,
        actor: Credentials,
        output: u64,
    ) -> LinuxResult {
        SemaphoreId::from_linux_id(identifier.0).map_or(LinuxResult::Error(Errno::EINVAL), |id| {
            self.semaphore_stat(abi, id, actor, false, output, false)
        })
    }

    fn semaphore_index_stat(
        &self,
        abi: &SysvAbi<'_, M>,
        index: SysvRawIndex,
        actor: Credentials,
        any: bool,
        output: u64,
    ) -> LinuxResult {
        let Ok(index) = usize::try_from(index.0) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let set = self.catalog.with_semaphores(|namespace| {
            namespace
                .snapshot()
                .sets
                .into_iter()
                .find(|value| value.metadata.id.slot as usize == index)
        });
        set.map_or(LinuxResult::Error(Errno::EINVAL), |value| {
            self.semaphore_stat(abi, value.metadata.id, actor, any, output, true)
        })
    }
}
