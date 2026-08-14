//! Output draining and owned-child lifecycle over the raw process.

#![allow(unsafe_code)]

use super::{POLL, TERM_GRACE};
use std::fs::File;
use std::os::fd::AsRawFd;
use std::os::unix::process::ExitStatusExt;
use std::process::Child;
use std::thread;
use std::time::Instant;

pub(super) fn nonblocking(source: &File) -> std::io::Result<()> {
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
        #[cfg(target_os = "linux")]
        let descendants = {
            // Freeze the original group first. Each escaped descendant is then
            // frozen before its children are enumerated, closing the fork race
            // while the complete ownership tree is captured.
            self.signal(libc::SIGSTOP)?;
            super::tree::Descendants::freeze(self.group)?
        };
        self.signal(libc::SIGTERM)?;
        #[cfg(target_os = "linux")]
        descendants.signal(libc::SIGTERM)?;
        #[cfg(target_os = "linux")]
        {
            descendants.signal(libc::SIGCONT)?;
            self.signal(libc::SIGCONT)?;
        }
        let deadline = Instant::now() + TERM_GRACE;
        while (!self.reaped || {
            #[cfg(target_os = "linux")]
            {
                descendants.exists()
            }
            #[cfg(not(target_os = "linux"))]
            {
                false
            }
        }) && Instant::now() < deadline
        {
            match self.try_wait() {
                Ok(None) => thread::sleep(POLL),
                Ok(Some(_)) | Err(_) => break,
            }
        }
        self.signal(libc::SIGKILL)?;
        #[cfg(target_os = "linux")]
        descendants.signal(libc::SIGKILL)?;
        let _ = self.process.kill();
        if !self.reaped {
            self.process.wait()?;
            self.reaped = true;
        }
        self.quiesce()?;
        #[cfg(target_os = "linux")]
        if !descendants.settle() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "detached host subprocess descendants did not quiesce",
            ));
        }
        Ok(())
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
            // terminate() already escalates to SIGKILL; Drop has no channel to report on.
            match self.terminate() {
                Ok(()) | Err(_) => {}
            }
        }
    }
}
