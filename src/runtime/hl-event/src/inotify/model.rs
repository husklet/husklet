pub(crate) const INOTIFY_HEADER_SIZE: usize = 16;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct Mask(u32);
pub type InotifyMask = Mask;

impl InotifyMask {
    pub const ACCESS: u32 = 0x0000_0001;
    pub const MODIFY: u32 = 0x0000_0002;
    pub const ATTRIB: u32 = 0x0000_0004;
    pub const CLOSE_WRITE: u32 = 0x0000_0008;
    pub const CLOSE_NOWRITE: u32 = 0x0000_0010;
    pub const OPEN: u32 = 0x0000_0020;
    pub const MOVED_FROM: u32 = 0x0000_0040;
    pub const MOVED_TO: u32 = 0x0000_0080;
    pub const CREATE: u32 = 0x0000_0100;
    pub const DELETE: u32 = 0x0000_0200;
    pub const DELETE_SELF: u32 = 0x0000_0400;
    pub const MOVE_SELF: u32 = 0x0000_0800;
    pub const UNMOUNT: u32 = 0x0000_2000;
    pub const QUEUE_OVERFLOW: u32 = 0x0000_4000;
    pub const IGNORED: u32 = 0x0000_8000;
    pub const ONLY_DIRECTORY: u32 = 0x0100_0000;
    pub const DONT_FOLLOW: u32 = 0x0200_0000;
    pub const EXCLUDE_UNLINKED: u32 = 0x0400_0000;
    pub const MASK_CREATE: u32 = 0x1000_0000;
    pub const MASK_ADD: u32 = 0x2000_0000;
    pub const IS_DIRECTORY: u32 = 0x4000_0000;
    pub const ONESHOT: u32 = 0x8000_0000;
    pub const EVENT_BITS: u32 = 0x0000_0fff;
    pub const OPTION_BITS: u32 = Self::ONLY_DIRECTORY
        | Self::DONT_FOLLOW
        | Self::EXCLUDE_UNLINKED
        | Self::MASK_CREATE
        | Self::MASK_ADD
        | Self::ONESHOT;
    pub const ALLOWED_WATCH_BITS: u32 = Self::EVENT_BITS | Self::OPTION_BITS;

    #[must_use]
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    #[must_use]
    pub const fn bits(self) -> u32 {
        self.0
    }

    #[must_use]
    pub const fn contains(self, bit: u32) -> bool {
        self.0 & bit != 0
    }

    pub(crate) const fn valid_watch(self) -> bool {
        self.0 & Self::EVENT_BITS != 0 && self.0 & !Self::ALLOWED_WATCH_BITS == 0
    }

    pub(crate) const fn source_bits(self) -> Self {
        Self(self.0 & !Self::MASK_ADD & !Self::MASK_CREATE)
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct WatchNodeIdentity {
    pub device: u64,
    pub object: u64,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
#[repr(transparent)]
pub struct WatchPathIdentity(pub u64);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WatchBinding {
    pub node: WatchNodeIdentity,
    pub path: WatchPathIdentity,
    pub is_directory: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WatchRequest<'a> {
    pub path: &'a [u8],
    pub mask: InotifyMask,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchSourceEvent {
    pub source_token: u64,
    pub mask: InotifyMask,
    pub cookie: u32,
    pub name: Vec<u8>,
    pub unlinked_child: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WatchSourceError {
    NotFound,
    NotDirectory,
    NameTooLong,
    PermissionDenied,
    AlreadyExists,
    ResourceLimit,
    Interrupted,
    NotSupported,
    Failed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Limits {
    pub watches: usize,
    pub queued_events: usize,
    pub queued_bytes: usize,
    pub name_bytes: usize,
}
pub type InotifyLimits = Limits;

impl Default for InotifyLimits {
    fn default() -> Self {
        Self {
            watches: 8_192,
            queued_events: 16_384,
            queued_bytes: 4 * 1_024 * 1_024,
            name_bytes: 255,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    InvalidArgument,
    WouldBlock,
    AlreadyExists,
    NotFound,
    NotDirectory,
    NameTooLong,
    PermissionDenied,
    ResourceLimit,
    Interrupted,
    NotSupported,
    Retired,
    SourceFailed,
}
pub type InotifyError = Error;

impl From<WatchSourceError> for InotifyError {
    fn from(error: WatchSourceError) -> Self {
        match error {
            WatchSourceError::NotFound => Self::NotFound,
            WatchSourceError::NotDirectory => Self::NotDirectory,
            WatchSourceError::NameTooLong => Self::NameTooLong,
            WatchSourceError::PermissionDenied => Self::PermissionDenied,
            WatchSourceError::AlreadyExists => Self::AlreadyExists,
            WatchSourceError::ResourceLimit => Self::ResourceLimit,
            WatchSourceError::Interrupted => Self::Interrupted,
            WatchSourceError::NotSupported => Self::NotSupported,
            WatchSourceError::Failed => Self::SourceFailed,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EventSnapshot {
    pub watch_descriptor: i32,
    pub mask: InotifyMask,
    pub cookie: u32,
    pub name: Vec<u8>,
}
pub type InotifyEventSnapshot = EventSnapshot;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct WatchSnapshot {
    pub watch_descriptor: i32,
    pub generation: u32,
    pub binding: WatchBinding,
    pub mask: InotifyMask,
}
pub type InotifyWatchSnapshot = WatchSnapshot;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Snapshot {
    pub limits: InotifyLimits,
    pub nonblocking: bool,
    pub next_cookie: u32,
    pub overflow_queued: bool,
    pub watch_generations: Vec<u32>,
    pub watches: Vec<InotifyWatchSnapshot>,
    pub queue: Vec<InotifyEventSnapshot>,
}
pub type InotifySnapshot = Snapshot;

pub type Status = crate::EventStatus;
pub type InotifyStatus = Status;
