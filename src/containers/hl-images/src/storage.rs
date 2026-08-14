use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{Read as _, Write as _},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use crate::{Error, Result, error::At as _};

/// A directory authority captured before publication begins.
///
/// Unix operations stay relative to the captured descriptor, so replacing the
/// pathname with a symlink cannot redirect an in-flight metadata operation.
#[derive(Debug)]
pub(crate) struct Directory {
    path: PathBuf,
    #[cfg(unix)]
    descriptor: std::os::fd::OwnedFd,
}

impl Directory {
    pub(crate) fn open(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref().to_owned();
        #[cfg(unix)]
        let descriptor = {
            use nix::fcntl::{OFlag, open};
            use nix::sys::stat::Mode;

            open(
                &path,
                OFlag::O_RDONLY | OFlag::O_DIRECTORY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
                Mode::empty(),
            )
            .map_err(std::io::Error::from)
            .at(&path)?
        };
        #[cfg(not(unix))]
        if !std::fs::metadata(&path).at(&path)?.is_dir() {
            return Err(Error::InvalidMetadata(format!(
                "metadata authority is not a directory: {}",
                path.display()
            )));
        }
        Ok(Self {
            path,
            #[cfg(unix)]
            descriptor,
        })
    }

    fn name<'a>(&self, name: &'a Path) -> Result<&'a std::ffi::OsStr> {
        let mut components = name.components();
        let Some(Component::Normal(name)) = components.next() else {
            return Err(Error::InvalidMetadata("metadata filename is invalid".into()));
        };
        if components.next().is_some() {
            return Err(Error::InvalidMetadata("metadata filename contains a directory".into()));
        }
        Ok(name)
    }

    pub(crate) fn replace(&self, name: &Path, bytes: &[u8]) -> Result<()> {
        let name = self.name(name)?;
        let temporary_name = format!(".{}.tmp-{}", name.to_str().unwrap_or("metadata"), uuid::Uuid::new_v4());
        let temporary = self.path.join(&temporary_name);
        let target = self.path.join(name);
        #[cfg(unix)]
        {
            use nix::fcntl::{OFlag, openat, renameat};
            use nix::sys::stat::Mode;
            use std::os::fd::AsFd as _;

            let descriptor = openat(
                &self.descriptor,
                temporary_name.as_str(),
                OFlag::O_WRONLY | OFlag::O_CREAT | OFlag::O_EXCL | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
                Mode::from_bits_truncate(0o600),
            )
            .map_err(std::io::Error::from)
            .at(&temporary)?;
            let mut file = File::from(descriptor);
            let result = (|| {
                file.write_all(bytes).at(&temporary)?;
                file.sync_all().at(&temporary)?;
                drop(file);
                renameat(&self.descriptor, temporary_name.as_str(), &self.descriptor, name)
                    .map_err(std::io::Error::from)
                    .at(&target)?;
                nix::unistd::fsync(self.descriptor.as_fd())
                    .map_err(std::io::Error::from)
                    .at(&self.path)?;
                Ok(())
            })();
            if result.is_err() {
                let _ = nix::unistd::unlinkat(
                    &self.descriptor,
                    temporary_name.as_str(),
                    nix::unistd::UnlinkatFlags::NoRemoveDir,
                );
            }
            result
        }
        #[cfg(not(unix))]
        {
            let result = (|| {
                let mut file = OpenOptions::new()
                    .create_new(true)
                    .write(true)
                    .open(&temporary)
                    .at(&temporary)?;
                file.write_all(bytes).at(&temporary)?;
                file.sync_all().at(&temporary)?;
                drop(file);
                std::fs::rename(&temporary, &target).at(&target)?;
                File::open(&self.path).at(&self.path)?.sync_all().at(&self.path)?;
                Ok(())
            })();
            if result.is_err() {
                let _ = std::fs::remove_file(temporary);
            }
            result
        }
    }

    pub(crate) fn read(&self, name: &Path, limit: u64) -> Result<Vec<u8>> {
        let name = self.name(name)?;
        let target = self.path.join(name);
        #[cfg(unix)]
        let file = {
            use nix::fcntl::{OFlag, openat};
            use nix::sys::stat::Mode;

            File::from(
                openat(
                    &self.descriptor,
                    name,
                    OFlag::O_RDONLY | OFlag::O_CLOEXEC | OFlag::O_NOFOLLOW,
                    Mode::empty(),
                )
                .map_err(std::io::Error::from)
                .at(&target)?,
            )
        };
        #[cfg(not(unix))]
        let file = File::open(&target).at(&target)?;
        let mut bytes = Vec::new();
        file.take(limit.saturating_add(1)).read_to_end(&mut bytes).at(&target)?;
        if bytes.len() as u64 > limit {
            return Err(Error::InvalidMetadata(format!("metadata file exceeds {limit} bytes")));
        }
        Ok(bytes)
    }

    pub(crate) fn remove(&self, name: &Path) -> Result<bool> {
        let name = self.name(name)?;
        let target = self.path.join(name);
        #[cfg(unix)]
        {
            use nix::errno::Errno;
            use std::os::fd::AsFd as _;

            match nix::unistd::unlinkat(&self.descriptor, name, nix::unistd::UnlinkatFlags::NoRemoveDir) {
                Ok(()) => {
                    nix::unistd::fsync(self.descriptor.as_fd())
                        .map_err(std::io::Error::from)
                        .at(&self.path)?;
                    Ok(true)
                }
                Err(Errno::ENOENT) => Ok(false),
                Err(error) => Err(std::io::Error::from(error)).at(target),
            }
        }
        #[cfg(not(unix))]
        {
            match std::fs::remove_file(&target) {
                Ok(()) => {
                    File::open(&self.path).at(&self.path)?.sync_all().at(&self.path)?;
                    Ok(true)
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
                Err(error) => Err(error).at(target),
            }
        }
    }
}

