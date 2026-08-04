/// Bounds applied while importing a container filesystem archive.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub entries: u64,
    pub bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            entries: 100_000,
            bytes: 128 * 1024 * 1024 * 1024,
        }
    }
}

/// Policy applied while importing a container filesystem archive.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Extraction {
    /// Preserve the archive's guest uid/gid metadata instead of assigning root ownership.
    pub copy_uid_gid: bool,
    /// Reject replacements that change a path between directory and non-directory kinds.
    pub no_overwrite_dir_non_dir: bool,
}
