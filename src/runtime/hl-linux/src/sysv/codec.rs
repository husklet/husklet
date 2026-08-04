use hl_isa::GuestArchitecture;

use super::abi::{Abi, AbiError};
use crate::{
    GuestMemory, IpcPermissions, MessageInfo, MessageQueueStatus, SemaphoreInfo, SemaphoreStatus, SharedMemoryInfo,
    SharedMemoryStatus, ShmInfo, StagedSysvCopyout,
};

impl<'a, M: GuestMemory> Abi<'a, M> {
    pub fn import_permissions(&self, source: u64) -> Result<IpcPermissions, AbiError> {
        Ok(Self::decode_permissions(&self.read(source, 48)?))
    }

    pub fn stage_permissions(&self, output: u64, value: IpcPermissions) -> Result<StagedSysvCopyout, AbiError> {
        let mut bytes = vec![0; 48];
        Self::encode_permissions(&mut bytes, value);
        self.stage(output, bytes)
    }

    pub fn import_shared_status(&self, source: u64) -> Result<SharedMemoryStatus, AbiError> {
        Ok(Self::decode_shared_status(&self.read(source, 112)?))
    }

    pub fn import_semaphore_status(&self, source: u64) -> Result<SemaphoreStatus, AbiError> {
        let length = match self.marshaller.architecture() {
            GuestArchitecture::Aarch64 => 88,
            GuestArchitecture::X86_64 => 104,
        };
        Ok(Self::decode_semaphore_status(
            &self.read(source, length)?,
            self.marshaller.architecture(),
        ))
    }

    pub fn import_message_status(&self, source: u64) -> Result<MessageQueueStatus, AbiError> {
        Ok(Self::decode_message_status(&self.read(source, 120)?))
    }

    pub fn import_semaphore_values(&self, source: u64, count: usize) -> Result<Vec<u16>, AbiError> {
        let bytes = self.read(source, count.checked_mul(2).ok_or(AbiError::Overflow)?)?;
        Ok(bytes
            .chunks_exact(2)
            .map(|item| u16::from_le_bytes([item[0], item[1]]))
            .collect())
    }

    pub fn stage_shared_status(&self, output: u64, value: SharedMemoryStatus) -> Result<StagedSysvCopyout, AbiError> {
        let mut bytes = vec![0; 112];
        Self::encode_permissions(&mut bytes, value.permissions);
        Self::put_u64(&mut bytes, 48, value.size);
        Self::put_i64(&mut bytes, 56, value.attached_at);
        Self::put_i64(&mut bytes, 64, value.detached_at);
        Self::put_i64(&mut bytes, 72, value.changed_at);
        Self::put_i32(&mut bytes, 80, value.creator_pid);
        Self::put_i32(&mut bytes, 84, value.last_pid);
        Self::put_u64(&mut bytes, 88, value.attaches);
        self.stage(output, bytes)
    }

    pub fn stage_semaphore_status(&self, output: u64, value: SemaphoreStatus) -> Result<StagedSysvCopyout, AbiError> {
        let (length, changed, count) = match self.marshaller.architecture() {
            GuestArchitecture::Aarch64 => (88, 56, 64),
            GuestArchitecture::X86_64 => (104, 64, 80),
        };
        let mut bytes = vec![0; length];
        Self::encode_permissions(&mut bytes, value.permissions);
        Self::put_i64(&mut bytes, 48, value.operated_at);
        Self::put_i64(&mut bytes, changed, value.changed_at);
        Self::put_u64(&mut bytes, count, value.semaphores);
        self.stage(output, bytes)
    }

    pub fn stage_message_status(&self, output: u64, value: MessageQueueStatus) -> Result<StagedSysvCopyout, AbiError> {
        let mut bytes = vec![0; 120];
        Self::encode_permissions(&mut bytes, value.permissions);
        Self::put_i64(&mut bytes, 48, value.sent_at);
        Self::put_i64(&mut bytes, 56, value.received_at);
        Self::put_i64(&mut bytes, 64, value.changed_at);
        Self::put_u64(&mut bytes, 72, value.bytes);
        Self::put_u64(&mut bytes, 80, value.messages);
        Self::put_u64(&mut bytes, 88, value.maximum_bytes);
        Self::put_i32(&mut bytes, 96, value.last_sender);
        Self::put_i32(&mut bytes, 100, value.last_receiver);
        self.stage(output, bytes)
    }

    pub fn stage_shared_info(&self, output: u64, value: SharedMemoryInfo) -> Result<StagedSysvCopyout, AbiError> {
        let mut bytes = vec![0; 72];
        for (index, value) in [
            value.maximum_size,
            value.minimum_size,
            value.maximum_segments,
            value.maximum_process_segments,
            value.maximum_pages,
        ]
        .into_iter()
        .enumerate()
        {
            Self::put_u64(&mut bytes, index * 8, value);
        }
        self.stage(output, bytes)
    }

    pub fn stage_shm_info(&self, output: u64, value: ShmInfo) -> Result<StagedSysvCopyout, AbiError> {
        let mut bytes = vec![0; 48];
        Self::put_i32(&mut bytes, 0, value.used_identifiers);
        for (index, value) in [
            value.total_pages,
            value.resident_pages,
            value.swapped_pages,
            value.swap_attempts,
            value.swap_successes,
        ]
        .into_iter()
        .enumerate()
        {
            Self::put_u64(&mut bytes, 8 + index * 8, value);
        }
        self.stage(output, bytes)
    }

