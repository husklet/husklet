//! Multi-window + Chrome split-client input routing.
//!
//! ## The problem this solves (ported from the legacy `dd-display` `run_multi` / `route_input`)
//! The native present backend shows one host window (NSWindow) per presented surface, keyed by the
//! compositor's HOST surface id. Two things then need routing that a single "keyboard focus" cannot
//! express on its own:
//!   1. **Multi-window**: a click on window B must reach the surface B backs, not whatever last held
//!      focus. The presenter maps the clicked `NSWindow*` → host sid (`Presenter::window_ptr_to_sid`);
//!      [`DdState::route_window_input`] turns that host sid into a routing decision.
//!   2. **Chrome split-client**: Chrome's BROWSER connection owns the `wl_seat` + `xdg_toplevel` (input +
//!      geometry) while a SEPARATE gpu/shim connection commits the visible IOSurface. So the window the
//!      user clicks is owned by a connection that CANNOT consume input; the event must be FORWARDED to the
//!      browser connection, and the browser's window geometry mirrored onto the gpu connection so the
//!      IOSurface is cropped to the visible window region (`external_logical_crop`, applied at present).
//!
//! ## Architecture note (legacy vs Smithay)
//! The legacy path had one `Server` per Wayland connection and iterated `&mut [Server]`; dd-compositor has
//! ONE `DdState` with many Smithay clients sharing one seat/presenter. So "which connection can receive
//! input" becomes a per-CLIENT predicate ([`DdState::surface_can_receive_input`], keyed on
//! `seat_input_clients` — clients that have held keyboard focus, i.e. bound `wl_seat`), and the forward
//! target is the current keyboard-focused surface (unambiguous — there is exactly one focus).
//!
//! ## Validation
//! The routing DECISION + geometry-mirror STATE MACHINE are pure functions over `DdState` and are proven
//! by the in-process `input_routing_tests`. The AppKit wiring in `main.rs` (`window_ptr_to_sid` → route →
//! deliver) is macOS-only and needs a live multi-window app on the mac bridge to validate end-to-end.

use hl_display::present::SurfaceBuffer;
use smithay::wayland::compositor::with_states;
use smithay::wayland::shell::xdg::SurfaceCachedState;

use crate::DdState;

/// A temporary logical crop mirrored from the input (browser) connection onto the visible (gpu/shim)
/// connection so its IOSurface is cropped to the browser window's region at present time.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExternalLogicalCrop {
    /// The input surface (browser toplevel) this crop was mirrored from.
    pub source_sid: u32,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    /// Which fallback produced the geometry (diagnostics).
    pub source: &'static str,
}

/// Best-known logical geometry of an input surface — the browser toplevel in the split-client path.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FocusedGeometry {
    pub sid: u32,
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
    pub source: &'static str,
}

/// Where a windowed input event (identified by the host sid of the NSWindow it targeted) should go.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PointerRoute {
    /// Deliver to the clicked window's own surface (it can consume input). The live loop transfers focus
    /// to it first, so multi-window clicks land on the clicked window.
    Target { sid: u32 },
    /// The clicked window (gpu/shim) cannot consume input: forward the event to `target_sid` (the input-
    /// capable focused surface of a DIFFERENT client), using the clicked window `via_sid`'s on-screen
    /// size for the coordinate flip.
    Forward { target_sid: u32, via_sid: u32 },
    /// No input-capable target — drop the event.
    Drop,
}

/// Whether the split-client geometry mirror is enabled (`HL_DISPLAY_MIRROR_INPUT_GEOMETRY`), matching the
/// legacy env knob so the two paths gate identically.
pub fn mirror_input_geometry_enabled() -> bool {
    matches!(
        std::env::var("HL_DISPLAY_MIRROR_INPUT_GEOMETRY").as_deref(),
        Ok(v) if !v.is_empty() && v != "0"
    )
}

impl DdState {
    /// Whether the surface's owning CLIENT can consume input — i.e. it has bound the seat (held keyboard
    /// focus at least once). Chrome's browser connection returns true; its gpu/shim connection false.
    pub(crate) fn surface_can_receive_input(&self, sid: u32) -> bool {
        self.surface_owners
            .get(&sid)
            .is_some_and(|owner| self.seat_input_clients.contains(owner))
    }

    /// Route a windowed input event whose target NSWindow resolved to host `clicked_sid`.
    #[doc(hidden)]
    pub fn route_window_input(&self, clicked_sid: u32) -> PointerRoute {
        if self.surface_can_receive_input(clicked_sid) {
            return PointerRoute::Target { sid: clicked_sid };
        }
        // The clicked window's connection cannot consume input (the gpu/shim connection): forward to the
        // input-capable focused surface of a DIFFERENT client (the browser connection's toplevel).
        let clicked_owner = self.surface_owners.get(&clicked_sid);
        let focus_sid = self.focus.as_ref().and_then(|s| self.surface_id_opt(s));
        match focus_sid {
            Some(fsid)
                if self.surface_can_receive_input(fsid)
                    && self.surface_owners.get(&fsid) != clicked_owner =>
            {
                PointerRoute::Forward { target_sid: fsid, via_sid: clicked_sid }
            }
            _ => PointerRoute::Drop,
        }
    }

