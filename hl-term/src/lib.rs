//! hl-term — TRANSITIONAL crate holding only the workspace model during the composition-root split.
//!
//! The terminal primitive (VT/grid/input/render/pty) now lives in `hl-ws-term`; the workspace model +
//! persistence + launch seam below is next to move into `hl-ws`. This crate re-exports `hl_ws_term` so
//! existing `hl_term::{Vt, PtyBackend, …}` importers keep compiling until they are repointed, and is
//! removed once `hl-ws` exists.

pub use hl_ws_term::*;

pub mod workspace;

pub use workspace::{Arch, CudaDevice, Launcher, LocalShellLauncher, Workspace, WorkspaceStore};
