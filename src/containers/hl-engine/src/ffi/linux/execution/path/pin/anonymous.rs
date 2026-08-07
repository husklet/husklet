//! Hidden-name support for emulated anonymous (`O_TMPFILE`) inodes.

use std::ffi::CString;
use std::fs::File;
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use hl_runtime::RuntimePathError;

/// Hidden name holding an emulated `O_TMPFILE` inode alive; dropping it unlinks the name.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub(in crate::ffi::linux::execution::path) struct AnonymousName {
    directory: File,
    name: CString,
    /// Cleared once the hidden name has been renamed onto a guest-visible one.
    armed: bool,
}

#[cfg(target_os = "linux")]
impl AnonymousName {
    pub(super) fn create(
        parent: RawFd,
        directory_name: &CString,
        flags: i32,
        mode: u32,
    ) -> Result<(File, Option<Self>), RuntimePathError> {
        // SAFETY: `parent` stays open across the call and `directory_name` is NUL-terminated.
        let directory = unsafe {
            libc::openat(
                parent,
                directory_name.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                0,
            )
        };
        if directory < 0 {
            return Err(super::super::HostError::map(std::io::Error::last_os_error()));
        }
        // SAFETY: successful openat returned one descriptor not owned elsewhere.
        let directory = unsafe { File::from_raw_fd(directory) };
        let flags = flags & !(libc::O_TMPFILE | libc::O_DIRECTORY) | libc::O_CREAT | libc::O_EXCL | libc::O_CLOEXEC;
        for _ in 0..64 {
            let name = Self::candidate();
            // SAFETY: `directory` stays open across the call and `name` is NUL-terminated.
            let descriptor = unsafe { libc::openat(directory.as_raw_fd(), name.as_ptr(), flags, mode as libc::mode_t) };
            if descriptor >= 0 {
                // SAFETY: successful openat returned one descriptor not owned elsewhere.
                let file = unsafe { File::from_raw_fd(descriptor) };
                return Ok((
                    file,
                    Some(Self {
                        directory,
                        name,
                        armed: true,
                    }),
                ));
            }
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::EEXIST) {
                return Err(super::super::HostError::map(error));
            }
        }
        Err(RuntimePathError::Exists)
    }

    pub(super) fn candidate() -> CString {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let value = NEXT.fetch_add(1, Ordering::Relaxed);
        CString::new(format!(".hl-tmpfile-{}-{value}", std::process::id())).unwrap_or_else(|_| c".hl-tmpfile".into())
    }
}

#[cfg(target_os = "linux")]
impl Drop for AnonymousName {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        // SAFETY: both the directory descriptor and the name outlive this unlink.
        unsafe { libc::unlinkat(self.directory.as_raw_fd(), self.name.as_ptr(), 0) };
    }
}

/// Shares one emulated anonymous inode between its description and its link capability.
#[cfg(target_os = "linux")]
#[derive(Clone, Debug, Default)]
pub(in crate::ffi::linux::execution::path) struct AnonymousSlot(Arc<Mutex<Option<AnonymousName>>>);

#[cfg(target_os = "linux")]
impl AnonymousSlot {
    pub(in crate::ffi::linux::execution::path) fn set(&self, name: Option<AnonymousName>) {
        *self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner) = name;
    }

    pub(in crate::ffi::linux::execution::path) fn present(&self) -> bool {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .is_some()
    }

    /// Materializes an emulated anonymous inode by renaming its hidden name onto `name`,
    /// which keeps the published link count at one. Returns `false` when the inode is a
    /// real `O_TMPFILE` and the caller should link it instead.
    pub(in crate::ffi::linux::execution::path) fn materialize(
        &self,
        parent: RawFd,
        name: &CString,
    ) -> Option<Result<(), RuntimePathError>> {
        let mut slot = self.0.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        let anonymous = slot.as_ref()?;
        // SAFETY: both directory descriptors and both names outlive this rename.
        let result = unsafe {
            libc::renameat2(
                anonymous.directory.as_raw_fd(),
                anonymous.name.as_ptr(),
                parent,
                name.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        if result == 0 {
            let mut name = slot.take()?;
            name.armed = false;
            return Some(Ok(()));
        }
        Some(Err(super::super::HostError::map(std::io::Error::last_os_error())))
    }
}

#[cfg(all(test, target_os = "linux"))]
mod anonymous_tests {
    use super::{AnonymousName, AnonymousSlot};
    use hl_runtime::RuntimePathError;
    use std::ffi::CString;
    use std::fs::File;
    use std::io::Write;
    use std::os::fd::AsRawFd;
    use std::os::unix::fs::MetadataExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicU64, Ordering};

    static NEXT: AtomicU64 = AtomicU64::new(0);

    pub(super) fn scratch() -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "hl-anon-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        std::fs::create_dir(root.join("scratch")).unwrap();
        root
    }

    pub(super) fn create(root: &Path) -> (File, AnonymousName) {
        let parent = File::open(root).unwrap();
        let (file, name) = AnonymousName::create(
            parent.as_raw_fd(),
            &CString::new("scratch").unwrap(),
            libc::O_RDWR,
            0o600,
        )
        .unwrap();
        (file, name.unwrap())
    }

    #[test]
    pub(super) fn hidden_name_is_removed_when_the_inode_is_never_materialized() {
        let root = scratch();
        let (mut file, name) = create(&root);
        file.write_all(b"body").unwrap();
        assert_eq!(std::fs::read_dir(root.join("scratch")).unwrap().count(), 1);
        drop(name);
        assert_eq!(std::fs::read_dir(root.join("scratch")).unwrap().count(), 0);
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    pub(super) fn materializing_leaves_exactly_one_link() {
        let root = scratch();
        let (mut file, name) = create(&root);
        file.write_all(b"tmpfile-body").unwrap();
        let slot = AnonymousSlot::default();
        slot.set(Some(name));
        assert!(slot.present());
        let directory = File::open(root.join("scratch")).unwrap();
        slot.materialize(directory.as_raw_fd(), &CString::new("materialized").unwrap())
            .unwrap()
            .unwrap();
        assert!(!slot.present());
        let published = root.join("scratch/materialized");
        assert_eq!(std::fs::read(&published).unwrap(), b"tmpfile-body");
        assert_eq!(std::fs::metadata(&published).unwrap().nlink(), 1);
        drop(slot);
        assert!(published.exists());
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    pub(super) fn materializing_onto_an_existing_name_reports_exists() {
        let root = scratch();
        let (_file, name) = create(&root);
        std::fs::write(root.join("scratch/taken"), b"other").unwrap();
        let slot = AnonymousSlot::default();
        slot.set(Some(name));
        let directory = File::open(root.join("scratch")).unwrap();
        assert_eq!(
            slot.materialize(directory.as_raw_fd(), &CString::new("taken").unwrap())
                .unwrap(),
            Err(RuntimePathError::Exists)
        );
        assert!(slot.present());
        assert_eq!(std::fs::read(root.join("scratch/taken")).unwrap(), b"other");
        std::fs::remove_dir_all(&root).unwrap();
    }

    #[test]
    pub(super) fn a_real_anonymous_inode_defers_to_linking() {
        assert!(
            AnonymousSlot::default()
                .materialize(-1, &CString::new("unused").unwrap())
                .is_none()
        );
    }
}
