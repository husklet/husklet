use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::PathBuf;
use std::sync::Arc;

use hl_descriptor::{DirectoryBatch, DirectoryBatchToken, ObjectError, OfdDirectoryEntry};

const ENTRY_LIMIT: usize = 65_536;

pub(super) struct State {
    path: PathBuf,
    terminals: Option<Arc<hl_runtime::TerminalCatalog>>,
    overlay: Option<Vec<PathBuf>>,
    entries: Option<Vec<OfdDirectoryEntry>>,
    cursor: usize,
    generation: u64,
}

impl State {
    pub(super) fn new(path: PathBuf) -> Self {
        Self {
            path,
            terminals: None,
            overlay: None,
            entries: None,
            cursor: 0,
            generation: 1,
        }
    }

    pub(super) fn with_overlay(mut self, paths: Vec<PathBuf>) -> Self {
        if paths.len() > 1 {
            self.overlay = Some(paths);
        }
        self
    }

    pub(super) fn with_terminals(mut self, terminals: Arc<hl_runtime::TerminalCatalog>) -> Self {
        self.terminals = Some(terminals);
        self
    }

    pub(super) fn read(&mut self, maximum: usize) -> Result<DirectoryBatch, ObjectError> {
        if self.entries.is_none() {
            self.entries = Some(self.load()?);
        }
        let entries = self.entries.as_ref().ok_or(ObjectError::Io)?;
        Ok(DirectoryBatch {
            token: DirectoryBatchToken {
                generation: self.generation,
                cookie: i64::try_from(self.cursor).map_err(|_| ObjectError::ResourceLimit)?,
            },
            entries: entries[self.cursor..].iter().take(maximum).cloned().collect(),
        })
    }

    pub(super) fn commit(&mut self, token: DirectoryBatchToken, count: usize) -> Result<(), ObjectError> {
        if token.generation != self.generation || usize::try_from(token.cookie).ok() != Some(self.cursor) {
            return Err(ObjectError::InvalidArgument);
        }
        let length = self.entries.as_ref().ok_or(ObjectError::InvalidArgument)?.len();
        self.cursor = self
            .cursor
            .checked_add(count)
            .filter(|cursor| *cursor <= length)
            .ok_or(ObjectError::InvalidArgument)?;
        Ok(())
    }

    pub(super) fn seek(&mut self, cookie: u64) -> Result<u64, ObjectError> {
        if self.entries.is_none() {
            self.entries = Some(self.load()?);
        }
        let position = usize::try_from(cookie).map_err(|_| ObjectError::InvalidArgument)?;
        if position > self.entries.as_ref().ok_or(ObjectError::Io)?.len() {
            return Err(ObjectError::InvalidArgument);
        }
        self.cursor = position;
        Ok(cookie)
    }

    fn load(&self) -> Result<Vec<OfdDirectoryEntry>, ObjectError> {
        if let Some(paths) = &self.overlay {
            return self.load_overlay(paths);
        }
        let mut entries = std::fs::read_dir(&self.path)
            .map_err(Self::object)?
            .take(ENTRY_LIMIT - 1)
            .map(Self::entry)
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(terminals) = &self.terminals {
            entries.retain(Self::retain_native);
            entries.push(Self::terminal(b"ptmx".to_vec(), 5, 2));
            entries.extend(Self::terminal_entries(terminals));
        }
        if entries.len() > ENTRY_LIMIT - 2 {
            return Err(ObjectError::ResourceLimit);
        }
        entries.sort_by(|left, right| left.name.cmp(&right.name));
        let current = std::fs::metadata(&self.path).map_err(Self::object)?;
        let parent = std::fs::metadata(self.path.join("..")).map_err(Self::object)?;
        entries.insert(0, Self::named(&parent, b".."));
        entries.insert(0, Self::named(&current, b"."));
        for (index, entry) in entries.iter_mut().enumerate() {
            entry.cookie = i64::try_from(index + 1).map_err(|_| ObjectError::ResourceLimit)?;
        }
        Ok(entries)
    }

