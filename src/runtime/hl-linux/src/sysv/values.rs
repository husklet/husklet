pub const IPC_CREAT: u32 = 0o1000;
pub const IPC_EXCL: u32 = 0o2000;
pub const IPC_NOWAIT: u32 = 0o4000;
pub const SEM_UNDO: u16 = 0x1000;
pub const SHM_RDONLY: u32 = 0o10000;
pub const SHM_RND: u32 = 0o20000;
pub const SHM_REMAP: u32 = 0x4000;
pub const SHM_EXEC: u32 = 0x8000;
pub const MSG_NOERROR: u32 = 0o10000;
pub const MSG_EXCEPT: u32 = 0o20000;
pub const MSG_NOWAIT: u32 = IPC_NOWAIT;
pub const IPC_RMID: u32 = 0;
pub const IPC_SET: u32 = 1;
pub const IPC_STAT: u32 = 2;
pub const IPC_INFO: u32 = 3;
pub const SHM_LOCK: u32 = 11;
pub const SHM_UNLOCK: u32 = 12;
pub const SHM_STAT: u32 = 13;
pub const SHM_INFO: u32 = 14;
pub const SHM_STAT_ANY: u32 = 15;
pub const MSG_STAT: u32 = 11;
pub const MSG_INFO: u32 = 12;
pub const MSG_STAT_ANY: u32 = 13;
pub const GETPID: u32 = 11;
pub const GETVAL: u32 = 12;
pub const GETALL: u32 = 13;
pub const GETNCNT: u32 = 14;
pub const GETZCNT: u32 = 15;
pub const SETVAL: u32 = 16;
pub const SETALL: u32 = 17;
pub const SEM_STAT: u32 = 18;
pub const SEM_INFO: u32 = 19;
pub const SEM_STAT_ANY: u32 = 20;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IpcCommand {
    Remove,
    Set,
    Stat,
    Info,
    Lock,
    Unlock,
    ObjectStat,
    ObjectInfo,
    ObjectStatAny,
    GetPid,
    GetValue,
    GetAll,
    GetDecrementWaiters,
    GetZeroWaiters,
    SetValue,
    SetAll,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IpcPermissions {
    pub key: i32,
    pub uid: u32,
    pub gid: u32,
    pub creator_uid: u32,
    pub creator_gid: u32,
    pub mode: u32,
    pub sequence: u16,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SharedMemoryStatus {
    pub permissions: IpcPermissions,
    pub size: u64,
    pub attached_at: i64,
    pub detached_at: i64,
    pub changed_at: i64,
    pub creator_pid: i32,
    pub last_pid: i32,
    pub attaches: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SemaphoreStatus {
    pub permissions: IpcPermissions,
    pub operated_at: i64,
    pub changed_at: i64,
    pub semaphores: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MessageQueueStatus {
    pub permissions: IpcPermissions,
    pub sent_at: i64,
    pub received_at: i64,
    pub changed_at: i64,
    pub bytes: u64,
    pub messages: u64,
    pub maximum_bytes: u64,
    pub last_sender: i32,
    pub last_receiver: i32,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SharedMemoryInfo {
    pub maximum_size: u64,
    pub minimum_size: u64,
    pub maximum_segments: u64,
    pub maximum_process_segments: u64,
    pub maximum_pages: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ShmInfo {
    pub used_identifiers: i32,
    pub total_pages: u64,
    pub resident_pages: u64,
    pub swapped_pages: u64,
    pub swap_attempts: u64,
    pub swap_successes: u64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SemaphoreInfo {
    pub values: [i32; 10],
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct MessageInfo {
    pub values: [i32; 7],
    pub segments: u16,
}
