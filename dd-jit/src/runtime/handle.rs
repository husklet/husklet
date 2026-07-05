//! [`RunHandle`] — the synchronous wait/signal handle returned by [`Runtime::run`](super::Runtime::run)
//! — and [`ExitStatus`], a finished container's exit code.

use super::error::Error;

/// A running container. Wait on it or send it a signal.
pub struct RunHandle {
    pub(crate) child: std::process::Child,
}

impl RunHandle {
    /// Block until the container exits.
    pub fn wait(&mut self) -> Result<ExitStatus, Error> {
        let st = self.child.wait()?;
        Ok(ExitStatus { code: st.code().unwrap_or(-1) })
    }

    /// The container's host process id.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Signal the container (e.g. `libc::SIGTERM`).
    pub fn signal(&self, sig: i32) -> Result<(), Error> {
        // Safety: kill(2) on our own child's pid; a stale pid just returns ESRCH.
        let r = unsafe { libc_kill(self.child.id() as i32, sig) };
        if r != 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        Ok(())
    }
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
}

/// The exit status of a finished container.
#[derive(Clone, Copy, Debug)]
pub struct ExitStatus {
    pub(crate) code: i32,
}

impl ExitStatus {
    /// The process exit code (-1 if terminated by signal / unavailable).
    pub fn code(&self) -> i32 {
        self.code
    }

    /// Whether the container exited successfully (code 0).
    pub fn success(&self) -> bool {
        self.code == 0
    }
}
