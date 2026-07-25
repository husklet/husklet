//! The PTY backend seam: a source of a bidirectional terminal byte stream + a resize channel.
//!
//! The [`PtyBackend`] trait itself is the shared terminal-interface primitive defined in the leaf `hl-ws`
//! (so `hl-ws`'s `Launcher` can name the handle it returns without depending on any terminal crate). This
//! crate depends UP on `hl-ws` and IMPLEMENTS that trait:
//!   * [`local::LocalPty`] — `openpty` + fork a host shell. Works on any Unix (this is what makes the
//!     whole terminal testable headlessly on the Linux dev host: spawn a real `bash`, drive it, assert
//!     the resulting grid).
//!   * Husklet's container adapter — a shell inside a workspace container.
//!
//! Both expose a real pollable master fd, so the GUI event loop can `poll`/`epoll`/`kqueue` them uniformly.

pub mod local;

/// Re-exported from `hl-ws` so `hl_ws_term::PtyBackend` (and `hl_ws_term::pty::PtyBackend`) keep resolving
/// for importers; the canonical definition is the shared primitive `hl_ws::terminal::PtyBackend`.
pub use hl_ws::terminal::PtyBackend;
