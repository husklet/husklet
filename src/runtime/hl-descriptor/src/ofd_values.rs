/// Host-neutral timestamp reported by an open description.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OfdTimestamp {
    pub seconds: i64,
    pub nanoseconds: u32,
}

/// Host-neutral metadata. No guest ABI padding or host `stat` layout is retained.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfdMetadata {
    pub device: u64,
    pub inode: u64,
    pub kind: u8,
    pub permissions: u16,
    pub links: u64,
    pub user: u32,
    pub group: u32,
    pub special_device: u64,
    pub size: u64,
    pub blocks_512: u64,
    pub block_size: u32,
    pub accessed: OfdTimestamp,
    pub modified: OfdTimestamp,
    pub changed: OfdTimestamp,
}

/// One directory result with an opaque continuation cookie and byte-exact name.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OfdDirectoryEntry {
    pub inode: u64,
    pub cookie: i64,
    pub file_type: u8,
    pub name: Vec<u8>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DirectoryBatchToken {
    pub generation: u64,
    pub cookie: i64,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryBatch {
    pub token: DirectoryBatchToken,
    pub entries: Vec<OfdDirectoryEntry>,
}
