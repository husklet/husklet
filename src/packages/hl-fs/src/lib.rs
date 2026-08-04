//! Transferable filesystem entities.

use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static ANONYMOUS_FILE_ID: AtomicU64 = AtomicU64::new(0);
static REPLACEMENT_FILE_ID: AtomicU64 = AtomicU64::new(0);

/// A durable filesystem value replaced atomically as one complete byte sequence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct File(PathBuf);

impl File {
    /// Writes, flushes, and atomically renames a sibling temporary file over this path.
    ///
    /// Readers observe either the previous complete value or the new complete value. The containing
    /// directory is synchronized so a successful return includes durable rename metadata.
    ///
    /// # Errors
    ///
    /// Returns directory creation, temporary-file allocation, write, synchronization, rename, or
    /// directory synchronization failures.
    pub fn replace(&self, bytes: impl AsRef<[u8]>) -> io::Result<()> {
        let permissions = match std::fs::metadata(&self.0) {
            Ok(metadata) => Some(metadata.permissions()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error),
        };
        let parent = self.0.parent().unwrap_or_else(|| Path::new("."));
        std::fs::create_dir_all(parent)?;
        let name = self
            .0
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("file");
        for _ in 0..128 {
            let id = REPLACEMENT_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let temporary = parent.join(format!(".{name}.replace-{}-{id}", std::process::id()));
            let mut file = match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temporary)
            {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
            let result = (|| {
                file.write_all(bytes.as_ref())?;
                if let Some(permissions) = permissions.clone() {
                    file.set_permissions(permissions)?;
                }
                file.sync_all()?;
                std::fs::rename(&temporary, &self.0)?;
                Directory::from(parent).sync()
            })();
            if result.is_err() {
                let _ = std::fs::remove_file(&temporary);
            }
            return result;
        }
        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique replacement file",
        ))
    }
}

impl<P: Into<PathBuf>> From<P> for File {
    fn from(path: P) -> Self {
        Self(path.into())
    }
}

/// An unlinked temporary file suitable for descriptor-backed shared memory.
pub struct AnonymousFile(std::fs::File);

impl AnonymousFile {
    /// Creates an exclusively owned file, sizes it, then removes its directory entry.
    ///
    /// The open descriptor retains the file until its final owner closes it. This is portable across the
    /// Linux and macOS hosts supported by Husklet and avoids platform-specific `memfd_create` assumptions.
    ///
    /// # Errors
    /// Returns directory, exclusive-create, sizing, or unlink failures.
    pub fn new(directory: &Path, name: &str, length: u64) -> io::Result<Self> {
        let name: String = name
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || *character == '-')
            .collect();
        let name = if name.is_empty() { "anonymous" } else { &name };

        for _ in 0..128 {
            let id = ANONYMOUS_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let path = directory.join(format!("hl-{name}-{}-{id}", std::process::id()));
            let file = match std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => file,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
            if let Err(error) = file.set_len(length) {
                let _ = std::fs::remove_file(path);
                return Err(error);
            }
            std::fs::remove_file(path)?;
            return Ok(Self(file));
        }

        Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "could not allocate a unique anonymous file",
        ))
    }

    /// Returns the owned file for descriptor transfer or mapping.
    #[must_use]
    pub fn into_file(self) -> std::fs::File {
        self.0
    }
}

/// A directory and operations whose meaning is independent of a product domain.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Directory(PathBuf);

impl Directory {
    /// Removes every child while preserving this directory.
    ///
    /// # Errors
    /// Returns filesystem traversal or removal failures.
    #[cfg(feature = "async")]
    pub async fn clear(&self) -> io::Result<()> {
        let mut entries = tokio::fs::read_dir(&self.0).await?;
        while let Some(entry) = entries.next_entry().await? {
            let path = entry.path();
            if entry.file_type().await?.is_dir() {
                tokio::fs::remove_dir_all(path).await?;
            } else {
                tokio::fs::remove_file(path).await?;
            }
        }
        Ok(())
    }

