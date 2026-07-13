//! The PTY backend seam: a source of a bidirectional terminal byte stream + a resize channel.
//!
//! One trait, two implementations behind it:
//!   * [`local::LocalPty`] — `openpty` + fork a host shell. Works on any Unix (this is what makes the
//!     whole terminal testable headlessly on the Linux dev host: spawn a real `bash`, drive it, assert
//!     the resulting grid).
//!   * `hl_jit::DdJitPty` (macOS) — a shell inside a dd workspace *container* via `hl_jit::Runtime`.
//!
//! Both expose a real pollable master fd (dd-jit's `pty_master()` and `openpty`'s master are both
//! ordinary host fds), so the GUI event loop can `poll`/`epoll`/`kqueue` them uniformly.

use std::os::unix::io::RawFd;

pub mod local;

/// A live pseudo-terminal connected to a shell (local or in-container).
pub trait PtyBackend: Send {
    /// Write bytes to the shell's stdin (keystrokes / paste). Best-effort; short writes are retried.
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<()>;

    /// Read currently-available output bytes into `buf` (non-blocking). `Ok(0)` means "nothing right
    /// now" (would-block) OR end-of-file — disambiguate with [`PtyBackend::try_wait`].
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize>;

    /// Tell the shell its window is now `cols × rows` (TIOCSWINSZ on the master).
    fn resize(&mut self, cols: u16, rows: u16);

    /// The pollable master fd, so a GUI/select loop can wait for readability. `None` if not applicable.
    fn master_fd(&self) -> Option<RawFd>;

    /// Reap the child. `Some(exit_code)` once it has exited; `None` while still running.
    fn try_wait(&mut self) -> Option<i32>;
}
