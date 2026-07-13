//! `zxdg_exporter_v2` / `zxdg_importer_v2` (xdg-foreign) — lets one client export a toplevel and
//! another import that handle to set a cross-client parent relationship (file-chooser / portal dialogs
//! parented onto the app that spawned them; Flatpak/portal UIs rely on this). Composed from the vendored
//! Smithay `xdg_foreign` module.
//!
//! ## Host policy (issue real handles)
//! There is no host device to virtualize here — the whole protocol is compositor-internal bookkeeping,
//! and dd supplies exactly that: Smithay mints a real 32-char export handle, tracks the exported surface,
//! resolves `import_toplevel(handle)` back to it, and applies `set_parent_of` through the same
//! `XdgToplevelSurfaceData.parent` field the xdg-shell window model already honours. dd's contribution is
//! composing the state + delegate and exposing the state getter; the parent relationship then flows into
//! the existing focus/stacking policy. Requires [`smithay::wayland::shell::xdg::XdgShellHandler`], which
//! `handlers::xdg` already implements.

use smithay::wayland::xdg_foreign::{XdgForeignHandler, XdgForeignState};

use crate::DdState;

impl XdgForeignHandler for DdState {
    fn xdg_foreign_state(&mut self) -> &mut XdgForeignState {
        &mut self.xdg_foreign
    }
}