/// Durable filesystem operations used at image metadata and content commit boundaries.
///
/// Implementations may provide fault injection, auditing, or a different durable filesystem
/// adapter without changing image graph semantics. The caller grants the implementation access
/// to paths below the image-store root; image credentials and guest data are never passed here.
pub trait Persistence: Send + Sync + 'static {
    /// Atomically replace `path` with `bytes` and make the containing directory durable.
    ///
    /// # Errors
    /// A failure before rename leaves `path` unchanged. A host durability failure after rename may
    /// leave the complete replacement visible; callers therefore reload before their next update.
    fn replace(&self, path: &Path, bytes: &[u8]) -> Result<()>;

    /// Remove one committed blob and make the containing directory durable.
    ///
    /// # Errors
    /// Returns an error when removal or directory synchronization fails.
    fn remove(&self, path: &Path) -> Result<bool>;
}

/// Native durable filesystem implementation.
#[derive(Clone, Copy, Debug, Default)]
pub struct Native;

/// Process-local serialization shared by every metadata store opened on one path.
pub(crate) struct Writers;

impl Writers {
    pub(crate) fn for_path(path: &Path) -> Result<Arc<Mutex<()>>> {
        static LOCKS: OnceLock<Mutex<BTreeMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();
        let mut locks = LOCKS
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
            .map_err(|_| crate::Error::InvalidMetadata("writer registry poisoned".into()))?;
        if let Some(lock) = locks.get(path).and_then(Weak::upgrade) {
            return Ok(lock);
        }
        let lock = Arc::new(Mutex::new(()));
        locks.insert(path.to_owned(), Arc::downgrade(&lock));
        Ok(lock)
    }
}

pub(crate) struct ExclusiveLock {
    _file: File,
}

impl ExclusiveLock {
    pub(crate) fn acquire(path: &Path) -> Result<Self> {
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)
            .at(path)?;
        fs2::FileExt::lock_exclusive(&file).at(path)?;
        Ok(Self { _file: file })
    }
}

impl Persistence for Native {
    fn replace(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| crate::Error::InvalidMetadata("metadata path has no parent".into()))?;
        let name = path
            .file_name()
            .ok_or_else(|| crate::Error::InvalidMetadata("metadata path has no filename".into()))?;
        Directory::open(parent)?.replace(Path::new(name), bytes)
    }

    fn remove(&self, path: &Path) -> Result<bool> {
        let parent = path
            .parent()
            .ok_or_else(|| crate::Error::InvalidMetadata("blob path has no parent".into()))?;
        let name = path
            .file_name()
            .ok_or_else(|| crate::Error::InvalidMetadata("blob path has no filename".into()))?;
        Directory::open(parent)?.remove(Path::new(name))
    }
}
