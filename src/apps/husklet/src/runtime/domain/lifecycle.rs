use std::io;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;

const WAITING: u8 = 0;
const KILL: u8 = 1;
const CHECKPOINT: u8 = 2;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Disposition {
    Kill,
    Checkpoint,
}

/// What asked the domain to stop, so a dying domain can name its own reason.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum Trigger {
    Signal,
    Request,
}

/// One decided stop: what the domain will do, and who asked for it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct Stop {
    pub(super) disposition: Disposition,
    pub(super) trigger: Trigger,
}

impl Trigger {
    pub(super) fn describe(self) -> &'static str {
        match self {
            Self::Signal => "terminating signal",
            Self::Request => "close request",
        }
    }
}

impl Disposition {
    pub(super) fn describe(self) -> &'static str {
        match self {
            Self::Kill => "stopping without checkpoint",
            Self::Checkpoint => "checkpointing then stopping",
        }
    }
}

struct Control {
    listener: tokio::net::UnixListener,
    path: PathBuf,
}

#[derive(Clone)]
pub(super) struct Shutdown {
    disposition: Arc<AtomicU8>,
    control: Arc<Control>,
}

impl Shutdown {
    pub(super) fn bind(path: PathBuf) -> io::Result<Self> {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(Self {
            disposition: Arc::new(AtomicU8::new(WAITING)),
            control: Arc::new(Control {
                listener: tokio::net::UnixListener::bind(&path)?,
                path,
            }),
        })
    }

    pub(super) fn disposition(&self) -> Disposition {
        match self.disposition.load(Ordering::Acquire) {
            CHECKPOINT => Disposition::Checkpoint,
            _ => Disposition::Kill,
        }
    }

    pub(super) fn request(path: &Path, disposition: Disposition) -> io::Result<()> {
        let mut connection = std::os::unix::net::UnixStream::connect(path)?;
        connection.write_all(&[disposition.code()])
    }

    pub(super) async fn wait(self) -> Stop {
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        let (disposition, trigger) = match terminate {
            Ok(mut terminate) => {
                tokio::select! {
                    _ = tokio::signal::ctrl_c() => (KILL, Trigger::Signal),
                    _ = terminate.recv() => (KILL, Trigger::Signal),
                    disposition = self.control.request() => (disposition, Trigger::Request),
                }
            }
            _ => (self.control.request().await, Trigger::Request),
        };
        self.disposition.store(disposition, Ordering::Release);
        Stop {
            disposition: self.disposition(),
            trigger,
        }
    }
}

impl Control {
    async fn request(&self) -> u8 {
        loop {
            let Ok((mut connection, _)) = self.listener.accept().await else {
                return KILL;
            };
            let mut byte = [0_u8; 1];
            if tokio::io::AsyncReadExt::read_exact(&mut connection, &mut byte)
                .await
                .is_ok()
                && matches!(byte[0], KILL | CHECKPOINT)
            {
                return byte[0];
            }
        }
    }
}

impl Drop for Control {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

impl Disposition {
    fn code(self) -> u8 {
        match self {
            Self::Kill => KILL,
            Self::Checkpoint => CHECKPOINT,
        }
    }
}

pub(super) struct Lease {
    _lock: ffi::FileLock,
}

impl Lease {
    pub(super) fn acquire(path: &Path) -> io::Result<Self> {
        match ffi::FileLock::try_acquire(path)? {
            Some(lock) => Ok(Self { _lock: lock }),
            None => Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "workspace execution domain is already starting",
            )),
        }
    }

    pub(super) fn acquire_wait(path: &Path, timeout: std::time::Duration) -> io::Result<Self> {
        let deadline = std::time::Instant::now() + timeout;
        loop {
            match Self::acquire(path) {
                Ok(lease) => return Ok(lease),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error),
            }
            if std::time::Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("timed out waiting for {}", path.display()),
                ));
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    pub(super) fn wait_available(path: &Path, timeout: std::time::Duration) -> io::Result<()> {
        drop(Self::acquire_wait(path, timeout)?);
        Ok(())
    }
}

/// Private file-lock boundary consumed through an owned safe lease.
mod ffi {
    // The advisory-lock calls in this boundary are `unsafe` libc entry points.
    #![allow(unsafe_code)]

    use std::os::fd::AsRawFd;

    pub(super) struct FileLock(std::fs::File);

    impl FileLock {
        pub(super) fn try_acquire(path: &std::path::Path) -> std::io::Result<Option<Self>> {
            let file = std::fs::OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(path)?;
            // SAFETY: `file` uniquely owns a valid descriptor through the call and, on success,
            // for the lock lifetime. `flock` retains no pointer, invokes no callback, accesses no
            // aliased Rust storage, and cannot unwind across the ABI.
            if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                return Ok(Some(Self(file)));
            }
            let error = std::io::Error::last_os_error();
            if contention(&error) {
                Ok(None)
            } else {
                Err(error)
            }
        }
    }

    #[hl_design::classify(domain = "lock")]
    fn contention(error: &std::io::Error) -> bool {
        let code = error.raw_os_error();
        code == Some(libc::EWOULDBLOCK) || code == Some(libc::EAGAIN)
    }

    impl Drop for FileLock {
        fn drop(&mut self) {
            // SAFETY: this value uniquely owns the live descriptor until after the call. `flock`
            // retains no pointer, invokes no callback, accesses no aliased Rust storage, and cannot
            // unwind across the ABI. Closing the file after return releases all remaining state.
            let _ = unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
        }
    }

    #[cfg(test)]
    mod tests {
        #[test]
        fn only_would_block_errors_are_lock_contention() {
            for code in [libc::EWOULDBLOCK, libc::EAGAIN] {
                assert!(super::contention(&std::io::Error::from_raw_os_error(code)));
            }
            for code in [libc::EINTR, libc::EBADF, libc::EINVAL, libc::ENOLCK, libc::EIO] {
                assert!(!super::contention(&std::io::Error::from_raw_os_error(code)));
            }
            assert!(!super::contention(&std::io::Error::other("host failure")));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Disposition, Lease, Shutdown, Trigger, CHECKPOINT, KILL};
    use std::sync::atomic::Ordering;

    #[tokio::test]
    async fn shutdown_defaults_to_kill_and_checkpoint_requires_an_explicit_request() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("control.sock");
        let shutdown = Shutdown::bind(path.clone()).unwrap();
        assert_eq!(shutdown.disposition(), Disposition::Kill);

        let waiting = shutdown.clone();
        let task = tokio::spawn(waiting.wait());
        Shutdown::request(&path, Disposition::Checkpoint).unwrap();

        let stop = task.await.unwrap();
        assert_eq!(stop.disposition, Disposition::Checkpoint);
        assert_eq!(stop.trigger, Trigger::Request);
        assert_eq!(shutdown.disposition.load(Ordering::Acquire), CHECKPOINT);

        shutdown.disposition.store(KILL, Ordering::Release);
        assert_eq!(shutdown.disposition(), Disposition::Kill);
    }

    #[test]
    fn lease_waits_for_the_previous_owner_to_finish_cleanup() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("domain.lock");
        let lease = Lease::acquire(&path).unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(60));
            drop(lease);
        });
        let started = std::time::Instant::now();

        Lease::wait_available(&path, std::time::Duration::from_secs(1)).unwrap();

        assert!(started.elapsed() >= std::time::Duration::from_millis(40));
        release.join().unwrap();
    }
}