    pub fn stage_semaphore_info(&self, output: u64, value: SemaphoreInfo) -> Result<StagedSysvCopyout, AbiError> {
        let mut bytes = vec![0; 40];
        for (index, value) in value.values.into_iter().enumerate() {
            Self::put_i32(&mut bytes, index * 4, value);
        }
        self.stage(output, bytes)
    }

    pub fn stage_message_info(&self, output: u64, value: MessageInfo) -> Result<StagedSysvCopyout, AbiError> {
        let mut bytes = vec![0; 32];
        for (index, value) in value.values.into_iter().enumerate() {
            Self::put_i32(&mut bytes, index * 4, value);
        }
        bytes[28..30].copy_from_slice(&value.segments.to_le_bytes());
        self.stage(output, bytes)
    }

    pub fn stage_message_receive(
        &self,
        output: u64,
        message_type: i64,
        body: &[u8],
    ) -> Result<StagedSysvCopyout, AbiError> {
        let mut bytes = Vec::with_capacity(8 + body.len());
        bytes.extend_from_slice(&message_type.to_le_bytes());
        bytes.extend_from_slice(body);
        self.stage(output, bytes)
    }

    pub fn stage_semaphore_values(&self, output: u64, values: &[u16]) -> Result<StagedSysvCopyout, AbiError> {
        let mut bytes = Vec::with_capacity(values.len() * 2);
        for value in values {
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        self.stage(output, bytes)
    }

    pub(super) fn decode_shared_status(bytes: &[u8]) -> SharedMemoryStatus {
        SharedMemoryStatus {
            permissions: Self::decode_permissions(bytes),
            size: Self::u64(bytes, 48),
            attached_at: Self::i64(bytes, 56),
            detached_at: Self::i64(bytes, 64),
            changed_at: Self::i64(bytes, 72),
            creator_pid: Self::i32(bytes, 80),
            last_pid: Self::i32(bytes, 84),
            attaches: Self::u64(bytes, 88),
        }
    }

    pub(super) fn decode_semaphore_status(bytes: &[u8], architecture: GuestArchitecture) -> SemaphoreStatus {
        let (changed, count) = match architecture {
            GuestArchitecture::Aarch64 => (56, 64),
            GuestArchitecture::X86_64 => (64, 80),
        };
        SemaphoreStatus {
            permissions: Self::decode_permissions(bytes),
            operated_at: Self::i64(bytes, 48),
            changed_at: Self::i64(bytes, changed),
            semaphores: Self::u64(bytes, count),
        }
    }

    pub(super) fn decode_message_status(bytes: &[u8]) -> MessageQueueStatus {
        MessageQueueStatus {
            permissions: Self::decode_permissions(bytes),
            sent_at: Self::i64(bytes, 48),
            received_at: Self::i64(bytes, 56),
            changed_at: Self::i64(bytes, 64),
            bytes: Self::u64(bytes, 72),
            messages: Self::u64(bytes, 80),
            maximum_bytes: Self::u64(bytes, 88),
            last_sender: Self::i32(bytes, 96),
            last_receiver: Self::i32(bytes, 100),
        }
    }

    fn stage(&self, output: u64, bytes: Vec<u8>) -> Result<StagedSysvCopyout, AbiError> {
        self.preflight(output, bytes.len())?;
        Ok(StagedSysvCopyout {
            destination: output,
            bytes,
        })
    }

    fn encode_permissions(bytes: &mut [u8], value: IpcPermissions) {
        Self::put_i32(bytes, 0, value.key);
        for (offset, value) in [
            (4, value.uid),
            (8, value.gid),
            (12, value.creator_uid),
            (16, value.creator_gid),
            (20, value.mode),
        ] {
            bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
        }
        bytes[24..26].copy_from_slice(&value.sequence.to_le_bytes());
    }

    fn decode_permissions(bytes: &[u8]) -> IpcPermissions {
        IpcPermissions {
            key: Self::i32(bytes, 0),
            uid: Self::u32(bytes, 4),
            gid: Self::u32(bytes, 8),
            creator_uid: Self::u32(bytes, 12),
            creator_gid: Self::u32(bytes, 16),
            mode: Self::u32(bytes, 20),
            sequence: u16::from_le_bytes(bytes[24..26].try_into().expect("two bytes")),
        }
    }

    fn put_u64(bytes: &mut [u8], offset: usize, value: u64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    fn put_i64(bytes: &mut [u8], offset: usize, value: i64) {
        bytes[offset..offset + 8].copy_from_slice(&value.to_le_bytes());
    }
    fn put_i32(bytes: &mut [u8], offset: usize, value: i32) {
        bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
    }
    fn u64(bytes: &[u8], offset: usize) -> u64 {
        u64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("eight bytes"))
    }
    fn i64(bytes: &[u8], offset: usize) -> i64 {
        i64::from_le_bytes(bytes[offset..offset + 8].try_into().expect("eight bytes"))
    }
    fn u32(bytes: &[u8], offset: usize) -> u32 {
        u32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
    }
    fn i32(bytes: &[u8], offset: usize) -> i32 {
        i32::from_le_bytes(bytes[offset..offset + 4].try_into().expect("four bytes"))
    }
}
