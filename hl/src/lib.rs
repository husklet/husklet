//! `hl` library surface.
//!
//! Two layers share this crate:
//!
//!   * **Always-on** — the light, `hl`-side workspace [`config`] (the bare `hl_ws::Workspace`
//!     primitive extended with its feature settings — vpn/cuda/gui/docker_sock/scrollback — plus their
//!     persistence). std + `hl-ws` only, so the GUI can consume it with `default-features = false` and
//!     never pull the engine stack.
//!   * **`cli`-gated** — ALL of the `hl` command logic (run/app/workspace/daemon/context/install/…),
//!     the engine-linked stack. The `hl` binary (`src/bin/hl.rs`) is a thin parsing layer: it owns the
//!     clap `Cli`/`Cmd` and dispatches into these `pub` module fns. Gated behind the default `cli`
//!     feature so a `default-features = false` consumer gets only `config`.

pub mod config;

#[cfg(feature = "cli")]
pub mod agent;
#[cfg(feature = "cli")]
pub mod app;
#[cfg(feature = "cli")]
pub mod context;
#[cfg(feature = "cli")]
pub mod daemon;
#[cfg(feature = "cli")]
pub mod hl_launcher;
#[cfg(feature = "cli")]
pub mod install;
#[cfg(feature = "cli")]
pub mod paths;
#[cfg(feature = "cli")]
pub mod platform;
#[cfg(feature = "cli")]
pub mod report;
#[cfg(feature = "cli")]
pub mod run;
#[cfg(feature = "cli")]
pub mod workspace;
#[cfg(feature = "cli")]
pub mod wsdaemon;
