//! Private operating-system boundary for the Husklet application.

use std::os::fd::{AsRawFd, RawFd};

/// RAII raw mode for a terminal descriptor; restores the saved attributes on drop.
pub(crate) struct RawMode {
    descriptor: RawFd,
    saved: Option<libc::termios>,
}

impl RawMode {
    pub(crate) fn enter(descriptor: RawFd) -> Self {
        // SAFETY: `termios` is an initialized C value before either kernel call can read it. The
        // inherited descriptor remains open for this value's lifetime; these calls retain no
        // pointer, alias no Rust storage, invoke no callback, and cannot unwind across the ABI.
        unsafe {
            let mut attributes: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(descriptor, &mut attributes) != 0 {
                return Self {
                    descriptor,
                    saved: None,
                };
            }
            let saved = attributes;
            libc::cfmakeraw(&mut attributes);
            libc::tcsetattr(descriptor, libc::TCSANOW, &attributes);
            Self {
                descriptor,
                saved: Some(saved),
            }
        }
    }
}

impl Drop for RawMode {
    fn drop(&mut self) {
        let Some(saved) = self.saved else {
            return;
        };
        // SAFETY: this owner keeps the inherited descriptor and initialized attributes alive for
        // the complete call. The kernel retains no pointer, accesses no aliased mutable Rust
        // storage, invokes no callback, and cannot unwind across the ABI.
        unsafe {
            libc::tcsetattr(self.descriptor, libc::TCSANOW, &saved);
        }
    }
}

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
