use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::{FsError, Result};

/// A host file read under an explicit allocation bound.
pub struct BoundedFile;

impl BoundedFile {
    /// Reads the complete file only when its size is at most `maximum`.
    pub fn read(path: impl AsRef<Path>, maximum: usize) -> Result<Vec<u8>> {
        let path = path.as_ref();
        let mut file = File::open(path).map_err(|error| FsError::io("open file", path, error))?;
        let metadata = file
            .metadata()
            .map_err(|error| FsError::io("read file metadata", path, error))?;
        if metadata.len() > maximum as u64 {
            return Err(FsError::LimitExceeded {
                path: path.to_owned(),
                limit: maximum,
            });
        }

        let initial = usize::try_from(metadata.len()).unwrap_or(maximum).min(maximum);
        let mut contents = Vec::with_capacity(initial);
        let mut buffer = [0_u8; 8192];
        loop {
            let read = file
                .read(&mut buffer)
                .map_err(|error| FsError::io("read file", path, error))?;
            if read == 0 {
                return Ok(contents);
            }
            if read > maximum.saturating_sub(contents.len()) {
                return Err(FsError::LimitExceeded {
                    path: path.to_owned(),
                    limit: maximum,
                });
            }
            contents.extend_from_slice(&buffer[..read]);
        }
    }
}

/// Persistence requested for an atomic file replacement.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Durability {
    /// Rename visibility only; no explicit synchronization.
    None,
    /// Synchronize replacement contents before publishing the rename.
    File,
    /// Synchronize contents and then the parent directory entry.
    FileAndDirectory,
}

/// Atomic same-directory file publication.
pub struct AtomicFile;

impl AtomicFile {
    /// Writes a unique sibling and publishes it with one host rename.
    ///
    /// The destination is never deleted as a fallback. If a host cannot
    /// atomically replace an existing destination, its rename error is returned.
    pub fn replace(path: impl AsRef<Path>, contents: &[u8], durability: Durability) -> Result<()> {
        let path = path.as_ref();
        let parent = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or(Path::new("."));
        let (temporary_path, mut temporary) = Self::temporary_sibling(path, parent)?;
        let mut cleanup = TemporaryCleanup(Some(temporary_path.clone()));
        temporary
            .write_all(contents)
            .map_err(|error| FsError::io("write temporary sibling", &temporary_path, error))?;
        if durability != Durability::None {
            temporary
                .sync_all()
                .map_err(|error| FsError::io("synchronize temporary sibling", &temporary_path, error))?;
        }
        drop(temporary);
        fs::rename(&temporary_path, path).map_err(|error| FsError::io("publish atomic replacement", path, error))?;
        cleanup.0 = None;
        if durability == Durability::FileAndDirectory {
            Self::synchronize_directory(parent)?;
        }
        Ok(())
    }

    fn temporary_sibling(path: &Path, parent: &Path) -> Result<(PathBuf, File)> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        for _ in 0..128 {
            let sequence = NEXT.fetch_add(1, Ordering::Relaxed);
            let candidate = parent.join(format!(".hl-replace-{}-{sequence}.tmp", std::process::id()));
            match OpenOptions::new().write(true).create_new(true).open(&candidate) {
                Ok(file) => return Ok((candidate, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(FsError::io("create temporary sibling", &candidate, error));
                }
            }
        }
        Err(FsError::TemporarySiblingExhausted(path.to_owned()))
    }

    #[cfg(unix)]
    fn synchronize_directory(path: &Path) -> Result<()> {
        let directory = File::open(path).map_err(|error| FsError::io("open parent directory", path, error))?;
        directory
            .sync_all()
            .map_err(|error| FsError::io("synchronize parent directory", path, error))
    }

    #[cfg(windows)]
    fn synchronize_directory(path: &Path) -> Result<()> {
        Err(FsError::DirectoryDurabilityUnsupported(path.to_owned()))
    }
}

struct TemporaryCleanup(Option<PathBuf>);

impl Drop for TemporaryCleanup {
    fn drop(&mut self) {
        if let Some(path) = self.0.take() {
            let _ = fs::remove_file(path);
        }
    }
}

/// Stable host filesystem identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum FileIdentity {
    /// POSIX device and inode pair.
    Unix {
        /// Filesystem device.
        device: u64,
        /// Object inode.
        inode: u64,
    },
    /// Windows volume and file index pair.
    Windows {
        /// Volume serial number.
        volume: u32,
        /// File index within the volume.
        index: u64,
    },
}

impl FileIdentity {
    /// Reads identity without following an API-specific guest policy.
    #[cfg(unix)]
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        use std::os::unix::fs::MetadataExt;

        let path = path.as_ref();
        let metadata = fs::metadata(path).map_err(|error| FsError::io("read file identity", path, error))?;
        Ok(Self::Unix {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }

    /// Reads identity without following an API-specific guest policy.
    #[cfg(windows)]
    pub fn read(path: impl AsRef<Path>) -> Result<Self> {
        use std::os::windows::fs::MetadataExt;

        let path = path.as_ref();
        let metadata = fs::metadata(path).map_err(|error| FsError::io("read file identity", path, error))?;
        let volume = metadata
            .volume_serial_number()
            .ok_or_else(|| FsError::IdentityUnavailable(path.to_owned()))?;
        let index = metadata
            .file_index()
            .ok_or_else(|| FsError::IdentityUnavailable(path.to_owned()))?;
        Ok(Self::Windows { volume, index })
    }
}
