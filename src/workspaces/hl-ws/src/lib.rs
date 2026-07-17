//! hl-ws — the VERY THIN, shared workspace foundation for the `hl-ws-*` family.
//!
//! std-ONLY and the LEAF of its family: it holds only the bare-minimum run primitive + trait seams common
//! to all consumers, and references NO other hl crate (verify with `cargo tree -p hl-ws`: zero `hl-*`
//! edges). Everything above depends UP onto it — `hl-ws-term` implements its [`terminal::PtyBackend`] +
//! [`launch::Launcher`], and `hl` owns the richer config (the bare [`model::Workspace`]
//! PLUS feature settings) + its persistence and composes it all.
//!
//! It deliberately does NOT own: any feature setting (vpn/cuda/gui/docker_sock/scrollback — those are
//! `hl`-side config), the workspace-config PERSISTENCE (also `hl`-side), or any setting→engine-argument
//! mapping (that is `hl`'s job).
//!
//! - [`model`]    — `Arch`, `Mount`, and the bare `Workspace` run primitive
//! - [`terminal`] — the `PtyBackend` terminal-interface trait (implemented by `hl-ws-term`)
//! - [`launch`]   — the `Launcher` trait (implemented by `hl-ws-term` / `hl`)

pub mod launch;
pub mod model;
pub mod terminal;

pub use launch::Launcher;
pub use model::{Arch, Mount, Workspace};
pub use terminal::PtyBackend;
