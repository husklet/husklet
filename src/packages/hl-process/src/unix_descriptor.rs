//! Audited wrappers for process-global Unix descriptor operations.

#![allow(unsafe_code)]

use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

/// Stable filesystem identity of an open descriptor.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Identity {
    /// Filesystem device identifier.
    pub device: u64,
    /// Filesystem object identifier.
    pub object: u64,
}

impl Identity {
    /// Reads the stable filesystem identity of an open descriptor.
    ///
    /// This operation observes but never closes, duplicates, or replaces the descriptor.
    ///
    /// # Errors
    /// Returns an operating-system error when descriptor metadata cannot be read.
    #[allow(clippy::unnecessary_cast)]
    pub fn of(descriptor: RawFd) -> io::Result<Self> {
        let mut status = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: status is valid writable storage and fstat initializes it on success.
        if unsafe { libc::fstat(descriptor, status.as_mut_ptr()) } < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful fstat initialized the complete stat value.
        let status = unsafe { status.assume_init() };
        Ok(Self {
            device: status.st_dev as u64,
            object: status.st_ino as u64,
        })
    }
}

/// Advisory whole-file lock operation.
#[derive(Clone, Copy, Debug)]
pub enum Lock {
    /// Acquire an exclusive lock without waiting.
    ExclusiveNonblocking,
    /// Release this descriptor's lock.
    Unlock,
}

/// One process-global standard descriptor slot.
#[derive(Clone, Copy, Debug)]
pub enum StandardDescriptor {
    /// Standard input, descriptor zero.
    Input,
    /// Standard output, descriptor one.
    Output,
    /// Standard error, descriptor two.
    Error,
}

impl StandardDescriptor {
    /// Closes this slot until the returned guard is restored or dropped.
    ///
    /// This changes process-global state. Callers must serialize other operations that
    /// consume or replace standard descriptors for the lifetime of the guard.
    ///
    /// # Errors
    /// Returns an operating-system error when the descriptor cannot be saved or closed.
    pub fn close(self) -> io::Result<ClosedStandardDescriptor> {
        let target = self.raw();
        let flags = get_flags(target)?;
        let saved = duplicate_at_least(target, 128)?;
        // SAFETY: standard descriptors are process-global slots rather than Rust-owned
        // values. The guard owns the temporary vacancy and restores it before release.
        if unsafe { libc::close(target) } < 0 {
            let close_error = io::Error::last_os_error();
            if close_error.raw_os_error() != Some(libc::EINTR) {
                return Err(close_error);
            }
            if !closed_after_interruption(target, saved.as_raw_fd(), flags)? {
                return Err(close_error);
            }
        }
        Ok(ClosedStandardDescriptor {
            target,
            saved: Some(saved),
            flags,
        })
    }

    const fn raw(self) -> RawFd {
        match self {
            Self::Input => libc::STDIN_FILENO,
            Self::Output => libc::STDOUT_FILENO,
            Self::Error => libc::STDERR_FILENO,
        }
    }
}

/// RAII ownership of a temporarily closed process-global standard descriptor.
///
/// Explicit restoration reports failures. Dropping makes a best-effort restoration during unwind.
pub struct ClosedStandardDescriptor {
    target: RawFd,
    saved: Option<OwnedFd>,
    flags: i32,
}

impl ClosedStandardDescriptor {
    fn restore_inner(&mut self) -> io::Result<()> {
        let Some(saved) = self.saved.as_ref() else {
            return Ok(());
        };
        replace(saved.as_raw_fd(), self.target)?;
        set_flags(self.target, self.flags)?;
        drop(self.saved.take());
        Ok(())
    }

    /// Restores the descriptor now instead of waiting for `Drop`.
    ///
    /// # Errors
    /// Returns an operating-system error when the original descriptor cannot be restored.
    pub fn restore(mut self) -> io::Result<()> {
        self.restore_inner()
    }
}

impl Drop for ClosedStandardDescriptor {
    fn drop(&mut self) {
        // Unwind cannot carry the error out, and a vacant standard slot is process-global: every
        // later open silently takes descriptor 0, 1 or 2 and the cause is untraceable from where it
        // surfaces. Naming the slot and the operating-system error here is the only report there is.
        if let Err(error) = self.restore_inner() {
            hl_log::hl_error!(
                hl_log::tag::FS,
                "standard descriptor {} was not restored on drop and its slot stays vacant: {error}",
                self.target
            );
        }
    }
}

/// Applies an advisory whole-file lock operation to an open descriptor.
///
/// This operation does not take ownership of the descriptor.
///
/// # Errors
/// Returns an operating-system error when the lock operation cannot be completed.
pub fn lock(descriptor: RawFd, operation: Lock) -> io::Result<()> {
    let operation = match operation {
        Lock::ExclusiveNonblocking => libc::LOCK_EX | libc::LOCK_NB,
        Lock::Unlock => libc::LOCK_UN,
    };
    // SAFETY: flock has no pointer argument and does not take ownership.
    if unsafe { libc::flock(descriptor, operation) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Reports whether an interrupted `close` had already vacated the slot, restoring it when it had not.
///
/// POSIX leaves descriptor state unspecified after EINTR. Under the documented serialization
/// contract the slot cannot be reused here, so F_GETFD distinguishes an already-completed close
/// from a live slot.
///
/// # Errors
/// Returns an operating-system error when a live slot cannot be restored from `saved`.
fn closed_after_interruption(target: RawFd, saved: RawFd, flags: i32) -> io::Result<bool> {
    match get_flags(target) {
        Err(error) if error.raw_os_error() == Some(libc::EBADF) => Ok(true),
        Ok(_) => Ok(false),
        Err(_) => {
            replace(saved, target)?;
            set_flags(target, flags)?;
            Ok(false)
        }
    }
}

fn get_flags(descriptor: RawFd) -> io::Result<i32> {
    // SAFETY: F_GETFD has no pointer argument and does not take ownership.
    let flags = unsafe { libc::fcntl(descriptor, libc::F_GETFD) };
    if flags < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(flags)
    }
}

fn duplicate_at_least(descriptor: RawFd, minimum: RawFd) -> io::Result<OwnedFd> {
    // SAFETY: F_DUPFD_CLOEXEC creates a new uniquely owned descriptor.
    let duplicate = unsafe { libc::fcntl(descriptor, libc::F_DUPFD_CLOEXEC, minimum) };
    if duplicate < 0 {
        Err(io::Error::last_os_error())
    } else {
        // SAFETY: fcntl returned a new descriptor owned by this function.
        Ok(unsafe { OwnedFd::from_raw_fd(duplicate) })
    }
}

fn replace(source: RawFd, target: RawFd) -> io::Result<()> {
    if source == target {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "source and target descriptors are equal",
        ));
    }
    // SAFETY: target is the vacant standard slot owned by the guard; source is
    // its live saved descriptor and remains owned by the guard.
    if unsafe { libc::dup2(source, target) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if let Err(error) = set_flags(target, libc::FD_CLOEXEC) {
        // SAFETY: this function just created target and owns that failed result.
        unsafe { libc::close(target) };
        return Err(error);
    }
    Ok(())
}

fn set_flags(descriptor: RawFd, flags: i32) -> io::Result<()> {
    // SAFETY: F_SETFD has no pointer argument and does not take ownership.
    if unsafe { libc::fcntl(descriptor, libc::F_SETFD, flags) } < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}
