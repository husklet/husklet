use std::{
    ffi::OsString,
    fs,
    path::{Path, PathBuf},
};

use crate::{FsError, Result};

/// Host directory whose entries can be enumerated deterministically.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Directory {
    path: PathBuf,
}

impl Directory {
    /// Refers to a host directory without applying guest path policy.
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    /// Directory path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Reads entries without following them and sorts by native filename bytes.
    pub fn entries(&self) -> Result<Vec<DirectoryEntry>> {
        let iterator = fs::read_dir(&self.path).map_err(|error| FsError::io("read directory", &self.path, error))?;
        let mut entries = Vec::new();
        for candidate in iterator {
            let candidate = candidate.map_err(|error| FsError::io("read directory entry", &self.path, error))?;
            let path = candidate.path();
            let kind = candidate
                .file_type()
                .map(EntryKind::from)
                .map_err(|error| FsError::io("read directory entry type", &path, error))?;
            entries.push(DirectoryEntry {
                name: candidate.file_name(),
                kind,
            });
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(entries)
    }
}

/// One deterministic directory entry.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectoryEntry {
    name: OsString,
    kind: EntryKind,
}

impl DirectoryEntry {
    /// Native filename, excluding the parent directory.
    pub fn name(&self) -> &OsString {
        &self.name
    }

    /// Non-following entry kind.
    pub fn kind(&self) -> EntryKind {
        self.kind
    }
}

/// Portable classification reported by `std::fs::FileType`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EntryKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Symbolic link.
    Symlink,
    /// Host object outside the portable standard-library categories.
    Other,
}

impl From<fs::FileType> for EntryKind {
    fn from(value: fs::FileType) -> Self {
        if value.is_file() {
            Self::File
        } else if value.is_dir() {
            Self::Directory
        } else if value.is_symlink() {
            Self::Symlink
        } else {
            Self::Other
        }
    }
}
