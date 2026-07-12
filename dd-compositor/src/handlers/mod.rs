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
pub mod scale;
pub mod seat;
pub mod xdg;

use crate::DdState;

use smithay::{
    delegate_compositor, delegate_cursor_shape, delegate_data_device, delegate_dmabuf,
    delegate_fractional_scale, delegate_output, delegate_pointer_constraints,
    delegate_presentation, delegate_primary_selection, delegate_relative_pointer, delegate_seat,
    delegate_shm, delegate_single_pixel_buffer, delegate_viewporter, delegate_xdg_activation,
    delegate_xdg_decoration, delegate_xdg_shell,
};

delegate_compositor!(DdState); // wl_compositor + wl_subcompositor
delegate_shm!(DdState);
delegate_dmabuf!(DdState); // zwp_linux_dmabuf_v1 (GPU present path → dd IOSurface, zero-copy)
delegate_xdg_shell!(DdState);
delegate_xdg_decoration!(DdState); // zxdg_decoration_manager_v1 + zxdg_toplevel_decoration_v1 (SSD/CSD)
delegate_xdg_activation!(DdState); // xdg_activation_v1 + xdg_activation_token_v1 (focus/raise on request)
delegate_seat!(DdState);
delegate_output!(DdState); // wl_output + zxdg_output_manager_v1 (xdg-output: logical geometry/name/desc)
delegate_viewporter!(DdState);
delegate_fractional_scale!(DdState); // wp_fractional_scale_manager_v1 + wp_fractional_scale_v1 (non-integer HiDPI)
delegate_single_pixel_buffer!(DdState); // wp_single_pixel_buffer_v1 (1×1 solid-color buffer optimization)
delegate_presentation!(DdState);
delegate_cursor_shape!(DdState); // wp_cursor_shape_manager_v1 + wp_cursor_shape_device_v1
delegate_data_device!(DdState); // wl_data_device_manager + wl_data_device/source/offer (clipboard + DnD)
delegate_primary_selection!(DdState); // zwp_primary_selection_v1 (X11-style middle-click paste)
delegate_relative_pointer!(DdState); // zwp_relative_pointer_v1 (game/3D relative motion)
delegate_pointer_constraints!(DdState); // zwp_pointer_constraints_v1 (pointer lock/confine)
