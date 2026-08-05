use std::sync::Mutex;

use hl_vfs::GuestPath;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DirectorySnapshot {
    pub path: GuestPath,
    pub deleted: bool,
}

/// Process-owned canonical guest working directory.
pub struct WorkingDirectory {
    state: Mutex<DirectorySnapshot>,
}

impl WorkingDirectory {
    #[must_use]
    pub fn root() -> Self {
        Self {
            state: Mutex::new(DirectorySnapshot {
                path: GuestPath::new("/").expect("root is a valid guest path"),
                deleted: false,
            }),
        }
    }

    #[must_use]
    pub fn from_snapshot(snapshot: DirectorySnapshot) -> Self {
        Self {
            state: Mutex::new(snapshot),
        }
    }

    pub fn snapshot(&self) -> DirectorySnapshot {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    pub fn replace(&self, path: GuestPath) {
        *self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner) =
            DirectorySnapshot { path, deleted: false };
    }

    pub fn replace_path(&self, path: &str) -> Result<(), ()> {
        self.replace(GuestPath::new(path).map_err(|_| ())?);
        Ok(())
    }

    pub fn mark_deleted(&self) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .deleted = true;
    }
}

impl Default for WorkingDirectory {
    fn default() -> Self {
        Self::root()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_is_independent() {
        let parent = WorkingDirectory::root();
        let child = WorkingDirectory::from_snapshot(parent.snapshot());
        child.replace(GuestPath::new("/child").unwrap());
        child.mark_deleted();
        assert_eq!(parent.snapshot().path.as_str(), "/");
        assert!(!parent.snapshot().deleted);
        assert_eq!(child.snapshot().path.as_str(), "/child");
        assert!(child.snapshot().deleted);
    }
}
