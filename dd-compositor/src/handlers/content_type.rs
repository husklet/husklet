//! `wp_content_type_manager_v1` — a surface tells the compositor what kind of content it is showing
//! (`none`/`photo`/`video`/`game`) so the compositor can tune presentation (tearing, scaling filter,
//! latency). Composed from the vendored Smithay `content_type` module.
//!
//! ## Host policy (store the hint)
//! dd presents through one Cocoa/Metal window and has no per-surface tearing/filter knob to flip today,
//! so the correct behaviour is to STORE the committed content type per surface (Smithay double-buffers
//! it in `ContentTypeSurfaceCachedState`; [`DdState::record_content_type`] snapshots the committed value
//! into `content_types` on every commit). [`DdState::content_type`] exposes it so the present path — and
//! a future tearing/latency policy — can read a surface's declared type. There is no server→client
//! event in this protocol; the roundtrip is bind → attach → set → commit, verified by the stored value.

use smithay::reexports::wayland_protocols::wp::content_type::v1::server::wp_content_type_v1::Type;
use smithay::reexports::wayland_server::protocol::wl_surface::WlSurface;
use smithay::wayland::compositor::with_states;
use smithay::wayland::content_type::ContentTypeSurfaceCachedState;

use crate::{surface_id, DdState};

/// Map Smithay's `wp_content_type_v1::Type` to its wire enum value (none=0, photo=1, video=2, game=3).
fn content_type_value(t: Type) -> u32 {
    match t {
        Type::None => 0,
        Type::Photo => 1,
        Type::Video => 2,
        Type::Game => 3,
        _ => 0,
    }
}

impl DdState {
    /// Snapshot a surface's just-committed `wp_content_type` into `content_types`. Called from the
    /// compositor commit path (Smithay has already applied the double-buffered pending→current state).
    /// A `none` type clears the entry so the map reflects only surfaces with a live declared type.
    pub(crate) fn record_content_type(&mut self, surface: &WlSurface) {
        let value = with_states(surface, |states| {
            let mut guard = states.cached_state.get::<ContentTypeSurfaceCachedState>();
            content_type_value(*guard.current().content_type())
        });
        let sid = surface_id(surface);
        if value == 0 {
            self.content_types.remove(&sid);
        } else {
            self.content_types.insert(sid, value);
        }
    }

    /// The declared `wp_content_type` for a surface (by surface id), or `None` if unset/`none`.
    pub fn content_type(&self, sid: u32) -> Option<u32> {
        self.content_types.get(&sid).copied()
    }
}
