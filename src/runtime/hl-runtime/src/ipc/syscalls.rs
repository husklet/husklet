use std::sync::Arc;

use hl_descriptor::DescriptorTable;
use hl_ipc::{
    Credentials, IpcCatalog, IpcKey, MessageError, MessageQueueId, MqNamespace, MsgGetRequest, SemGetRequest,
    SemaphoreError, SemaphoreId, SemaphoreOperation as DomainSemOperation, ShmGetRequest,
};
use hl_isa::GuestArchitecture;
use hl_linux::{
    Errno, GuestMarshaller, GuestMemory, IpcCommand, IpcSyscalls, LinuxResult, SemaphoreControlPlan, SyscallOperation,
    SysvAbi,
};
use hl_task::{ProcessId, TaskRegistry};
use hl_time::Clock;

use super::error_projection::ErrorProjection;
use crate::{BlockingWait, MemoryPort};

/// Composes Linux SysV ABI marshalling with the guest-independent IPC domains.
pub struct RuntimeIpcSyscalls<M: GuestMemory> {
    pub(super) catalog: Arc<IpcCatalog>,
    pub(super) tasks: Arc<TaskRegistry>,
    pub(super) process: ProcessId,
    pub(super) memory: M,
    pub(super) architecture: GuestArchitecture,
    pub(super) clock: Arc<dyn Clock>,
    pub(super) shared_memory: Option<Arc<dyn MemoryPort>>,
    pub(super) wait: Option<Arc<dyn BlockingWait>>,
    pub(super) posix: Option<Arc<MqNamespace>>,
    pub(super) descriptors: Option<Arc<DescriptorTable>>,
}

