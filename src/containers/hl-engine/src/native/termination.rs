#![allow(unsafe_code)]

use std::sync::atomic::{AtomicI32, Ordering};

use super::HostError;

static PENDING: AtomicI32 = AtomicI32::new(0);

/// Process termination requests converted into a pollable, async-safe flag.
///
/// The owner must poll [`pending`](Self::pending), tear down its children, and
/// only then leave the process. A second request remains represented by the
/// same flag; SIGKILL is intentionally outside this cooperative contract.
pub struct TerminationSignals {
    #[cfg(unix)]
    previous: [(i32, libc::sigaction); 3],
}

impl TerminationSignals {
    /// Installs handlers for HUP, INT, and TERM in the current process.
    pub fn install() -> Result<Self, HostError> {
        PENDING.store(0, Ordering::Release);
        #[cfg(unix)]
        return unix::install();
        #[cfg(not(unix))]
        Err(HostError::Unsupported)
    }

    /// Returns the first pending signal without clearing it.
    #[must_use]
    pub fn pending() -> Option<i32> {
        match PENDING.load(Ordering::Acquire) {
            0 => None,
            signal => Some(signal),
        }
    }

    #[cfg(test)]
    pub(crate) fn mark(signal: i32) {
        handler(signal);
    }

    #[cfg(unix)]
    fn restore(installed: &[(i32, libc::sigaction)]) {
        for (signal, previous) in installed.iter().rev() {
            // SAFETY: each value was initialized by a successful sigaction.
            let _ = unsafe { libc::sigaction(*signal, previous, std::ptr::null_mut()) };
        }
    }
}

#[cfg(unix)]
impl Drop for TerminationSignals {
    fn drop(&mut self) {
        for (signal, previous) in &self.previous {
            // SAFETY: previous is the complete sigaction returned for this
            // signal at installation. libc borrows it only for this call.
            let _ = unsafe { libc::sigaction(*signal, previous, std::ptr::null_mut()) };
        }
        PENDING.store(0, Ordering::Release);
    }
}

extern "C" fn handler(signal: i32) {
    let _ = PENDING.compare_exchange(0, signal, Ordering::AcqRel, Ordering::Acquire);
}

#[cfg(unix)]
mod unix {
    use super::{HostError, TerminationSignals, handler};

    pub(super) fn install() -> Result<TerminationSignals, HostError> {
        let mut installed = Vec::new();
        for signal in [libc::SIGHUP, libc::SIGINT, libc::SIGTERM] {
            // SAFETY: zero is a valid starting representation for sigaction;
            // every field used below is initialized before the syscall.
            let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
            action.sa_sigaction = handler as *const () as usize;
            // SAFETY: mask is uniquely writable and retained in action.
            if unsafe { libc::sigemptyset(&mut action.sa_mask) } != 0 {
                TerminationSignals::restore(&installed);
                return Err(HostError::Failed);
            }
            action.sa_flags = 0;
            // SAFETY: previous is uniquely writable and action remains live;
            // sigaction copies both values and retains no Rust pointer.
            let mut previous = unsafe { std::mem::zeroed::<libc::sigaction>() };
            if unsafe { libc::sigaction(signal, &action, &mut previous) } != 0 {
                TerminationSignals::restore(&installed);
                return Err(HostError::Failed);
            }
            installed.push((signal, previous));
        }
        let previous = installed.try_into().map_err(|_| HostError::Failed)?;
        Ok(TerminationSignals { previous })
    }
}

#[cfg(test)]
mod test {
    use super::TerminationSignals;

    #[test]
    fn first_signal_wins() {
        TerminationSignals::mark(2);
        TerminationSignals::mark(15);
        assert_eq!(TerminationSignals::pending(), Some(2));
    }
}
