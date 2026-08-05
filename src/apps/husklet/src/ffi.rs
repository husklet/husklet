//! Private operating-system boundary for the Husklet application.

use std::os::fd::AsRawFd;

/// An exclusively locked file whose descriptor owns the lock lifetime.
pub(crate) struct ExclusiveFileLock(std::fs::File);

impl ExclusiveFileLock {
    /// Opens `path` and waits until this process owns its exclusive advisory lock.
    pub(crate) fn acquire(path: &std::path::Path) -> std::io::Result<Self> {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(path)?;
        // SAFETY: `file` owns a valid descriptor for the complete call and resulting lock lifetime.
        // `flock` retains no pointer, does not alias Rust storage, and cannot unwind across the ABI.
        if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error());
        }
        Ok(Self(file))
    }
}

impl Drop for ExclusiveFileLock {
    fn drop(&mut self) {
        // SAFETY: this value exclusively owns the live descriptor until after `flock` returns. The
        // operation retains no pointer, does not race Rust access, and cannot unwind across the ABI.
        let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

#[cfg(test)]
mod tests {
    use super::ExclusiveFileLock;

    #[test]
    fn blocks_competitor() {
        let temporary = tempfile::tempdir().unwrap();
        let path = temporary.path().join("lease");
        let lease = ExclusiveFileLock::acquire(&path).unwrap();
        let (entered, observed) = std::sync::mpsc::channel();
        let waiter = std::thread::spawn(move || {
            let _next = ExclusiveFileLock::acquire(&path).unwrap();
            entered.send(()).unwrap();
        });

        assert!(observed.recv_timeout(std::time::Duration::from_millis(50)).is_err());
        drop(lease);
        observed.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        waiter.join().unwrap();
    }
}
