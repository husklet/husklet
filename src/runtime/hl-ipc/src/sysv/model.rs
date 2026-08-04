use hl_memory::{SharedBackingRef, SharedError, SharedObjectId};

pub const IPC_PRIVATE: IpcKey = IpcKey(0);
pub const SHM_RDONLY: u32 = 0x1000;
pub const SHM_RND: u32 = 0x2000;
pub const SHM_REMAP: u32 = 0x4000;
pub const SHM_EXEC: u32 = 0x8000;
pub(crate) const ATTACH_FLAGS: u32 = SHM_RDONLY | SHM_RND | SHM_REMAP | SHM_EXEC;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct IpcKey(pub i32);

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SharedMemoryId {
    pub slot: u32,
    pub generation: u32,
}

/// The page-residency control operation requested for a shared-memory segment.
///
/// Husklet does not currently wire guest pages for either operation, but keeps
/// the intent typed so authorization cannot accidentally collapse unlock into
/// a different control command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SharedMemoryLockIntent {
    Lock,
    Unlock,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Credentials {
    pub uid: u32,
    pub gid: u32,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SharedMemoryLimits {
    pub segments: usize,
    pub segment_bytes: usize,
    pub total_bytes: usize,
    pub attachments: usize,
}

impl Default for SharedMemoryLimits {
    fn default() -> Self {
        Self {
            segments: 4096,
            segment_bytes: 1 << 30,
            total_bytes: 1 << 32,
            attachments: 1 << 20,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ShmGetRequest {
    pub key: IpcKey,
    pub size: usize,
    pub create: bool,
    pub exclusive: bool,
    pub mode: u16,
    pub actor: Credentials,
    pub pid: u32,
    pub now: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AttachPlan {
    pub segment: SharedMemoryId,
    pub backing: SharedBackingRef,
    pub read_only: bool,
    pub executable: bool,
    pub round_address: bool,
    pub replace: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InheritedAttachment {
    pub parent: u64,
    pub child: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SharedMemoryMetadata {
    pub id: SharedMemoryId,
    pub key: Option<IpcKey>,
    pub backing: SharedObjectId,
    pub size: usize,
    pub owner: Credentials,
    pub creator_uid: u32,
    pub creator_gid: u32,
    pub mode: u16,
    pub creator_pid: u32,
    pub last_pid: u32,
    pub attaches: usize,
    pub marked_for_removal: bool,
    pub created_at: u64,
    pub attached_at: Option<u64>,
    pub detached_at: Option<u64>,
    pub changed_at: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SharedMemorySnapshot {
    pub generations: Vec<u32>,
    pub segments: Vec<SharedMemoryMetadata>,
    pub attachments: Vec<(u64, SharedMemoryId, u32)>,
    pub next_attachment: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SharedMemoryError {
    InvalidArgument,
    NotFound,
    Exists,
    Permission,
    ResourceLimit,
    Size,
    Removed,
    Shared(SharedError),
}

impl From<SharedError> for SharedMemoryError {
    fn from(error: SharedError) -> Self {
        Self::Shared(error)
    }
}
