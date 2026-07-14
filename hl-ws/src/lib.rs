//! hl-ws — the VERY THIN, shared workspace foundation for the `hl-ws-*` family.
//!
//! std-ONLY and the LEAF of its family: it holds only the bare-minimum types + trait seams common to all
//! consumers, and references NO other hl crate (verify with `cargo tree -p hl-ws`: zero `hl-*` edges).
//! Everything above depends UP onto it — `hl-ws-term` implements its [`terminal::PtyBackend`] +
//! [`launch::Launcher`], the GUI + feature settings live in `hl`/`hl-ws-gui`, and `hl` composes it all.
//!
//! It deliberately does NOT own: the generic settings-UI schema (that is shared between `hl` and
//! `hl-ws-gui`), any feature/plugin trait, or any setting→engine-argument mapping (that is `hl`'s job).
//!
//! - [`model`]    — `Arch`, `Mount`, the `Workspace` identity/model, and its plain settings data types
//!                  (`VpnConfig`/`VpnKind`, `CudaDevice`) that are fields of a workspace
//! - [`store`]    — `WorkspaceStore` persistence
//! - [`terminal`] — the `PtyBackend` terminal-interface trait (implemented by `hl-ws-term`)
//! - [`launch`]   — the `Launcher` trait (implemented by `hl-ws-term` / `hl`)

pub mod launch;
pub mod model;
pub mod store;
pub mod terminal;

pub use launch::Launcher;
pub use model::{Arch, CudaDevice, Mount, VpnConfig, VpnKind, Workspace};
pub use store::WorkspaceStore;
pub use terminal::PtyBackend;
