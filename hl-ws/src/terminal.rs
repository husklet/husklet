//! The terminal/PTY interface — a primitive TRAIT shared across the `hl-ws-*` family.
//!
//! Defined here (in the leaf `hl-ws`) so the [`crate::launch::Launcher`] trait can name the handle it
//! returns without `hl-ws` depending on any concrete terminal crate. `hl-ws-term` depends on `hl-ws` and
//! IMPLEMENTS this for its `LocalPty` (and the engine's `HlJitPty` does likewise), so the edge always
//! points UP to `hl-ws` and never the other way.

use std::io;
use std::os::unix::io::RawFd;

/// A live pseudo-terminal connected to a shell (local or in-container). The concrete implementors live in
/// the terminal/engine crates; `hl-ws` only speaks this interface.
pub trait PtyBackend: Send {
    /// Write bytes to the shell's stdin (keystrokes / paste). Best-effort; short writes are retried.
    fn write(&mut self, bytes: &[u8]) -> io::Result<()>;

    /// Read currently-available output bytes into `buf` (non-blocking). `Ok(0)` means "nothing right now"
    /// (would-block) OR end-of-file — disambiguate with [`PtyBackend::try_wait`].
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;

    /// Tell the shell its window is now `cols × rows` (TIOCSWINSZ on the master).
    fn resize(&mut self, cols: u16, rows: u16);

    /// The pollable master fd, so a GUI/select loop can wait for readability. `None` if not applicable.
    fn master_fd(&self) -> Option<RawFd>;

    /// Reap the child. `Some(exit_code)` once it has exited; `None` while still running.
    fn try_wait(&mut self) -> Option<i32>;
}
