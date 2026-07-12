//! Per-protocol `Handler` impls for [`crate::DdState`], split one file per concern so parallel work
//! can own a module without colliding. The shared `DdState` struct stays in `crate` (lib.rs); each
//! submodule adds its trait impl(s) plus any `DdState` helper methods that belong to that protocol.
//!
//! Rust privacy note: these are descendant modules of the crate root, so they can read/write
//! `DdState`'s private fields (`present_seq`, `start`, `last_cfg`, …) directly.
//!
//! The `delegate_*!` macros below live here (not in a submodule) because each expands to the full
//! `Dispatch`/`GlobalDispatch` impls for `DdState`, and keeping them together documents exactly which
//! globals the compositor dispatches — the Smithay-generated equivalent of server.rs's 4900 lines.

pub mod compositor;
pub mod dmabuf;
pub mod output;
pub mod seat;
pub mod xdg;

use crate::DdState;

use smithay::{
    delegate_compositor, delegate_cursor_shape, delegate_data_device, delegate_dmabuf,
    delegate_output, delegate_presentation, delegate_seat, delegate_shm, delegate_viewporter,
    delegate_xdg_shell,
};

delegate_compositor!(DdState); // wl_compositor + wl_subcompositor
delegate_shm!(DdState);
delegate_dmabuf!(DdState); // zwp_linux_dmabuf_v1 (GPU present path → dd IOSurface, zero-copy)
delegate_xdg_shell!(DdState);
delegate_seat!(DdState);
delegate_output!(DdState);
delegate_viewporter!(DdState);
delegate_presentation!(DdState);
delegate_cursor_shape!(DdState); // wp_cursor_shape_manager_v1 + wp_cursor_shape_device_v1
delegate_data_device!(DdState); // wl_data_device_manager + wl_data_device/source/offer (clipboard + DnD)
