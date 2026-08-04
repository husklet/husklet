//! Host file-lock ownership for app lifecycle coordination.

#![allow(unsafe_code)]

use std::fs::{File, OpenOptions};
use std::path::Path;

/// Process-scoped exclusive file lock released by the host after a crash.
pub struct FileLock {
    file: File,
}

impl FileLock {
    pub fn acquire(path: impl AsRef<Path>) -> std::io::Result<Self> {
        let path = path.as_ref();
        #[cfg(unix)]
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        #[cfg(windows)]
        let file = {
            use std::os::windows::fs::OpenOptionsExt;

            OpenOptions::new()
                .read(true)
                .write(true)
                .create(true)
                .truncate(false)
                .share_mode(0)
                .open(path)?
        };
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;

            // SAFETY: flock receives one live owned descriptor and retains no
            // Rust pointer. The lock remains owned until `file` is dropped.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                return Err(std::io::Error::last_os_error());
            }
        }
        Ok(Self { file })
    }
}

impl Drop for FileLock {
    fn drop(&mut self) {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;

            // SAFETY: this owner still holds the live descriptor. Close also
            // releases the lock, so an unlock failure requires no fallback.
            let _ = unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}
