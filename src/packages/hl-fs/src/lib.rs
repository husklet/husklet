//! Transferable filesystem entities.

use std::io;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

static ANONYMOUS_FILE_ID: AtomicU64 = AtomicU64::new(0);
static REPLACEMENT_FILE_ID: AtomicU64 = AtomicU64::new(0);

/// A staged sibling whose directory entry is owned until atomic publication.
struct ReplacementFile {
    path: PathBuf,
    file: Option<std::fs::File>,
}

impl ReplacementFile {
    fn create(path: PathBuf) -> io::Result<Self> {
        let file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path)?;
        Ok(Self { path, file: Some(file) })
    }

    fn publish(
        mut self,
        target: &Path,
        parent: &Path,
        bytes: &[u8],
        permissions: Option<&std::fs::Permissions>,
    ) -> io::Result<()> {
        let prepared = self.prepare(bytes, permissions);
        drop(self.file.take());
        prepared?;
        std::fs::rename(&self.path, target)?;
        Directory::from(parent).sync()
    }

    fn prepare(&mut self, bytes: &[u8], permissions: Option<&std::fs::Permissions>) -> io::Result<()> {
        let file = self.file.as_mut().expect("replacement handle exists until publication");
        file.write_all(bytes)?;
        if let Some(permissions) = permissions {
            file.set_permissions(permissions.clone())?;
        }
        file.sync_all()
    }
}

impl Drop for ReplacementFile {
    fn drop(&mut self) {
        drop(self.file.take());
        let _ = std::fs::remove_file(&self.path);
    }
}

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
        let name = self.0.file_name().and_then(|name| name.to_str()).unwrap_or("file");
        for _ in 0..128 {
            let id = REPLACEMENT_FILE_ID.fetch_add(1, Ordering::Relaxed);
            let temporary = parent.join(format!(".{name}.replace-{}-{id}", std::process::id()));
            let replacement = match ReplacementFile::create(temporary) {
                Ok(replacement) => replacement,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            };
            return replacement.publish(&self.0, parent, bytes.as_ref(), permissions.as_ref());
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

enum TreeKind {
    Directory(std::fs::Permissions),
    File,
    Symlink,
    Unsupported,
}

/// One inspected source entry paired with its exact destination path.
struct TreeEntry {
    source: PathBuf,
    destination: PathBuf,
    kind: TreeKind,
}

impl TreeEntry {
    fn inspect(entry: std::fs::DirEntry, destination: &Path) -> io::Result<Self> {
        let source = entry.path();
        let metadata = std::fs::symlink_metadata(&source)?;
        let file_type = metadata.file_type();
        let kind = match (file_type.is_dir(), file_type.is_file(), file_type.is_symlink()) {
            (true, _, _) => TreeKind::Directory(metadata.permissions()),
            (_, true, _) => TreeKind::File,
            (_, _, true) => TreeKind::Symlink,
            _ => TreeKind::Unsupported,
        };
        Ok(Self {
            source,
            destination: destination.join(entry.file_name()),
            kind,
        })
    }

    fn copy(self) -> io::Result<()> {
        match self.kind {
            TreeKind::Directory(permissions) => {
                std::fs::create_dir(&self.destination)?;
                Directory::from(&self.source).copy_to(&Directory::from(&self.destination))?;
                std::fs::set_permissions(self.destination, permissions)
            }
            TreeKind::File => std::fs::copy(self.source, self.destination).map(|_| ()),
            TreeKind::Symlink => Directory::copy_symlink(&self.source, &self.destination),
            TreeKind::Unsupported => Err(io::Error::new(
                io::ErrorKind::Unsupported,
                format!("cannot copy special node {}", self.source.display()),
            )),
        }
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
            TreeEntry::inspect(entry?, &destination.0)?.copy()?;
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
        assert!(
            std::fs::read_dir(temporary.path())
                .unwrap()
                .all(|entry| { !entry.unwrap().file_name().to_string_lossy().contains(".replace-") })
        );
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

        assert_eq!(std::fs::metadata(target).unwrap().permissions().mode() & 0o777, 0o600);
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
        assert_eq!(std::fs::read(destination.0.join("one")).unwrap(), b"123");
        assert_eq!(std::fs::read(destination.0.join("nested/two")).unwrap(), b"4567");

        destination.clear().await.unwrap();
        assert_eq!(std::fs::read_dir(&destination.0).unwrap().count(), 0);
        destination.remove().await.unwrap();
        destination.remove().await.unwrap();
        assert!(!destination.0.exists());
    }

    #[cfg(unix)]
    #[test]
    fn tree_copy_contracts() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        std::fs::create_dir_all(source.join("nested")).unwrap();
        std::fs::create_dir(&destination).unwrap();
        std::fs::write(source.join("nested/value"), b"value").unwrap();
        std::fs::set_permissions(source.join("nested"), std::fs::Permissions::from_mode(0o750)).unwrap();
        symlink("nested/value", source.join("link")).unwrap();

        Directory::from(source).copy_to(&Directory::from(&destination)).unwrap();

        assert_eq!(
            std::fs::read_link(destination.join("link")).unwrap(),
            std::path::Path::new("nested/value")
        );
        assert_eq!(
            std::fs::metadata(destination.join("nested"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o750
        );
    }

    #[cfg(unix)]
    #[test]
    fn special_nodes_rejected() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("source");
        let destination = temporary.path().join("destination");
        std::fs::create_dir(&source).unwrap();
        std::fs::create_dir(&destination).unwrap();
        let socket = std::os::unix::net::UnixListener::bind(source.join("socket")).unwrap();

        let error = Directory::from(source)
            .copy_to(&Directory::from(destination))
            .unwrap_err();
        assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);
        drop(socket);
    }
}