impl<M: GuestMemory> RuntimeIpcSyscalls<M> {
    pub fn new(
        catalog: Arc<IpcCatalog>,
        tasks: Arc<TaskRegistry>,
        process: ProcessId,
        memory: M,
        architecture: GuestArchitecture,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            catalog,
            tasks,
            process,
            memory,
            architecture,
            clock,
            shared_memory: None,
            wait: None,
            posix: None,
            descriptors: None,
        }
    }

    #[must_use]
    pub fn with_posix_queues(mut self, namespace: Arc<MqNamespace>, descriptors: Arc<DescriptorTable>) -> Self {
        self.posix = Some(namespace);
        self.descriptors = Some(descriptors);
        self
    }

    #[must_use]
    pub fn with_memory_port(mut self, shared_memory: Arc<dyn MemoryPort>) -> Self {
        self.shared_memory = Some(shared_memory);
        self
    }

    #[must_use]
    pub fn with_wait_port(mut self, wait: Arc<dyn BlockingWait>) -> Self {
        self.wait = Some(wait);
        self
    }

    pub(super) fn context(&self) -> Result<(Credentials, u32, u64), Errno> {
        let snapshot = self
            .tasks
            .snapshot()
            .processes
            .into_iter()
            .find(|value| value.id == self.process)
            .ok_or(Errno::ESRCH)?;
        let now = self.clock.realtime_now().map_err(|_| Errno::EIO)?.seconds();
        Ok((
            Credentials {
                uid: snapshot.credentials.effective_user,
                gid: snapshot.credentials.effective_group,
            },
            self.process.number(),
            now,
        ))
    }

    pub(super) fn permitted(actor: Credentials, owner: Credentials, creator_uid: u32, mode: u16, want: u16) -> bool {
        if actor.uid == 0 {
            return true;
        }
        let shift = if actor.uid == owner.uid || actor.uid == creator_uid {
            6
        } else if actor.gid == owner.gid {
            3
        } else {
            0
        };
        ((mode >> shift) & want) == want
    }

    fn shmget(&self, arguments: [u64; 6]) -> LinuxResult {
        let plan = SysvAbi::<M>::shmget(arguments[0], arguments[1], arguments[2] as u32);
        let (actor, pid, now) = match self.context() {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let Ok(size) = usize::try_from(plan.size) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let result = self.catalog.with_shared_memory(|namespace| {
            namespace.shmget(ShmGetRequest {
                key: IpcKey(plan.key),
                size,
                create: plan.create,
                exclusive: plan.exclusive,
                mode: plan.mode,
                actor,
                pid,
                now,
            })
        });
        match result {
            Ok(id) => id.linux_id().map_or(LinuxResult::Error(Errno::EOVERFLOW), |value| {
                LinuxResult::Value(value as u64)
            }),
            Err(error) => LinuxResult::Error(ErrorProjection::shared_get(error)),
        }
    }

    fn msgget(&self, arguments: [u64; 6]) -> LinuxResult {
        let plan = SysvAbi::<M>::msgget(arguments[0], arguments[1] as u32);
        let (actor, pid, now) = match self.context() {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let result = self.catalog.with_messages(|namespace| {
            namespace.msgget(MsgGetRequest {
                key: IpcKey(plan.key),
                create: plan.create,
                exclusive: plan.exclusive,
                mode: plan.mode,
                actor,
                pid,
                now,
            })
        });
        match result {
            Ok(id) => id.linux_id().map_or(LinuxResult::Error(Errno::EOVERFLOW), |value| {
                LinuxResult::Value(value as u64)
            }),
            Err(error) => LinuxResult::Error(ErrorProjection::message_get(error)),
        }
    }

    fn msgsnd(&self, arguments: [u64; 6]) -> LinuxResult {
        let plan = match SysvAbi::new(&self.memory, self.architecture).msgsnd(
            arguments[0],
            arguments[1],
            arguments[2] as usize,
            arguments[3] as u32,
        ) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let Some(id) = MessageQueueId::from_linux_id(plan.identifier.0) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let (actor, pid, now) = match self.context() {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let result = self
            .catalog
            .with_messages(|namespace| namespace.send(id, actor, pid, plan.message_type, &plan.bytes, 0, now));
        match result {
            Ok(()) => LinuxResult::Value(0),
            Err(MessageError::Again) if !plan.nowait => self.blocking_message_send(id, actor, pid, &plan, now),
            Err(error) => LinuxResult::Error(ErrorProjection::message(error)),
        }
    }

    fn semget(&self, arguments: [u64; 6]) -> LinuxResult {
        let plan = SysvAbi::<M>::semget(arguments[0], arguments[1], arguments[2] as u32);
        let Ok(count) = usize::try_from(plan.semaphores) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let (actor, pid, now) = match self.context() {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let result = self.catalog.with_semaphores(|namespace| {
            namespace.semget(SemGetRequest {
                key: IpcKey(plan.key),
                semaphores: count,
                create: plan.create,
                exclusive: plan.exclusive,
                mode: plan.mode,
                actor,
                pid,
                now,
            })
        });
        match result {
            Ok(id) => id.linux_id().map_or(LinuxResult::Error(Errno::EOVERFLOW), |value| {
                LinuxResult::Value(value as u64)
            }),
            Err(error) => LinuxResult::Error(ErrorProjection::semaphore_get(error)),
        }
    }

    fn semop(&self, arguments: [u64; 6], timed: bool) -> LinuxResult {
        let plan = match SysvAbi::new(&self.memory, self.architecture).semop(
            arguments[0],
            arguments[1],
            arguments[2] as usize,
            timed.then_some(arguments[3]),
        ) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let Some(id) = SemaphoreId::from_linux_id(plan.identifier.0) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let operations = plan
            .operations
            .iter()
            .map(|value| DomainSemOperation {
                index: value.index,
                delta: i32::from(value.delta),
                flags: value.flags,
            })
            .collect::<Vec<_>>();
        let (actor, pid, now) = match self.context() {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let result = self
            .catalog
            .with_semaphores(|namespace| namespace.operate(id, actor, pid, &operations, now));
        match result {
            Ok(()) => LinuxResult::Value(0),
            Err(SemaphoreError::Again) if operations.iter().any(|value| value.flags & hl_ipc::IPC_NOWAIT == 0) => {
                self.blocking_semaphore_operate(id, actor, pid, &operations, plan.timeout, now)
            }
            Err(error) => LinuxResult::Error(ErrorProjection::semaphore(error)),
        }
    }

    fn semctl(&self, arguments: [u64; 6]) -> LinuxResult {
        let abi = SysvAbi::new(&self.memory, self.architecture);
        let plan = match abi.semctl(arguments[0], arguments[1], arguments[2] as u32, arguments[3]) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        let (actor, pid, now) = match self.context() {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        if let Some(result) = self.semctl_control(&abi, plan.clone(), actor, pid, now) {
            return result;
        }
        match plan {
            SemaphoreControlPlan::Scalar {
                identifier,
                index,
                command,
                value,
            } => self.semaphore_scalar(identifier, index, command, value, actor, pid, now),
            SemaphoreControlPlan::Array {
                identifier,
                command,
                address,
            } => self.semaphore_array(identifier, command, address, actor, pid, now),
            _ => LinuxResult::Error(Errno::ENOSYS),
        }
    }

    fn semaphore_array(
        &self,
        identifier: hl_linux::SysvIdentifier,
        command: IpcCommand,
        address: u64,
        actor: Credentials,
        pid: u32,
        now: u64,
    ) -> LinuxResult {
        let Some(id) = SemaphoreId::from_linux_id(identifier.0) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        match command {
            IpcCommand::GetAll => self.semaphore_get_all(id, address, actor),
            IpcCommand::SetAll => self.semaphore_set_all(id, address, actor, pid, now),
            _ => LinuxResult::Error(Errno::EINVAL),
        }
    }

    fn semaphore_get_all(&self, id: SemaphoreId, address: u64, actor: Credentials) -> LinuxResult {
        let values = match self.catalog.with_semaphores(|namespace| namespace.get_all(id, actor)) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ErrorProjection::semaphore(error)),
        };
        let staged = match SysvAbi::new(&self.memory, self.architecture).stage_semaphore_values(address, &values) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    fn semaphore_set_all(&self, id: SemaphoreId, address: u64, actor: Credentials, pid: u32, now: u64) -> LinuxResult {
        let metadata = self
            .catalog
            .with_semaphores(|namespace| namespace.snapshot().sets.into_iter().find(|set| set.metadata.id == id));
        let Some(set) = metadata else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        if !Self::permitted(
            actor,
            set.metadata.owner,
            set.metadata.creator_uid,
            set.metadata.mode,
            0o2,
        ) {
            return LinuxResult::Error(Errno::EACCES);
        }
        let values =
            match SysvAbi::new(&self.memory, self.architecture).import_semaphore_values(address, set.values.len()) {
                Ok(value) => value,
                Err(error) => return LinuxResult::Error(error.errno()),
            };
        match self
            .catalog
            .with_semaphores(|namespace| namespace.set_all(id, &values, actor, pid, now))
        {
            Ok(()) => LinuxResult::Value(0),
            Err(error) => LinuxResult::Error(ErrorProjection::semaphore(error)),
        }
    }

    fn semaphore_scalar(
        &self,
        identifier: hl_linux::SysvIdentifier,
        index: i32,
        command: IpcCommand,
        value: i32,
        actor: Credentials,
        pid: u32,
        now: u64,
    ) -> LinuxResult {
        let Some(id) = SemaphoreId::from_linux_id(identifier.0) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let Ok(index) = usize::try_from(index) else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        let result = match command {
            IpcCommand::GetPid => self.semaphore_pid(id, index, actor),
            IpcCommand::GetValue => self.semaphore_value(id, index, actor),
            IpcCommand::GetDecrementWaiters => self.semaphore_waiters(id, index, actor).map(|value| value.0 as u64),
            IpcCommand::GetZeroWaiters => self.semaphore_waiters(id, index, actor).map(|value| value.1 as u64),
            IpcCommand::SetValue => self.semaphore_set_value(id, index, value, actor, pid, now),
            _ => Err(SemaphoreError::InvalidArgument),
        };
        match result {
            Ok(value) => LinuxResult::Value(value),
            Err(error) => LinuxResult::Error(ErrorProjection::semaphore(error)),
        }
    }

    fn semaphore_pid(&self, id: SemaphoreId, index: usize, actor: Credentials) -> Result<u64, SemaphoreError> {
        self.catalog
            .with_semaphores(|namespace| namespace.get_pid(id, index, actor).map(u64::from))
    }

    fn semaphore_value(&self, id: SemaphoreId, index: usize, actor: Credentials) -> Result<u64, SemaphoreError> {
        self.catalog
            .with_semaphores(|namespace| namespace.get_value(id, index, actor).map(u64::from))
    }

    fn semaphore_waiters(
        &self,
        id: SemaphoreId,
        index: usize,
        actor: Credentials,
    ) -> Result<(usize, usize), SemaphoreError> {
        self.catalog
            .with_semaphores(|namespace| namespace.get_wait_counts(id, index, actor))
    }

    fn semaphore_set_value(
        &self,
        id: SemaphoreId,
        index: usize,
        value: i32,
        actor: Credentials,
        pid: u32,
        now: u64,
    ) -> Result<u64, SemaphoreError> {
        let value = u16::try_from(value).map_err(|_| SemaphoreError::Range)?;
        self.catalog
            .with_semaphores(|namespace| namespace.set_value(id, index, value, actor, pid, now).map(|()| 0))
    }
}

impl<M: GuestMemory> IpcSyscalls for RuntimeIpcSyscalls<M> {
    fn handle(&mut self, operation: SyscallOperation, arguments: [u64; 6]) -> LinuxResult {
        let result = match operation.name {
            "shmget" => self.shmget(arguments),
            "msgget" => self.msgget(arguments),
            "msgsnd" => self.msgsnd(arguments),
            "msgrcv" => self.msgrcv(arguments),
            "semget" => self.semget(arguments),
            "semop" => self.semop(arguments, false),
            "semtimedop" => self.semop(arguments, true),
            "shmctl" => self.shmctl(arguments),
            "msgctl" => self.msgctl(arguments),
            "semctl" => self.semctl(arguments),
            "shmat" => self.shmat(arguments),
            "shmdt" => self.shmdt(arguments),
            "mq_open" => self.mq_open(arguments),
            "mq_unlink" => self.mq_unlink(arguments),
            "mq_timedsend" => self.mq_timedsend(arguments),
            "mq_timedreceive" => self.mq_timedreceive(arguments),
            "mq_notify" => self.mq_notify(arguments),
            "mq_getsetattr" => self.mq_getsetattr(arguments),
            _ => LinuxResult::Error(Errno::ENOSYS),
        };
        hl_log::hl_debug!(
            hl_log::tag::IPC,
            "{} key={:#x} argument={:#x} flags={:#x} result={:#x}",
            operation.name,
            arguments[0],
            arguments[1],
            arguments[2],
            result.encode()
        );
        result
    }
}
