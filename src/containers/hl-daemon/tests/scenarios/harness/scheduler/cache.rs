use super::Error;
use crate::contract::Target;
use fs2::FileExt as _;
use std::{
    fs::{self, File, OpenOptions},
    path::{Path, PathBuf},
};

pub(super) struct RunLock {
    _file: File,
}

impl RunLock {
    pub(super) fn acquire(cache: &Path) -> Result<Self, Error> {
        let path = cache
            .parent()
            .unwrap_or_else(|| Path::new("target/scenarios"))
            .join("matrix.lock");
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.lock_exclusive()?;
        Ok(Self { _file: file })
    }
}

pub(super) fn absolute(target: Target) -> Result<PathBuf, Error> {
    Ok(crate::fixture::cache_root(&target.platform())?)
}

pub(super) fn test_lock() -> Result<(), Error> {
    let temporary = tempfile::tempdir()?;
    let cache = temporary.path().join("images");
    fs::create_dir_all(&cache)?;
    let held = RunLock::acquire(&cache)?;
    let contender = OpenOptions::new()
        .read(true)
        .write(true)
        .open(temporary.path().join("matrix.lock"))?;
    if contender.try_lock_exclusive().is_ok() {
        return Err("independent matrix orchestrators acquired the same cache lock".into());
    }
    drop(held);
    contender.try_lock_exclusive()?;
    Ok(())
}
