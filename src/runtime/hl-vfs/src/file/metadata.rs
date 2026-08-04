/// Stable filesystem object identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct Identity {
    pub device: u64,
    pub inode: u64,
}

/// Host-neutral filesystem object kind.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Kind {
    Regular,
    Directory,
    Symlink,
    Character,
    Block,
    Fifo,
    Socket,
}

/// Permission and special mode bits, excluding the file-kind encoding.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
#[repr(transparent)]
pub struct Permissions(u16);

impl Permissions {
    pub const SET_USER_ID: u16 = 0o4000;
    pub const SET_GROUP_ID: u16 = 0o2000;
    pub const STICKY: u16 = 0o1000;
    pub const OWNER_READ: u16 = 0o0400;
    pub const OWNER_WRITE: u16 = 0o0200;
    pub const OWNER_EXECUTE: u16 = 0o0100;
    pub const GROUP_READ: u16 = 0o0040;
    pub const GROUP_WRITE: u16 = 0o0020;
    pub const GROUP_EXECUTE: u16 = 0o0010;
    pub const OTHER_READ: u16 = 0o0004;
    pub const OTHER_WRITE: u16 = 0o0002;
    pub const OTHER_EXECUTE: u16 = 0o0001;
    pub const MASK: u16 = 0o7777;

    #[must_use]
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits & Self::MASK)
    }

    #[must_use]
    pub const fn bits(self) -> u16 {
        self.0
    }

    #[must_use]
    pub const fn any_execute(self) -> bool {
        self.0 & 0o0111 != 0
    }

    pub(crate) const fn class_bits(self, shift: u16) -> u16 {
        (self.0 >> shift) & 0o7
    }
}

/// Signed seconds plus a normalized nanosecond fraction.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Timestamp {
    pub seconds: i64,
    pub nanoseconds: u32,
}

impl Timestamp {
    pub fn new(seconds: i64, nanoseconds: u32) -> Result<Self, MetadataError> {
        if nanoseconds >= 1_000_000_000 {
            return Err(MetadataError::InvalidTimestamp);
        }
        Ok(Self { seconds, nanoseconds })
    }
}

/// Pointer-free metadata owned by the VFS domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Metadata {
    pub identity: Identity,
    pub kind: Kind,
    pub permissions: Permissions,
    pub links: u64,
    pub user: u32,
    pub group: u32,
    pub special_device: u64,
    pub size: u64,
    pub blocks_512: u64,
    pub accessed: Timestamp,
    pub modified: Timestamp,
    pub changed: Timestamp,
}

/// Invalid neutral metadata.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MetadataError {
    InvalidTimestamp,
    InvalidLinkCount,
}

impl Metadata {
    pub fn validate(&self) -> Result<(), MetadataError> {
        if self.links == 0 {
            return Err(MetadataError::InvalidLinkCount);
        }
        for timestamp in [self.accessed, self.modified, self.changed] {
            Timestamp::new(timestamp.seconds, timestamp.nanoseconds)?;
        }
        Ok(())
    }
}
