use crate::{Error, Result};
use std::{
    io::{Seek as _, SeekFrom, Write as _},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU32, Ordering},
};

static NEXT: AtomicU32 = AtomicU32::new(2);

/// Shared filesystem epoch observed by long-running engine processes.
#[derive(Clone, Debug)]
pub(crate) struct Generation(PathBuf);

impl Generation {
    pub(crate) fn open(path: impl Into<PathBuf>) -> Result<Self> {
        let path = path.into();
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&path)?;
        match file.metadata()?.len() {
            0 => file.write_all(&1_u32.to_ne_bytes())?,
            4 => {}
            _ => {
                return Err(Error::Corrupt(format!(
                    "filesystem generation file {} is not four bytes",
                    path.display()
                )));
            }
        }
        Ok(Self(path))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.0
    }

    pub(crate) fn bump(&self) -> Result<()> {
        let value = NEXT.fetch_add(1, Ordering::Relaxed).max(2);
        let mut file = std::fs::OpenOptions::new().write(true).open(&self.0)?;
        file.seek(SeekFrom::Start(0))?;
        file.write_all(&value.to_ne_bytes())?;
        file.sync_data()?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Generation;

    #[test]
    fn epoch_is_fixed_width_and_changes_without_replacing_the_file() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("generation");
        let generation = Generation::open(&path).unwrap();
        let before = std::fs::read(&path).unwrap();
        generation.bump().unwrap();
        let after = std::fs::read(&path).unwrap();
        assert_eq!(before.len(), 4);
        assert_eq!(after.len(), 4);
        assert_ne!(before, after);
        assert_eq!(generation.path(), path);
    }

    #[test]
    fn malformed_epoch_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("generation");
        std::fs::write(&path, [0_u8; 3]).unwrap();
        assert!(Generation::open(path).unwrap_err().to_string().contains("four bytes"));
    }
}
