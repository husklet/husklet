#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemKind {
    Overlay,
    Proc,
    Sys,
    Cgroup2,
    Tmpfs,
    Devpts,
    Mqueue,
}

/// Host-neutral filesystem geometry and mount identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FilesystemStats {
    pub kind: FilesystemKind,
    pub block_size: u64,
    pub blocks: u64,
    pub blocks_free: u64,
    pub blocks_available: u64,
    pub files: u64,
    pub files_free: u64,
    pub filesystem_id: [u32; 2],
    pub name_maximum: u64,
    pub fragment_size: u64,
    pub read_only: bool,
    pub nosuid: bool,
    pub nodev: bool,
    pub noexec: bool,
    pub relatime: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FilesystemStatsError {
    InvalidGeometry,
}

impl FilesystemStats {
    pub fn validate(self) -> Result<Self, FilesystemStatsError> {
        if self.block_size == 0
            || self.fragment_size == 0
            || self.blocks_free > self.blocks
            || self.blocks_available > self.blocks
            || self.files_free > self.files
        {
            return Err(FilesystemStatsError::InvalidGeometry);
        }
        Ok(self)
    }
}
