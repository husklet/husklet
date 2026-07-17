//! [`RunHandle`] — the synchronous wait/signal handle returned by [`Runtime::run`](super::Runtime::run)
//! — and [`ExitStatus`], a finished container's exit code.

use super::error::Error;

/// A running container. Wait on it or send it a signal. The container is forked by the engine FFI
/// (`hl_spawn`), so the handle owns just the raw pid and reaps it with `waitpid(2)`.
pub struct RunHandle {
    pub(crate) pid: u32,
}

impl RunHandle {
    /// Block until the container exits.
    pub fn wait(&mut self) -> Result<ExitStatus, Error> {
        let mut status: i32 = 0;
        // Safety: waitpid(2) on our own forked child's pid; a valid `*mut status` is provided.
        let r = unsafe { libc_waitpid(self.pid as i32, &mut status, 0) };
        if r < 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        Ok(ExitStatus {
            code: decode_wait_status(status),
        })
    }

    /// The container's host process id.
    pub fn pid(&self) -> u32 {
        self.pid
    }

    /// Signal the container (e.g. `libc::SIGTERM`).
    pub fn signal(&self, sig: i32) -> Result<(), Error> {
        // Safety: kill(2) on our own child's pid; a stale pid just returns ESRCH.
        let r = unsafe { libc_kill(self.pid as i32, sig) };
        if r != 0 {
            return Err(Error::Io(std::io::Error::last_os_error()));
        }
        Ok(())
    }
}

/// Decode a `waitpid` status into an exit code: the normal exit code, or `128 + signum` when killed by
/// a signal (the POSIX-shell / Docker convention — SIGKILL→137, SIGTERM→143 — so `docker wait`,
/// `inspect .State.ExitCode`, the `die` event and `ps --filter exited=N` all report the same value the
/// real engine would). Shared by the sync [`RunHandle::wait`] here and the async `reap` in
/// [`super::engine`] (which casts the result to `i64`) — the single source of truth for the convention.
pub(crate) fn decode_wait_status(status: i32) -> i32 {
    // WIFEXITED: low 7 bits are 0 → WEXITSTATUS is bits 8..16. Otherwise the child was signalled: the
    // low 7 bits carry the signal number (the 0x80 core-dump flag is masked off), Docker reports 128+n.
    if status & 0x7f == 0 {
        (status >> 8) & 0xff
    } else {
        128 + (status & 0x7f)
    }
}

#[cfg(test)]
mod tests {
    use super::decode_wait_status;

    #[test]
    fn normal_exit_extracts_wexitstatus() {
        // WIFEXITED: low 7 bits are 0, exit code lives in bits 8..16 (`code << 8`).
        assert_eq!(decode_wait_status(5 << 8), 5);
        assert_eq!(decode_wait_status(0), 0);
        assert_eq!(decode_wait_status(255 << 8), 255);
    }

    #[test]
    fn signalled_returns_128_plus_signum() {
        // WIFSIGNALED: low 7 bits carry the signal number → Docker's 128+n (SIGKILL 9→137, SIGTERM 15→143).
        assert_eq!(decode_wait_status(9), 137);
        assert_eq!(decode_wait_status(libc::SIGKILL), 128 + libc::SIGKILL);
        assert_eq!(decode_wait_status(15), 143);
        // The 0x80 WCOREDUMP flag is masked off — a core-dumped SIGSEGV (11|0x80) is still 128+11=139.
        assert_eq!(decode_wait_status(libc::SIGSEGV | 0x80), 139);
        // A signalled status may also carry an unrelated high byte — only the low 7 bits count.
        assert_eq!(decode_wait_status((7 << 8) | 15), 143);
    }
}

extern "C" {
    #[link_name = "kill"]
    fn libc_kill(pid: i32, sig: i32) -> i32;
    #[link_name = "waitpid"]
    fn libc_waitpid(pid: i32, status: *mut i32, options: i32) -> i32;
}

/// The exit status of a finished container.
#[derive(Clone, Copy, Debug)]
pub struct ExitStatus {
    pub(crate) code: i32,
}

impl ExitStatus {
    /// The process exit code (`128 + signum` if terminated by signal, per the Docker/shell convention).
    pub fn code(&self) -> i32 {
        self.code
    }

    /// Whether the container exited successfully (code 0).
    pub fn success(&self) -> bool {
        self.code == 0
    }
}
