//! The PTY backend seam: a source of a bidirectional terminal byte stream + a resize channel.
//!
//! The [`PtyBackend`] trait itself is the shared terminal-interface primitive defined in the leaf `hl-ws`
//! (so `hl-ws`'s `Launcher` can name the handle it returns without depending on any terminal crate). This
//! crate depends UP on `hl-ws` and IMPLEMENTS that trait:
//!   * [`local::LocalPty`] — `openpty` + fork a host shell. Unix only, and declared so: the module and
//!     the `libc` edge that carries it are both `cfg(unix)`. This is what makes the whole terminal
//!     testable headlessly on the Linux dev host: spawn a real `bash`, drive it, assert the grid.
//!   * Husklet's container adapter — a shell inside a workspace container.
//!
//! Both expose a real pollable master fd, so the GUI event loop can `poll`/`epoll`/`kqueue` them uniformly.

// `local` is the POSIX-98 pty implementation (`posix_openpt`/`grantpt`/`unlockpt`/`ptsname` plus
// `fork`/`execvp`), so it exists exactly where `libc` supplies those symbols -- which is the same
// `cfg(unix)` the manifest now carries on the `libc` edge. Windows' equivalent is ConPTY
// (`CreatePseudoConsole`), a different mechanism and a different module; until one exists,
// `launcher.rs` refuses there and says so.
#[cfg(unix)]
pub mod local;

/// Re-exported from `hl-ws` so `hl_ws_term::PtyBackend` (and `hl_ws_term::pty::PtyBackend`) keep resolving
/// for importers; the canonical definition is the shared primitive `hl_ws::terminal::PtyBackend`.
pub use hl_ws::terminal::PtyBackend;