    /// Best-known logical geometry for input surface `sid`, mirroring the legacy fallback chain: the
    /// client's `xdg_surface.set_window_geometry`, else the presenter's live/committed window size, else
    /// the committed buffer's logical size.
    pub(crate) fn focused_logical_geometry(&self, sid: u32) -> Option<FocusedGeometry> {
        let surface = self.surface_resources.get(&sid)?;
        let geo = with_states(surface, |states| {
            states.cached_state.get::<SurfaceCachedState>().current().geometry
        });
        if let Some(r) = geo {
            if r.size.w > 0 && r.size.h > 0 {
                return Some(FocusedGeometry {
                    sid,
                    x: r.loc.x,
                    y: r.loc.y,
                    w: r.size.w,
                    h: r.size.h,
                    source: "xdg_window_geometry",
                });
            }
        }
        if let Some((w, h)) = self.presenter.window_content_size(sid) {
            if w > 0 && h > 0 {
                return Some(FocusedGeometry { sid, x: 0, y: 0, w, h, source: "presenter_content_size" });
            }
        }
        if let Some((w, h)) = self.presenter.surface_size(sid) {
            if w > 0 && h > 0 {
                return Some(FocusedGeometry { sid, x: 0, y: 0, w, h, source: "presenter_surface_size" });
            }
        }
        if let Some((w, h)) = self.surface_logical_size(surface) {
            if w > 0 && h > 0 {
                return Some(FocusedGeometry { sid, x: 0, y: 0, w, h, source: "buffer_logical_size" });
            }
        }
        None
    }

    /// The crop to mirror onto the VISIBLE (gpu/shim) surface `visible_sid` from the input (browser)
    /// connection: only when the mirror is enabled, the visible surface can't itself receive input, and
    /// there is an input-capable focused surface of a DIFFERENT client. `None` otherwise (no over-crop).
    pub(crate) fn mirrored_input_crop(&self, visible_sid: u32) -> Option<ExternalLogicalCrop> {
        if !mirror_input_geometry_enabled() || self.surface_can_receive_input(visible_sid) {
            return None;
        }
        let visible_owner = self.surface_owners.get(&visible_sid);
        let fsid = self.focus.as_ref().and_then(|s| self.surface_id_opt(s))?;
        if !self.surface_can_receive_input(fsid) || self.surface_owners.get(&fsid) == visible_owner {
            return None;
        }
        let geom = self.focused_logical_geometry(fsid)?;
        Some(ExternalLogicalCrop {
            source_sid: fsid,
            x: geom.x,
            y: geom.y,
            w: geom.w,
            h: geom.h,
            source: geom.source,
        })
    }

    /// Set (or clear) the server-wide external logical crop. The live loop refreshes it each present cycle
    /// via [`Self::refresh_input_geometry_mirror`]; `snapshot_surface` applies it to the visible surface.
    pub(crate) fn set_external_logical_crop(&mut self, crop: Option<ExternalLogicalCrop>) {
        self.external_logical_crop = crop;
    }

    /// Recompute + install the input-geometry mirror for the currently visible/clicked window `visible_sid`
    /// (the gpu/shim connection). Called from the live present loop; a no-op when the mirror is disabled or
    /// the surface can receive input itself.
    pub fn refresh_input_geometry_mirror(&mut self, visible_sid: u32) {
        let crop = self.mirrored_input_crop(visible_sid);
        self.set_external_logical_crop(crop);
    }

    /// Apply the external logical crop to a just-built presented [`SurfaceBuffer`] for the visible
    /// (gpu/shim) surface: narrow the presented logical size + backing sample rect to the mirrored browser
    /// window region so the IOSurface shows only the visible window, not the whole (possibly larger)
    /// backing target. No-op for the crop's own source surface, for input-capable surfaces, or when unset.
    pub(crate) fn apply_external_crop(&self, sb: &mut SurfaceBuffer, sid: u32) {
        let Some(crop) = self.external_logical_crop else {
            return;
        };
        if crop.source_sid == sid || self.surface_can_receive_input(sid) {
            return;
        }
        let (ow, oh) = (sb.width.max(1), sb.height.max(1));
        let x = crop.x.clamp(0, ow);
        let y = crop.y.clamp(0, oh);
        let w = crop.w.clamp(1, (ow - x).max(1));
        let h = crop.h.clamp(1, (oh - y).max(1));
        // Narrow the normalized backing sample rect to the crop sub-region of the surface's logical size.
        let [u0, v0, u1, v1] = sb.uv_rect;
        let (uw, uh) = (u1 - u0, v1 - v0);
        sb.uv_rect = [
            u0 + uw * (x as f32 / ow as f32),
            v0 + uh * (y as f32 / oh as f32),
            u0 + uw * ((x + w) as f32 / ow as f32),
            v0 + uh * ((y + h) as f32 / oh as f32),
        ];
        sb.width = w;
        sb.height = h;
    }
}
