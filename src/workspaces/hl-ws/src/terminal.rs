//! The terminal/PTY interface — a primitive TRAIT shared across the `hl-ws-*` family.
//!
//! Defined here (in the leaf `hl-ws`) so the [`crate::launch::Launcher`] trait can name the handle it
//! returns without `hl-ws` depending on any concrete terminal crate. `hl-ws-term` depends on `hl-ws` and
//! IMPLEMENTS this for its `LocalPty` (and the engine's `HlJitPty` does likewise), so the edge always
//! points UP to `hl-ws` and never the other way.

use std::io;

/// The host handle a GUI or select loop waits on to learn the terminal has output.
///
/// A pseudo-terminal master is a different object on each host and the two are not
/// interchangeable: on Unix it is the file descriptor `openpty(3)` returns, which a
/// `poll(2)` set can carry directly; on Windows a `ConPTY` hands back the read end of its
/// output pipe as a `HANDLE`, which is waited on with `WaitForMultipleObjects`. The
/// standard library spells each under its own `std::os` subtree and offers no portable
/// name for either, so this names both rather than inventing a third.
///
/// `hl-ws` only names the handle. Every implementor, and every event loop that waits on
/// one, lives in a crate that owns a terminal -- `hl-ws-term` for the host shell, the
/// engine for a guest pty.
#[cfg(unix)]
pub type PtyDescriptor = std::os::unix::io::RawFd;

/// The Windows spelling of [`PtyDescriptor`]; see that alias for why there are two.
#[cfg(windows)]
pub type PtyDescriptor = std::os::windows::io::RawHandle;

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

    /// The pollable pseudo-terminal master, so a GUI/select loop can wait for readability. `None` if
    /// not applicable -- a backend that pumps its own I/O over a transport has no handle to offer.
    fn master_descriptor(&self) -> Option<PtyDescriptor>;

    /// Reap the child. `Some(exit_code)` once it has exited; `None` while still running.
    fn try_wait(&mut self) -> Option<i32>;
}
