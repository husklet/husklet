use hl_ipc::{
    Credentials, MessageError, MessageQueueId, MessageQueueMetadata, SemaphoreError, SemaphoreId, SemaphoreMetadata,
    SharedMemoryError, SharedMemoryId, SharedMemoryMetadata,
};
use hl_linux::{
    Errno, GuestMarshaller, GuestMemory, IpcPermissions, LinuxResult, MessageInfo, MessageQueueStatus, SemaphoreInfo,
    SemaphoreStatus, SharedMemoryInfo, SharedMemoryStatus, ShmInfo, StagedSysvCopyout, SysvAbi,
};

use super::error_projection::ErrorProjection;
use super::syscalls::RuntimeIpcSyscalls;

impl<M: GuestMemory> RuntimeIpcSyscalls<M> {
    pub(super) fn shared_stat(
        &self,
        abi: &SysvAbi<'_, M>,
        id: SharedMemoryId,
        actor: Credentials,
        any: bool,
        output: u64,
        return_id: bool,
    ) -> LinuxResult {
        let metadata = match self.catalog.with_shared_memory(|namespace| namespace.metadata(id)) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ControlProjection::shared_errno(error)),
        };
        if !any && !Self::permitted(actor, metadata.owner, metadata.creator_uid, metadata.mode, 0o4) {
            return LinuxResult::Error(Errno::EACCES);
        }
        let status = match ControlProjection::shared_status(metadata) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let staged = match abi.stage_shared_status(output, status) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        self.commit_status(staged, return_id.then(|| id.linux_id()).flatten())
    }

    pub(super) fn message_stat(
        &self,
        abi: &SysvAbi<'_, M>,
        id: MessageQueueId,
        actor: Credentials,
        any: bool,
        output: u64,
        return_id: bool,
    ) -> LinuxResult {
        let metadata = match self.catalog.with_messages(|namespace| namespace.metadata(id)) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(ControlProjection::message_errno(error)),
        };
        if !any && !Self::permitted(actor, metadata.owner, metadata.creator_uid, metadata.mode, 0o4) {
            return LinuxResult::Error(Errno::EACCES);
        }
        let status = match ControlProjection::message_status(metadata) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let staged = match abi.stage_message_status(output, status) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        self.commit_status(staged, return_id.then(|| id.linux_id()).flatten())
    }

    pub(super) fn semaphore_stat(
        &self,
        abi: &SysvAbi<'_, M>,
        id: SemaphoreId,
        actor: Credentials,
        any: bool,
        output: u64,
        return_id: bool,
    ) -> LinuxResult {
        let snapshot = self.catalog.with_semaphores(|namespace| {
            namespace
                .snapshot()
                .sets
                .into_iter()
                .find(|value| value.metadata.id == id)
        });
        let Some(set) = snapshot else {
            return LinuxResult::Error(Errno::EINVAL);
        };
        if !any
            && !Self::permitted(
                actor,
                set.metadata.owner,
                set.metadata.creator_uid,
                set.metadata.mode,
                0o4,
            )
        {
            return LinuxResult::Error(Errno::EACCES);
        }
        let status = match ControlProjection::semaphore_status(set.metadata, set.values.len()) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error),
        };
        let staged = match abi.stage_semaphore_status(output, status) {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        self.commit_status(staged, return_id.then(|| id.linux_id()).flatten())
    }

    fn commit_status(&self, staged: StagedSysvCopyout, identifier: Option<i32>) -> LinuxResult {
        match staged.commit(&GuestMarshaller::new(&self.memory, self.architecture)) {
            Ok(()) => LinuxResult::Value(identifier.unwrap_or(0) as u64),
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    pub(super) fn shared_info(&self, abi: &SysvAbi<'_, M>, usage: bool, output: u64) -> LinuxResult {
        let limits = self.catalog.shared_limits();
        let snapshot = self.catalog.with_shared_memory(|namespace| namespace.snapshot());
        let highest = ControlProjection::highest_shared(&snapshot.segments);
        let staged = if usage {
            let bytes = snapshot.segments.iter().map(|value| value.size as u64).sum::<u64>();
            abi.stage_shm_info(
                output,
                ShmInfo {
                    used_identifiers: snapshot.segments.len() as i32,
                    total_pages: bytes.div_ceil(4096),
                    resident_pages: bytes.div_ceil(4096),
                    swapped_pages: 0,
                    swap_attempts: 0,
                    swap_successes: 0,
                },
            )
        } else {
            abi.stage_shared_info(
                output,
                SharedMemoryInfo {
                    maximum_size: limits.segment_bytes as u64,
                    minimum_size: 1,
                    maximum_segments: limits.segments as u64,
                    maximum_process_segments: limits.attachments as u64,
                    maximum_pages: (limits.total_bytes as u64) / 4096,
                },
            )
        };
        ControlProjection::commit_info(self, staged, highest)
    }

    pub(super) fn message_info(&self, abi: &SysvAbi<'_, M>, usage: bool, output: u64) -> LinuxResult {
        let limits = self.catalog.message_limits();
        let snapshot = self.catalog.with_messages(|namespace| namespace.snapshot());
        let highest = snapshot
            .queues
            .iter()
            .map(|value| value.metadata.id.slot)
            .max()
            .unwrap_or(0);
        let bytes = snapshot.queues.iter().map(|value| value.metadata.bytes).sum::<usize>();
        let messages = snapshot
            .queues
            .iter()
            .map(|value| value.metadata.messages)
            .sum::<usize>();
        let values = if usage {
            [
                bytes as i32,
                messages as i32,
                limits.message_bytes as i32,
                snapshot.queues.len() as i32,
                limits.queues as i32,
                16,
                messages as i32,
            ]
        } else {
            [
                0,
                limits.message_bytes as i32,
                limits.message_bytes as i32,
                limits.queue_bytes as i32,
                limits.queues as i32,
                16,
                limits.total_messages as i32,
            ]
        };
        ControlProjection::commit_info(
            self,
            abi.stage_message_info(output, MessageInfo { values, segments: 0 }),
            highest,
        )
    }

    pub(super) fn semaphore_info(&self, abi: &SysvAbi<'_, M>, usage: bool, output: u64) -> LinuxResult {
        let limits = self.catalog.semaphore_limits();
        let snapshot = self.catalog.with_semaphores(|namespace| namespace.snapshot());
        let highest = snapshot
            .sets
            .iter()
            .map(|value| value.metadata.id.slot)
            .max()
            .unwrap_or(0);
        let used = snapshot.sets.iter().map(|value| value.values.len()).sum::<usize>();
        let mut values = [
            limits.sets as i32,
            limits.sets as i32,
            limits.total_semaphores as i32,
            limits.undo_entries as i32,
            limits.set_semaphores as i32,
            limits.operations as i32,
            limits.undo_entries as i32,
            20,
            i32::from(limits.maximum_value),
            i32::from(limits.maximum_value),
        ];
        if usage {
            values[7] = snapshot.sets.len() as i32;
            values[9] = used as i32;
        }
        ControlProjection::commit_info(
            self,
            abi.stage_semaphore_info(output, SemaphoreInfo { values }),
            highest,
        )
    }
}

pub(super) struct ControlProjection;

impl ControlProjection {
    fn permissions(
        key: Option<hl_ipc::IpcKey>,
        owner: Credentials,
        creator_uid: u32,
        creator_gid: u32,
        mode: u16,
        generation: u32,
    ) -> Result<IpcPermissions, Errno> {
        Ok(IpcPermissions {
            key: key.map_or(0, |value| value.0),
            uid: owner.uid,
            gid: owner.gid,
            creator_uid,
            creator_gid,
            mode: u32::from(mode),
            sequence: u16::try_from(generation.checked_sub(1).ok_or(Errno::EINVAL)?).map_err(|_| Errno::EOVERFLOW)?,
        })
    }

    fn shared_status(value: SharedMemoryMetadata) -> Result<SharedMemoryStatus, Errno> {
        Ok(SharedMemoryStatus {
            permissions: Self::permissions(
                value.key,
                value.owner,
                value.creator_uid,
                value.creator_gid,
                value.mode,
                value.id.generation,
            )?,
            size: value.size as u64,
            attached_at: value.attached_at.unwrap_or(0) as i64,
            detached_at: value.detached_at.unwrap_or(0) as i64,
            changed_at: value.changed_at as i64,
            creator_pid: value.creator_pid as i32,
            last_pid: value.last_pid as i32,
            attaches: value.attaches as u64,
        })
    }

    fn message_status(value: MessageQueueMetadata) -> Result<MessageQueueStatus, Errno> {
        Ok(MessageQueueStatus {
            permissions: Self::permissions(
                value.key,
                value.owner,
                value.creator_uid,
                value.creator_gid,
                value.mode,
                value.id.generation,
            )?,
            sent_at: value.sent_at.unwrap_or(0) as i64,
            received_at: value.received_at.unwrap_or(0) as i64,
            changed_at: value.changed_at as i64,
            bytes: value.bytes as u64,
            messages: value.messages as u64,
            maximum_bytes: value.maximum_bytes as u64,
            last_sender: value.last_send_pid as i32,
            last_receiver: value.last_receive_pid as i32,
        })
    }

    fn semaphore_status(value: SemaphoreMetadata, count: usize) -> Result<SemaphoreStatus, Errno> {
        Ok(SemaphoreStatus {
            permissions: Self::permissions(
                value.key,
                value.owner,
                value.creator_uid,
                value.creator_gid,
                value.mode,
                value.id.generation,
            )?,
            operated_at: value.operated_at.unwrap_or(0) as i64,
            changed_at: value.changed_at as i64,
            semaphores: count as u64,
        })
    }

    fn highest_shared(values: &[SharedMemoryMetadata]) -> u32 {
        values.iter().map(|value| value.id.slot).max().unwrap_or(0)
    }

    fn commit_info<M: GuestMemory>(
        runtime: &RuntimeIpcSyscalls<M>,
        staged: Result<StagedSysvCopyout, hl_linux::SysvMarshalError>,
        highest: u32,
    ) -> LinuxResult {
        let staged = match staged {
            Ok(value) => value,
            Err(error) => return LinuxResult::Error(error.errno()),
        };
        match staged.commit(&GuestMarshaller::new(&runtime.memory, runtime.architecture)) {
            Ok(()) => LinuxResult::Value(u64::from(highest)),
            Err(error) => LinuxResult::Error(error.errno()),
        }
    }

    pub(super) const fn shared_errno(error: SharedMemoryError) -> Errno {
        match error {
            SharedMemoryError::Permission => Errno::EACCES,
            SharedMemoryError::NotFound | SharedMemoryError::Removed => Errno::EINVAL,
            other => ErrorProjection::shared_get(other),
        }
    }

    pub(super) const fn message_errno(error: MessageError) -> Errno {
        match error {
            MessageError::Permission => Errno::EACCES,
            MessageError::NotFound | MessageError::Removed => Errno::EINVAL,
            other => ErrorProjection::message(other),
        }
    }

    pub(super) const fn semaphore_errno(error: SemaphoreError) -> Errno {
        match error {
            SemaphoreError::Permission => Errno::EACCES,
            SemaphoreError::NotFound | SemaphoreError::Removed => Errno::EINVAL,
            other => ErrorProjection::semaphore(other),
        }
    }
}
