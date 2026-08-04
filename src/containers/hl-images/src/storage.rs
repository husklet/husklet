use std::{
    collections::BTreeMap,
    fs,
    fs::{File, OpenOptions},
    io::Write as _,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock, Weak},
};

use crate::Result;

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
            .open(path)?;
        fs2::FileExt::lock_exclusive(&file)?;
        Ok(Self { _file: file })
    }
}

impl Persistence for Native {
    fn replace(&self, path: &Path, bytes: &[u8]) -> Result<()> {
        let parent = path
            .parent()
            .ok_or_else(|| crate::Error::InvalidMetadata("metadata path has no parent".into()))?;
        let temporary = parent.join(format!(
            ".{}.tmp-{}",
            path.file_name().and_then(|value| value.to_str()).unwrap_or("metadata"),
            uuid::Uuid::new_v4()
        ));
        let result = (|| {
            let mut file = File::create(&temporary)?;
            file.write_all(bytes)?;
            file.sync_all()?;
            drop(file);
            fs::rename(&temporary, path)?;
            File::open(parent)?.sync_all()?;
            Ok(())
        })();
        if result.is_err() {
            let _ = fs::remove_file(temporary);
        }
        result
    }

    fn remove(&self, path: &Path) -> Result<bool> {
        match fs::remove_file(path) {
            Ok(()) => {
                let parent = path
                    .parent()
                    .ok_or_else(|| crate::Error::InvalidMetadata("blob path has no parent".into()))?;
                File::open(parent)?.sync_all()?;
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(error) => Err(error.into()),
        }
    }
}