    fn load_overlay(&self, paths: &[PathBuf]) -> Result<Vec<OfdDirectoryEntry>, ObjectError> {
        let layers = paths.iter().map(Self::overlay_layer).collect::<Result<Vec<_>, _>>()?;
        let merged = super::overlay_entries::merge(&layers, &[]).map_err(Self::object)?;
        merged
            .into_iter()
            .enumerate()
            .map(|(index, entry)| {
                let metadata = if entry.name == b"." {
                    std::fs::symlink_metadata(&self.path)
                } else if entry.name == b".." {
                    std::fs::symlink_metadata(self.path.join(".."))
                } else {
                    paths
                        .iter()
                        .map(|path| std::fs::symlink_metadata(path.join(std::ffi::OsStr::from_bytes(&entry.name))))
                        .find_map(Result::ok)
                        .ok_or_else(|| std::io::Error::from(std::io::ErrorKind::NotFound))
                }
                .map_err(Self::object)?;
                Ok(OfdDirectoryEntry {
                    inode: metadata.ino(),
                    cookie: i64::try_from(index + 1).map_err(|_| ObjectError::ResourceLimit)?,
                    file_type: entry.kind,
                    name: entry.name,
                })
            })
            .collect()
    }

    fn overlay_layer(path: &PathBuf) -> Result<super::overlay_entries::LayerDirectory, ObjectError> {
        let mut opaque = false;
        let mut entries = Vec::new();
        let directory = match std::fs::read_dir(path) {
            Ok(directory) => directory,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(super::overlay_entries::LayerDirectory::default());
            }
            Err(error) => return Err(Self::object(error)),
        };
        for value in directory.take(ENTRY_LIMIT) {
            let value = value.map_err(Self::object)?;
            let raw = value.file_name();
            let raw = raw.as_bytes();
            if raw == b".wh..wh..opq" {
                opaque = true;
                continue;
            }
            let (name, whiteout) = raw.strip_prefix(b".wh.").map_or((raw, false), |name| (name, true));
            let name = hl_runtime::GuestName::new(name).map_err(|_| ObjectError::Io)?;
            let metadata = std::fs::symlink_metadata(value.path()).map_err(Self::object)?;
            entries.push(super::overlay_entries::Candidate {
                name,
                kind: Self::kind(metadata.mode()),
                whiteout,
            });
        }
        Ok(super::overlay_entries::LayerDirectory { entries, opaque })
    }

    fn retain_native(entry: &OfdDirectoryEntry) -> bool {
        entry.name != b"ptmx" && entry.name.iter().any(|byte| !byte.is_ascii_digit())
    }

    fn terminal_entries(terminals: &hl_runtime::TerminalCatalog) -> Vec<OfdDirectoryEntry> {
        terminals
            .indices()
            .into_iter()
            .map(|index| Self::terminal(index.to_string().into_bytes(), 136, u32::from(index)))
            .collect()
    }

    fn named(metadata: &std::fs::Metadata, name: &[u8]) -> OfdDirectoryEntry {
        OfdDirectoryEntry {
            inode: metadata.ino(),
            cookie: 0,
            file_type: Self::kind(metadata.mode()),
            name: name.to_vec(),
        }
    }

    fn terminal(name: Vec<u8>, major: u32, minor: u32) -> OfdDirectoryEntry {
        OfdDirectoryEntry {
            inode: (u64::from(major) << 32) | u64::from(minor),
            cookie: 0,
            file_type: 2,
            name,
        }
    }

    fn entry(value: Result<std::fs::DirEntry, std::io::Error>) -> Result<OfdDirectoryEntry, ObjectError> {
        let entry = value.map_err(Self::object)?;
        let metadata = std::fs::symlink_metadata(entry.path()).map_err(Self::object)?;
        Ok(OfdDirectoryEntry {
            inode: metadata.ino(),
            cookie: 0,
            file_type: Self::kind(metadata.mode()),
            name: entry.file_name().as_bytes().to_vec(),
        })
    }

    fn kind(mode: u32) -> u8 {
        match mode & super::native::mode::TYPE_MASK {
            super::native::mode::FIFO => 1,
            super::native::mode::CHARACTER => 2,
            super::native::mode::DIRECTORY => 4,
            super::native::mode::BLOCK => 6,
            super::native::mode::REGULAR => 8,
            super::native::mode::SYMLINK => 10,
            super::native::mode::SOCKET => 12,
            _ => 0,
        }
    }

    fn object(error: std::io::Error) -> ObjectError {
        match error.kind() {
            std::io::ErrorKind::PermissionDenied => ObjectError::PermissionDenied,
            std::io::ErrorKind::WouldBlock => ObjectError::WouldBlock,
            std::io::ErrorKind::Interrupted => ObjectError::Interrupted,
            _ => ObjectError::Io,
        }
    }
}
