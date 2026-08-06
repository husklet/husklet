use crate::{Credentials, IpcKey};

pub const MSG_NOWAIT: u32 = 0o4000;
pub const MSG_NOERROR: u32 = 0o10000;
pub const MSG_EXCEPT: u32 = 0o20000;
pub const MSG_COPY: u32 = 0o40000;
pub(crate) const MESSAGE_FLAGS: u32 = MSG_NOWAIT | MSG_NOERROR | MSG_EXCEPT | MSG_COPY;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct QueueId {
    pub slot: u32,
    pub generation: u32,
}
pub type MessageQueueId = QueueId;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub queues: usize,
    pub queue_bytes: usize,
    pub queue_messages: usize,
    pub total_bytes: usize,
    pub total_messages: usize,
    pub message_bytes: usize,
}
pub type MessageLimits = Limits;

impl Default for MessageLimits {
    fn default() -> Self {
        Self {
            queues: 32000,
            queue_bytes: 16384,
            queue_messages: 16384,
            total_bytes: 1 << 30,
            total_messages: 1 << 20,
            message_bytes: 8192,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct MsgGetRequest {
    pub key: IpcKey,
    pub create: bool,
    pub exclusive: bool,
    pub mode: u16,
    pub actor: Credentials,
    pub pid: u32,
    pub now: u64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Receipt {
    pub message_type: i64,
    pub bytes: Vec<u8>,
    pub truncated: bool,
}
pub type MessageReceive = Receipt;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub message_type: i64,
    pub bytes: Vec<u8>,
}
pub type MessageSnapshot = Snapshot;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueMetadata {
    pub id: MessageQueueId,
    pub key: Option<IpcKey>,
    pub owner: Credentials,
    pub creator_uid: u32,
    pub creator_gid: u32,
    pub mode: u16,
    /// Maximum payload bytes retained by this queue.
    ///
    /// This is the domain representation of `SysV` `msg_qbytes`; guest ABI
    /// layouts remain outside `hl-ipc`.
    pub maximum_bytes: usize,
    pub bytes: usize,
    pub messages: usize,
    pub last_send_pid: u32,
    pub last_receive_pid: u32,
    pub created_at: u64,
    pub sent_at: Option<u64>,
    pub received_at: Option<u64>,
    pub changed_at: u64,
}
pub type MessageQueueMetadata = QueueMetadata;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct QueueSnapshot {
    pub metadata: MessageQueueMetadata,
    pub messages: Vec<MessageSnapshot>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NamespaceSnapshot {
    pub generations: Vec<u32>,
    pub queues: Vec<QueueSnapshot>,
}
pub type MessageQueueSnapshot = NamespaceSnapshot;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidArgument,
    NotFound,
    Exists,
    Permission,
    ResourceLimit,
    Again,
    NoMessage,
    TooBig,
    Removed,
    Interrupted,
    TimedOut,
    Clock,
}
pub type MessageError = Error;
