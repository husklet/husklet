//! Per-protocol `Handler` impls for [`crate::HlState`], split one file per concern so parallel work
//! can own a module without colliding. The shared `HlState` struct stays in `crate` (lib.rs); each
//! submodule adds its trait impl(s) plus any `HlState` helper methods that belong to that protocol.
//!
//! Rust privacy note: these are descendant modules of the crate root, so they can read/write
//! `HlState`'s private fields (`start`, `last_cfg`, …) directly.
//!
//! The `delegate_*!` macros below live here (not in a submodule) because each expands to the full
//! `Dispatch`/`GlobalDispatch` impls for `HlState`, and keeping them together documents exactly which
//! globals the compositor dispatches — the Smithay-generated equivalent of server.rs's 4900 lines.

pub mod compositor;
pub mod dmabuf;
pub mod output;
pub mod scale;
pub mod seat;
pub mod text_input;
pub mod xdg;

// ---- Modern GUI protocol groups composed from the vendored Smithay tree (codex-rendering §5.2/§9.4) ----
// Each module supplies hl host policy (state + delegate below + handler/query methods) for a protocol
// the vendored `vendor/smithay-0.7.0` already implements but hl-compositor did not previously compose.
pub mod color;
pub mod content_type;
pub mod explicit_sync;
pub mod idle_inhibit;
pub mod keyboard_shortcuts_inhibit;
pub mod pointer_gestures;
pub mod input_routing;
pub mod tablet;
pub mod xdg_foreign;

// XWayland bridge (X11-only guest apps). Opt-in, OFF by default — see `Cargo.toml` and `xwayland.rs` for
// why it is not a declared cargo feature on the offline dev host (its `x11rb` deps are unfetchable).
#[cfg(feature = "xwayland")]
pub mod xwayland;

use crate::HlState;

use smithay::{
    delegate_compositor, delegate_content_type, delegate_cursor_shape, delegate_data_device,
    delegate_dmabuf, delegate_fractional_scale, delegate_idle_inhibit,
    delegate_keyboard_shortcuts_inhibit, delegate_output, delegate_pointer_constraints,
    delegate_pointer_gestures, delegate_presentation, delegate_primary_selection,
    delegate_relative_pointer, delegate_seat, delegate_shm, delegate_single_pixel_buffer,
    delegate_tablet_manager, delegate_viewporter, delegate_xdg_activation, delegate_xdg_decoration,
    delegate_xdg_foreign, delegate_xdg_shell,
};

delegate_compositor!(HlState); // wl_compositor + wl_subcompositor
delegate_shm!(HlState);
delegate_dmabuf!(HlState); // zwp_linux_dmabuf_v1 (GPU present path → dd IOSurface, zero-copy)
delegate_xdg_shell!(HlState);
delegate_xdg_decoration!(HlState); // zxdg_decoration_manager_v1 + zxdg_toplevel_decoration_v1 (SSD/CSD)
delegate_xdg_activation!(HlState); // xdg_activation_v1 + xdg_activation_token_v1 (focus/raise on request)
delegate_seat!(HlState);
delegate_output!(HlState); // wl_output + zxdg_output_manager_v1 (xdg-output: logical geometry/name/desc)
delegate_viewporter!(HlState);
delegate_fractional_scale!(HlState); // wp_fractional_scale_manager_v1 + wp_fractional_scale_v1 (non-integer HiDPI)
delegate_single_pixel_buffer!(HlState); // wp_single_pixel_buffer_v1 (1×1 solid-color buffer optimization)
delegate_presentation!(HlState);
delegate_cursor_shape!(HlState); // wp_cursor_shape_manager_v1 + wp_cursor_shape_device_v1
delegate_data_device!(HlState); // wl_data_device_manager + wl_data_device/source/offer (clipboard + DnD)
delegate_primary_selection!(HlState); // zwp_primary_selection_v1 (X11-style middle-click paste)
delegate_relative_pointer!(HlState); // zwp_relative_pointer_v1 (game/3D relative motion)
delegate_pointer_constraints!(HlState); // zwp_pointer_constraints_v1 (pointer lock/confine)

// ---- Modern GUI protocol groups composed from the vendored Smithay tree (codex-rendering §5.2/§9.4) ----
// Policy + state/query methods live in the same-named handlers::* submodules above. tearing-control
// (wp_tearing_control_manager_v1) is NOT in vendored smithay-0.7.0 and is therefore not composed here.
delegate_content_type!(HlState); // wp_content_type_manager_v1 + wp_content_type_v1 (photo/video/game hint)
delegate_idle_inhibit!(HlState); // zwp_idle_inhibit_manager_v1 + zwp_idle_inhibitor_v1 (keep session awake)
delegate_keyboard_shortcuts_inhibit!(HlState); // zwp_keyboard_shortcuts_inhibit_manager_v1 (forward all keys)
delegate_pointer_gestures!(HlState); // zwp_pointer_gestures_v1 (touchpad swipe/pinch/hold)
delegate_tablet_manager!(HlState); // zwp_tablet_manager_v2 (graphics tablet/stylus; TabletSeatHandler in seat.rs)
delegate_xdg_foreign!(HlState); // zxdg_exporter_v2 + zxdg_importer_v2 (cross-client toplevel parenting)
