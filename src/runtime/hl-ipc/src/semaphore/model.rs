use crate::{Credentials, IpcKey};

pub const IPC_NOWAIT: u16 = 0o4000;
pub const SEM_UNDO: u16 = 0x1000;
pub(crate) const SEM_FLAGS: u16 = IPC_NOWAIT | SEM_UNDO;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct Id {
    pub slot: u32,
    pub generation: u32,
}
pub type SemaphoreId = Id;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub sets: usize,
    pub set_semaphores: usize,
    pub total_semaphores: usize,
    pub maximum_value: u16,
    pub operations: usize,
    pub undo_entries: usize,
}
pub type SemaphoreLimits = Limits;

impl Default for SemaphoreLimits {
    fn default() -> Self {
        Self {
            sets: 32000,
            set_semaphores: 32000,
            total_semaphores: 1 << 20,
            maximum_value: 32767,
            operations: 500,
            undo_entries: 1 << 20,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SemGetRequest {
    pub key: IpcKey,
    pub semaphores: usize,
    pub create: bool,
    pub exclusive: bool,
    pub mode: u16,
    pub actor: Credentials,
    pub pid: u32,
    pub now: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Operation {
    pub index: u16,
    pub delta: i32,
    pub flags: u16,
}
pub type SemaphoreOperation = Operation;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetMetadata {
    pub id: SemaphoreId,
    pub key: Option<IpcKey>,
    pub owner: Credentials,
    pub creator_uid: u32,
    pub creator_gid: u32,
    pub mode: u16,
    pub last_pid: u32,
    pub created_at: u64,
    pub operated_at: Option<u64>,
    pub changed_at: u64,
}
pub type SemaphoreMetadata = SetMetadata;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SetSnapshot {
    pub metadata: SemaphoreMetadata,
    pub values: Vec<u16>,
    pub last_pids: Vec<u32>,
}
pub type SemaphoreSetSnapshot = SetSnapshot;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub generations: Vec<u32>,
    pub sets: Vec<SemaphoreSetSnapshot>,
    pub undo: Vec<(u32, SemaphoreId, u16, i32)>,
}
pub type SemaphoreSnapshot = Snapshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidArgument,
    NotFound,
    Exists,
    Permission,
    ResourceLimit,
    Range,
    Again,
    Removed,
    Interrupted,
    TimedOut,
    Clock,
}
pub type SemaphoreError = Error;
