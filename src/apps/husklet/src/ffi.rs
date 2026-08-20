//! Private operating-system boundary for the Husklet application.

// The termios and advisory-lock calls in this boundary are `unsafe` libc entry points.
#![allow(unsafe_code)]

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
            if libc::tcgetattr(descriptor, &raw mut attributes) != 0 {
                return Self {
                    descriptor,
                    saved: None,
                };
            }
            let saved = attributes;
            libc::cfmakeraw(&raw mut attributes);
            libc::tcsetattr(descriptor, libc::TCSANOW, &raw const attributes);
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
            libc::tcsetattr(self.descriptor, libc::TCSANOW, &raw const saved);
        }
    }
}

/// Blocks the terminal-generated terminating signals for the window in which a pane worker has
/// no raw mode of its own.
///
/// A pane worker inherits a **cooked** PTY from the application. Until `RawMode::enter` runs, that
/// line discipline turns a Ctrl-C into `SIGINT` delivered to the worker, whose default disposition
/// terminates it; the pane then reports `workspace session ended (signal 2)` for the worker's wait
/// status, not for anything the guest did. The window is the whole of `execution::launch`, which on
/// a reopened workspace lasts as long as the restore takes -- and the pane has already replayed the
/// previous session's scrollback by then, so the user is looking at a prompt and types into it.
///
/// Blocking is deliberately narrow. Once raw mode is in effect the line discipline generates no
/// signal at all and Ctrl-C travels to the guest as the `0x03` byte the relay already forwards, so
/// releasing this value restores the inherited mask without changing what reaches the guest.
/// Instances that arrived while blocked are discarded on release rather than delivered late: they
/// were typed at a launcher that was never able to act on them.
pub(crate) struct InterruptMask {
    previous: libc::sigset_t,
    released: bool,
}

impl InterruptMask {
    /// The tty-generated signals whose default disposition terminates the worker.
    const BLOCKED: [libc::c_int; 2] = [libc::SIGINT, libc::SIGQUIT];

    /// Blocks the interrupt signals until this value is released or dropped.
    pub(crate) fn block() -> Self {
        // SAFETY: both sets are initialized C aggregates owned by this frame for the whole call.
        // `sigprocmask` retains no pointer, aliases no Rust storage, invokes no callback, and
        // cannot unwind across the ABI.
        let previous = unsafe {
            let mut blocked: libc::sigset_t = std::mem::zeroed();
            let mut previous: libc::sigset_t = std::mem::zeroed();
            libc::sigemptyset(&raw mut blocked);
            for signal in Self::BLOCKED {
                libc::sigaddset(&raw mut blocked, signal);
            }
            libc::sigprocmask(libc::SIG_BLOCK, &raw const blocked, &raw mut previous);
            previous
        };
        Self {
            previous,
            released: false,
        }
    }

    /// Restores the inherited mask, discarding anything that arrived while blocked.
    pub(crate) fn release(mut self) {
        self.restore();
    }

    /// Reports whether a blocked interrupt signal is currently pending for this process.
    #[cfg(test)]
    pub(crate) fn pending() -> bool {
        // SAFETY: `pending` is an initialized C aggregate owned by this frame for the whole call.
        unsafe {
            let mut pending: libc::sigset_t = std::mem::zeroed();
            if libc::sigpending(&raw mut pending) != 0 {
                return false;
            }
            Self::BLOCKED
                .into_iter()
                .any(|signal| libc::sigismember(&raw const pending, signal) == 1)
        }
    }

    fn restore(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        // SAFETY: each call names a signal number, a process-lifetime handler constant, or this
        // value's own initialized mask. Ignoring a signal before unblocking is what discards a
        // pending instance; the saved disposition is reinstated immediately afterwards. None of
        // these calls retains a pointer, aliases Rust storage, or can unwind across the ABI.
        unsafe {
            let saved = Self::BLOCKED.map(|signal| (signal, libc::signal(signal, libc::SIG_IGN)));
            libc::sigprocmask(libc::SIG_SETMASK, &raw const self.previous, std::ptr::null_mut());
            for (signal, handler) in saved {
                libc::signal(signal, handler);
            }
        }
    }
}

impl Drop for InterruptMask {
    fn drop(&mut self) {
        self.restore();
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
    use super::{ExclusiveFileLock, InterruptMask};

    fn interrupt_blocked_now() -> bool {
        // SAFETY: `mask` is an initialized C aggregate owned by this frame for the whole call.
        unsafe {
            let mut mask: libc::sigset_t = std::mem::zeroed();
            libc::sigprocmask(libc::SIG_BLOCK, std::ptr::null(), &raw mut mask);
            libc::sigismember(&raw const mask, libc::SIGINT) == 1
        }
    }

    /// The launch-window half of the contract: a Ctrl-C typed before the pane worker owns raw mode
    /// must not terminate it, and must not be delivered late once it does.
    #[test]
    fn interrupts_typed_before_raw_mode_neither_kill_the_worker_nor_arrive_afterwards() {
        assert!(
            !interrupt_blocked_now(),
            "test thread starts with interrupts deliverable"
        );
        let mask = InterruptMask::block();
        assert!(interrupt_blocked_now());
        // SAFETY: `raise` names a signal number and targets this thread only.
        unsafe {
            libc::raise(libc::SIGINT);
            libc::raise(libc::SIGQUIT);
        }
        assert!(
            InterruptMask::pending(),
            "a Ctrl-C during launch must be held, not delivered to the default disposition"
        );

        mask.release();

        assert!(
            !InterruptMask::pending(),
            "a held interrupt must be discarded on release, not delivered late"
        );
        assert!(
            !interrupt_blocked_now(),
            "releasing must restore the inherited mask so nothing stays blocked for the session"
        );
    }

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
