//! Per-protocol `Handler` impls for [`crate::DdState`], split one file per concern so parallel work
//! can own a module without colliding. The shared `DdState` struct stays in `crate` (lib.rs); each
//! submodule adds its trait impl(s) plus any `DdState` helper methods that belong to that protocol.
//!
//! Rust privacy note: these are descendant modules of the crate root, so they can read/write
//! `DdState`'s private fields (`start`, `last_cfg`, …) directly.
//!
//! The `delegate_*!` macros below live here (not in a submodule) because each expands to the full
//! `Dispatch`/`GlobalDispatch` impls for `DdState`, and keeping them together documents exactly which
//! globals the compositor dispatches — the Smithay-generated equivalent of server.rs's 4900 lines.

pub mod compositor;
pub mod dmabuf;
pub mod output;
pub mod scale;
pub mod seat;
pub mod text_input;
pub mod xdg;

// ---- Modern GUI protocol groups composed from the vendored Smithay tree (codex-rendering §5.2/§9.4) ----
// Each module supplies dd host policy (state + delegate below + handler/query methods) for a protocol
// the vendored `third_party/smithay-0.7.0` already implements but dd-compositor did not previously compose.
pub mod content_type;
pub mod explicit_sync;
pub mod idle_inhibit;
pub mod keyboard_shortcuts_inhibit;
pub mod pointer_gestures;
pub mod tablet;
pub mod xdg_foreign;

use crate::DdState;

use smithay::{
    delegate_compositor, delegate_content_type, delegate_cursor_shape, delegate_data_device,
    delegate_dmabuf, delegate_fractional_scale, delegate_idle_inhibit,
    delegate_keyboard_shortcuts_inhibit, delegate_output, delegate_pointer_constraints,
    delegate_pointer_gestures, delegate_presentation, delegate_primary_selection,
    delegate_relative_pointer, delegate_seat, delegate_shm, delegate_single_pixel_buffer,
    delegate_tablet_manager, delegate_viewporter, delegate_xdg_activation, delegate_xdg_decoration,
    delegate_xdg_foreign, delegate_xdg_shell,
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

// ---- Modern GUI protocol groups composed from the vendored Smithay tree (codex-rendering §5.2/§9.4) ----
// Policy + state/query methods live in the same-named handlers::* submodules above. tearing-control
// (wp_tearing_control_manager_v1) is NOT in vendored smithay-0.7.0 and is therefore not composed here.
delegate_content_type!(DdState); // wp_content_type_manager_v1 + wp_content_type_v1 (photo/video/game hint)
delegate_idle_inhibit!(DdState); // zwp_idle_inhibit_manager_v1 + zwp_idle_inhibitor_v1 (keep session awake)
delegate_keyboard_shortcuts_inhibit!(DdState); // zwp_keyboard_shortcuts_inhibit_manager_v1 (forward all keys)
delegate_pointer_gestures!(DdState); // zwp_pointer_gestures_v1 (touchpad swipe/pinch/hold)
delegate_tablet_manager!(DdState); // zwp_tablet_manager_v2 (graphics tablet/stylus; TabletSeatHandler in seat.rs)
delegate_xdg_foreign!(DdState); // zxdg_exporter_v2 + zxdg_importer_v2 (cross-client toplevel parenting)