    /// Removes this directory tree. A missing directory is already removed.
    ///
    /// # Errors
    /// Returns removal failures other than an absent directory.
    #[cfg(feature = "async")]
    pub async fn remove(&self) -> io::Result<()> {
        match tokio::fs::remove_dir_all(&self.0).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Returns the total size of regular files below this directory.
    ///
    /// # Errors
    /// Returns metadata or directory traversal failures.
    pub fn size(&self) -> io::Result<u64> {
        let mut total = 0_u64;
        for entry in std::fs::read_dir(&self.0)? {
            let entry = entry?;
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if metadata.is_dir() {
                total = total.saturating_add(Self::from(entry.path()).size()?);
            } else if metadata.is_file() {
                total = total.saturating_add(metadata.len());
            }
        }
        Ok(total)
    }

    /// Copies this directory's children into an existing destination directory.
    ///
    /// # Errors
    /// Returns invalid source, traversal, copy, permission, symlink, or unsupported-node failures.
    pub fn copy_to(&self, destination: &Directory) -> io::Result<()> {
        let metadata = std::fs::symlink_metadata(&self.0)?;
        if !metadata.is_dir() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} is not a directory", self.0.display()),
            ));
        }
        for entry in std::fs::read_dir(&self.0)? {
            let entry = entry?;
            let target = destination.0.join(entry.file_name());
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if metadata.is_dir() {
                std::fs::create_dir(&target)?;
                Self::from(entry.path()).copy_to(&Self::from(target.clone()))?;
                std::fs::set_permissions(target, metadata.permissions())?;
            } else if metadata.is_file() {
                std::fs::copy(entry.path(), target)?;
            } else if metadata.file_type().is_symlink() {
                Self::copy_symlink(&entry.path(), &target)?;
            } else {
                return Err(io::Error::new(
                    io::ErrorKind::Unsupported,
                    format!("cannot copy special node {}", entry.path().display()),
                ));
            }
        }
        Ok(())
    }

    /// Flushes directory metadata to stable storage.
    ///
    /// # Errors
    /// Returns directory-open or synchronization failures.
    pub fn sync(&self) -> io::Result<()> {
        std::fs::File::open(&self.0)?.sync_all()
    }

    #[cfg(unix)]
    fn copy_symlink(source: &Path, destination: &Path) -> io::Result<()> {
        std::os::unix::fs::symlink(std::fs::read_link(source)?, destination)
    }

    #[cfg(not(unix))]
    fn copy_symlink(source: &Path, _destination: &Path) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("cannot copy symlink {} on this host", source.display()),
        ))
    }
}

impl<P: Into<PathBuf>> From<P> for Directory {
    fn from(path: P) -> Self {
        Self(path.into())
    }
}

#[cfg(test)]
mod tests {
    use super::{AnonymousFile, Directory, File};
    use std::io::{Read, Seek, Write};

    #[test]
    fn anonymous_file_is_sized_writable_and_unlinked() {
        let mut file = AnonymousFile::new(&std::env::temp_dir(), "wayland shm", 16)
            .unwrap()
            .into_file();
        assert_eq!(file.metadata().unwrap().len(), 16);
        file.write_all(b"pixels").unwrap();
        file.rewind().unwrap();
        let mut bytes = [0; 6];
        file.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"pixels");
    }

    #[test]
    fn replacement_never_exposes_a_partial_value() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("configuration");
        let file = File::from(path.clone());
        let first = vec![b'a'; 64 * 1024];
        let second = vec![b'b'; 64 * 1024];
        file.replace(&first).unwrap();
        let running = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(true));
        let reader_running = std::sync::Arc::clone(&running);
        let reader = std::thread::spawn(move || {
            while reader_running.load(std::sync::atomic::Ordering::Acquire) {
                let value = std::fs::read(&path).unwrap();
                assert!(value == first || value == second);
            }
        });

        for index in 0..32 {
            file.replace(if index % 2 == 0 {
                vec![b'b'; 64 * 1024]
            } else {
                vec![b'a'; 64 * 1024]
            })
            .unwrap();
        }
        running.store(false, std::sync::atomic::Ordering::Release);
        reader.join().unwrap();
    }

    #[test]
    fn failed_replacement_removes_its_temporary_file() {
        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("target");
        std::fs::create_dir(&target).unwrap();

        assert!(File::from(target).replace(b"value").is_err());
        assert_eq!(std::fs::read_dir(temporary.path()).unwrap().count(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn replacement_preserves_existing_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().unwrap();
        let target = temporary.path().join("secret");
        std::fs::write(&target, b"old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();

        File::from(&target).replace(b"new").unwrap();

        assert_eq!(
            std::fs::metadata(target).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[tokio::test]
    async fn copies_sizes_clears_and_removes_directory_trees() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(source.join("one"), b"123").unwrap();
        std::fs::write(source.join("nested/two"), b"4567").unwrap();

        let source = Directory::from(source);
        let destination = Directory::from(destination.clone());
        source.copy_to(&destination).unwrap();
        assert_eq!(destination.size().unwrap(), 7);

        destination.clear().await.unwrap();
        assert_eq!(std::fs::read_dir(&destination.0).unwrap().count(), 0);
        destination.remove().await.unwrap();
        destination.remove().await.unwrap();
        assert!(!destination.0.exists());
    }
}
