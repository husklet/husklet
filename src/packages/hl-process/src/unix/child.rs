//! Output draining and owned-child lifecycle over the raw process.

#![allow(unsafe_code)]

use super::{POLL, TERM_GRACE};
use std::fs::File;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::process::ExitStatusExt;
use std::process::Child;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread;
use std::time::Instant;

pub(super) struct Drained {
    pub(super) bytes: Vec<u8>,
    pub(super) exceeded: bool,
}

pub(super) struct Drain {
    count: Arc<AtomicU64>,
    limit: u64,
    stopping: Arc<AtomicBool>,
    thread: thread::JoinHandle<std::io::Result<Vec<u8>>>,
}

impl Drain {
    pub(super) fn spawn(source: File, limit: u64) -> std::io::Result<Self> {
        nonblocking(&source)?;
        let count = Arc::new(AtomicU64::new(0));
        let observed = Arc::clone(&count);
        let stopping = Arc::new(AtomicBool::new(false));
        let stop = Arc::clone(&stopping);
        let thread = thread::spawn(move || Self::drain(source, limit, &observed, &stop));
        Ok(Self {
            count,
            limit,
            stopping,
            thread,
        })
    }

    fn drain(mut source: File, limit: u64, observed: &AtomicU64, stop: &AtomicBool) -> std::io::Result<Vec<u8>> {
        let capacity = usize::try_from(limit.min(1024 * 1024)).unwrap_or(1024 * 1024);
        let mut retained = Vec::with_capacity(capacity);
        let mut buffer = [0_u8; 16 * 1024];
        loop {
            let size = match source.read(&mut buffer) {
                Ok(size) => size,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock && stop.load(Ordering::Acquire) => {
                    break;
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(POLL);
                    continue;
                }
                Err(error) => return Err(error),
            };
            if size == 0 {
                break;
            }
            observed.fetch_add(size as u64, Ordering::Release);
            let available = usize::try_from(limit.saturating_sub(retained.len() as u64)).unwrap_or(usize::MAX);
            retained.extend_from_slice(&buffer[..size.min(available)]);
        }
        Ok(retained)
    }

    pub(super) fn exceeded(&self) -> bool {
        self.count.load(Ordering::Acquire) > self.limit
    }

    pub(super) fn finish(self) -> std::io::Result<Drained> {
        self.stopping.store(true, Ordering::Release);
        let bytes = self
            .thread
            .join()
            .map_err(|_| std::io::Error::other("subprocess capture thread panicked"))??;
        let exceeded = self.count.load(Ordering::Acquire) > self.limit;
        Ok(Drained { bytes, exceeded })
    }
}

fn nonblocking(source: &File) -> std::io::Result<()> {
    let descriptor = source.as_raw_fd();
    // SAFETY: `descriptor` is a live pipe descriptor owned by `source`.
    // F_GETFL reads integer descriptor flags, retains no pointer, and cannot
    // unwind or affect the descriptor lifetime.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFL) };
    if flags < 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: the same uniquely owned descriptor remains live. F_SETFL copies
    // the integer flags, retains no pointer, and concurrent code only reads the
    // pipe through the drain thread created after this call.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(())
}

pub(super) struct OwnedChild {
    process: Process,
    group: u32,
    reaped: bool,
}

impl OwnedChild {
    pub(super) fn new(process: Process) -> Self {
        let group = process.id();
        Self {
            process,
            group,
            reaped: false,
        }
    }

    pub(super) fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        let status = self.process.try_wait()?;
        self.reaped |= status.is_some();
        Ok(status)
    }

    pub(super) fn terminate(&mut self) -> std::io::Result<()> {
        self.signal(libc::SIGTERM)?;
        let deadline = Instant::now() + TERM_GRACE;
        while !self.reaped && Instant::now() < deadline {
            match self.try_wait() {
                Ok(None) => thread::sleep(POLL),
                Ok(Some(_)) | Err(_) => break,
            }
        }
        self.signal(libc::SIGKILL)?;
        let _ = self.process.kill();
        if !self.reaped {
            self.process.wait()?;
            self.reaped = true;
        }
        self.quiesce()
    }

    pub(super) fn quiesce(&self) -> std::io::Result<()> {
        self.signal(libc::SIGTERM)?;
        if self.settle() {
            return Ok(());
        }
        self.signal(libc::SIGKILL)?;
        if self.settle() {
            return Ok(());
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            "host subprocess group did not quiesce",
        ))
    }

    /// Waits out the grace period, reporting whether the group disappeared within it.
    pub(super) fn settle(&self) -> bool {
        let deadline = Instant::now() + TERM_GRACE;
        while self.group_exists() {
            if Instant::now() >= deadline {
                return false;
            }
            thread::sleep(POLL);
        }
        true
    }

    pub(super) fn signal(&self, signal: i32) -> std::io::Result<bool> {
        let group =
            i32::try_from(self.group).map_err(|_| std::io::Error::other("subprocess group exceeded host pid range"))?;
        // SAFETY: a negative, validated process-group ID and integer signal do
        // not reference Rust memory. The kernel owns process identity, and the
        // call cannot unwind or retain an alias.
        let result = unsafe { libc::kill(-group, signal) };
        if result == 0 {
            Ok(true)
        } else {
            let error = std::io::Error::last_os_error();
            if error.raw_os_error() == Some(libc::ESRCH) {
                Ok(false)
            } else {
                Err(error)
            }
        }
    }

    fn group_exists(&self) -> bool {
        self.signal(0).unwrap_or(true)
    }
}

pub(super) enum Process {
    Standard(Child),
    Exact(libc::pid_t),
}

impl Process {
    pub(super) fn id(&self) -> u32 {
        match self {
            Self::Standard(child) => child.id(),
            Self::Exact(pid) => u32::try_from(*pid).unwrap_or(u32::MAX),
        }
    }

    pub(super) fn try_wait(&mut self) -> std::io::Result<Option<std::process::ExitStatus>> {
        match self {
            Self::Standard(child) => child.try_wait(),
            Self::Exact(pid) => wait_pid(*pid, libc::WNOHANG),
        }
    }

    pub(super) fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        let pid = match self {
            Self::Standard(child) => return child.wait(),
            Self::Exact(pid) => *pid,
        };
        loop {
            if let Some(status) = wait_pid(pid, 0)? {
                break Ok(status);
            }
        }
    }

    pub(super) fn kill(&mut self) -> std::io::Result<()> {
        match self {
            Self::Standard(child) => child.kill(),
            Self::Exact(pid) => {
                // SAFETY: pid is the positive identity returned by posix_spawn.
                // The call touches no Rust memory, retains nothing, and cannot unwind.
                if unsafe { libc::kill(*pid, libc::SIGKILL) } == 0 {
                    return Ok(());
                }
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    Ok(())
                } else {
                    Err(error)
                }
            }
        }
    }
}

fn wait_pid(pid: libc::pid_t, flags: i32) -> std::io::Result<Option<std::process::ExitStatus>> {
    loop {
        let mut status = 0;
        // SAFETY: status is aligned writable integer storage, pid is a child
        // returned by posix_spawn, and waitpid retains no pointer or alias.
        let waited = unsafe { libc::waitpid(pid, &raw mut status, flags) };
        if waited == pid {
            return Ok(Some(std::process::ExitStatus::from_raw(status)));
        }
        if waited == 0 {
            return Ok(None);
        }
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() != Some(libc::EINTR) {
            return Err(error);
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.reaped || self.group_exists() {
            let _ = self.terminate();
        }
    }
}
