use hl_ipc::{
    Credentials, MessageError, MessageQueueId, MessageQueueNamespace, PreparedMessageReceive, SemaphoreError,
    SemaphoreId, SemaphoreOperation,
};
use hl_linux::{Errno, GuestMarshaller, GuestMemory, LinuxResult, MessageReceivePlan, MessageSendPlan, SysvAbi};
use hl_time::{Deadline, Duration};

use super::error_projection::ErrorProjection;
use super::syscalls::RuntimeIpcSyscalls;

impl<M: GuestMemory> RuntimeIpcSyscalls<M> {
    pub(super) fn msgrcv(&self, arguments: [u64; 6]) -> LinuxResult {
        let abi = SysvAbi::new(&self.memory, self.architecture);
        let plan = match abi.msgrcv(
            arguments[0],
            arguments[1],
            arguments[2] as usize,
            arguments[3],
            arguments[4] as u32,
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
        self.catalog.with_messages(|namespace| {
            let prepared = match self.prepare_message_receive(namespace, id, actor, pid, &plan, now) {
                Ok(value) => value,
                Err(result) => return result,
            };
            self.commit_message_receive(&abi, plan.output, prepared)
        })
    }

    fn prepare_message_receive<'a>(
        &self,
        namespace: &'a MessageQueueNamespace,
        id: MessageQueueId,
        actor: Credentials,
        pid: u32,
        plan: &MessageReceivePlan,
        now: u64,
    ) -> Result<PreparedMessageReceive<'a>, LinuxResult> {
        match namespace.prepare_receive(id, actor, pid, plan.message_type, plan.maximum, plan.flags, now) {
            Ok(value) => Ok(value),
            Err(MessageError::NoMessage) if plan.flags & hl_ipc::MSG_NOWAIT == 0 => {
                self.blocking_message_receive(namespace, id, actor, pid, plan, now)
            }
            Err(error) => Err(LinuxResult::Error(ErrorProjection::message(error))),
        }
    }

    pub(super) fn blocking_message_send(
        &self,
        id: MessageQueueId,
        actor: Credentials,
        pid: u32,
        plan: &MessageSendPlan,
        now: u64,
    ) -> LinuxResult {
        let Some(wait) = &self.wait else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let interruption = wait.interruption();
        match self.catalog.with_messages(|namespace| {
            namespace.send_wait(
                id,
                actor,
                pid,
                plan.message_type,
                &plan.bytes,
                &interruption,
                None,
                self.clock.as_ref(),
                now,
            )
        }) {
            Ok(()) => LinuxResult::Value(0),
            Err(MessageError::Removed) => LinuxResult::Error(Errno::EIDRM),
            Err(error) => LinuxResult::Error(ErrorProjection::message(error)),
        }
    }

    pub(super) fn blocking_message_receive<'a>(
        &self,
        namespace: &'a MessageQueueNamespace,
        id: MessageQueueId,
        actor: Credentials,
        pid: u32,
        plan: &MessageReceivePlan,
        now: u64,
    ) -> Result<PreparedMessageReceive<'a>, LinuxResult> {
        let Some(wait) = &self.wait else {
            return Err(LinuxResult::Error(Errno::ENOSYS));
        };
        let interruption = wait.interruption();
        namespace
            .prepare_receive_wait(
                id,
                actor,
                pid,
                plan.message_type,
                plan.maximum,
                plan.flags,
                &interruption,
                None,
                self.clock.as_ref(),
                now,
            )
            .map_err(|error| {
                LinuxResult::Error(match error {
                    MessageError::Removed => Errno::EIDRM,
                    error => ErrorProjection::message(error),
                })
            })
    }

    pub(super) fn commit_message_receive(
        &self,
        abi: &SysvAbi<'_, M>,
        output: u64,
        prepared: PreparedMessageReceive<'_>,
    ) -> LinuxResult {
        let message = prepared.message();
        let staged = match abi.stage_message_receive(output, message.message_type, &message.bytes) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        if let Err(error) = staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            return LinuxResult::Error(error.errno());
        }
        match prepared.commit() {
            Ok(value) => LinuxResult::Value(value.bytes.len() as u64),
            Err(error) => LinuxResult::Error(ErrorProjection::message(error)),
        }
    }

    pub(super) fn blocking_semaphore_operate(
        &self,
        id: SemaphoreId,
        actor: Credentials,
        pid: u32,
        operations: &[SemaphoreOperation],
        timeout: Option<(i64, i64)>,
        now: u64,
    ) -> LinuxResult {
        let Some(wait) = &self.wait else {
            return LinuxResult::Error(Errno::ENOSYS);
        };
        let deadline = match self.deadline(timeout) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let interruption = wait.interruption();
        match self.catalog.with_semaphores(|namespace| {
            namespace.operate_wait(
                id,
                actor,
                pid,
                operations,
                &interruption,
                deadline,
                self.clock.as_ref(),
                now,
            )
        }) {
            Ok(()) => LinuxResult::Value(0),
            Err(SemaphoreError::Again) => LinuxResult::Error(Errno::EAGAIN),
            Err(SemaphoreError::Removed) => LinuxResult::Error(Errno::EIDRM),
            Err(error) => LinuxResult::Error(ErrorProjection::semaphore(error)),
        }
    }

    fn deadline(&self, timeout: Option<(i64, i64)>) -> Result<Option<Deadline>, Errno> {
        let Some((seconds, nanoseconds)) = timeout else {
            return Ok(None);
        };
        let seconds = u64::try_from(seconds).map_err(|_| Errno::EINVAL)?;
        let nanoseconds = u64::try_from(nanoseconds).map_err(|_| Errno::EINVAL)?;
        let duration = seconds
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(nanoseconds))
            .map(Duration::from_nanoseconds)
            .ok_or(Errno::EINVAL)?;
        let current = self.clock.monotonic_now().map_err(|_| Errno::EIO)?;
        Ok(Some(current.deadline_after(duration)))
    }
}
