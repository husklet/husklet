//! The pointer-driven `xdg_toplevel.resize` grab — interactive resize by dragging a client-side-decoration
//! edge.
//!
//! xdg-shell says the compositor answers `resize` by starting an interactive resize: while the drag is live
//! it sends `xdg_toplevel.configure`s carrying the `resizing` state and the size the pointer implies, and it
//! clears `resizing` when the drag ends. A client whose only resize affordance is a CSD edge (GTK4, Qt with
//! client decorations) cannot change its size any other way.
//!
//! The grab lives here rather than in a Smithay `PointerGrab` because the geometry is the neutral scene's:
//! every toplevel roots its own tree at `(0, 0)`, so a resize is a size delta computed from the pointer's
//! root-space travel, not a rectangle moved on a desktop plane.

use super::*;

/// A live interactive resize. `origin`/`start` are the pointer position and window size at the moment the
/// client's `resize` request was honoured, so every configure is computed from the ORIGINAL size — a
/// per-motion delta would accumulate rounding and clamp error.
pub(super) struct ResizeGrab {
    surface: SurfaceId,
    edges: ResizeEdge,
    origin: (f64, f64),
    start: (i32, i32),
    /// The size most recently CONFIGURED during the drag. Not readable back from the toplevel: its
    /// `current_state` only advances when the client acks, which lags the drag by at least a round trip, so
    /// the final configure would otherwise snap the window back to the pre-drag size.
    latest: (i32, i32),
}

impl HlState {
    /// Honour `xdg_toplevel.resize`: record the grab and send the first `resizing` configure.
    pub(super) fn begin_resize(&mut self, toplevel: &ToplevelSurface, edges: ResizeEdge) {
        let Some(surface) = self.sid(toplevel.wl_surface()) else {
            return;
        };
        if matches!(edges, ResizeEdge::None) {
            return;
        }
        // The size the drag grows from: what the client was last configured at, or — if the compositor left
        // the size to the client (`configure(0, 0)`) — the size it actually committed. Falling straight
        // through to the initial size would make the window JUMP on the first motion.
        let start = toplevel
            .current_state()
            .size
            .map(|size| (size.w, size.h))
            .filter(|&(w, h)| w > 0 && h > 0)
            .or_else(|| {
                self.engine
                    .scene
                    .get(surface)
                    .and_then(crate::scene::model::Surface::logical_size)
            })
            .unwrap_or(INITIAL_TOPLEVEL_SIZE);
        let origin = self.engine.scene.seat().pointer_location;
        hl_debug!(
            tag::WAYLAND,
            "resize begin surface={} edges={:?} from={}x{}",
            surface.0,
            edges,
            start.0,
            start.1
        );
        self.resize_grab = Some(ResizeGrab {
            surface,
            edges,
            origin,
            start,
            latest: start,
        });
        self.engine
            .presenter_mut()
            .begin_interaction(surface, WindowInteraction::Resize);
        self.send_resize_configure(surface, start, true);
    }

    /// Advance a live resize to the pointer at root-space `(x, y)`. Returns whether the motion was consumed
    /// by the grab — a grab owns the pointer, so the motion is NOT also delivered to the surface.
    pub(super) fn drive_resize(&mut self, x: f64, y: f64) -> bool {
        let Some(grab) = self.resize_grab.as_ref() else {
            return false;
        };
        let (dx, dy) = (x - grab.origin.0, y - grab.origin.1);
        let (w, h) = grab.start;
        let edges = grab.edges;
        let width = match edges {
            ResizeEdge::Left | ResizeEdge::TopLeft | ResizeEdge::BottomLeft => w - dx as i32,
            ResizeEdge::Right | ResizeEdge::TopRight | ResizeEdge::BottomRight => w + dx as i32,
            _ => w,
        };
        let height = match edges {
            ResizeEdge::Top | ResizeEdge::TopLeft | ResizeEdge::TopRight => h - dy as i32,
            ResizeEdge::Bottom | ResizeEdge::BottomLeft | ResizeEdge::BottomRight => h + dy as i32,
            _ => h,
        };
        let surface = grab.surface;
        let size = self.clamp_resize(surface, width, height);
        if let Some(grab) = self.resize_grab.as_mut() {
            grab.latest = size;
        }
        self.send_resize_configure(surface, size, true);
        true
    }

    /// End a live resize (the drag's button came up): a final configure without `resizing`. Returns whether
    /// a grab was active.
    pub(super) fn finish_resize(&mut self) -> bool {
        let Some(grab) = self.resize_grab.take() else {
            return false;
        };
        let size = grab.latest;
        hl_debug!(
            tag::WAYLAND,
            "resize end surface={} size={}x{}",
            grab.surface.0,
            size.0,
            size.1
        );
        self.send_resize_configure(grab.surface, size, false);
        true
    }

    /// Clamp a proposed size to at least 1×1 and to the client's own `set_min_size`/`set_max_size` — a
    /// configure outside them is a protocol violation the client is entitled to ignore.
    fn clamp_resize(&self, surface: SurfaceId, width: i32, height: i32) -> (i32, i32) {
        let Some(wl) = self.surfaces_by_id.get(&surface) else {
            return (width.max(1), height.max(1));
        };
        let (min, max) = with_states(wl, |states| {
            let mut cached = states.cached_state.get::<XdgSurfaceCachedState>();
            let current = cached.current();
            (current.min_size, current.max_size)
        });
        let axis = |value: i32, min: i32, max: i32| {
            let value = value.max(min.max(1));
            if max > 0 {
                value.min(max)
            } else {
                value
            }
        };
        (axis(width, min.w, max.w), axis(height, min.h, max.h))
    }

    /// Send the grabbed toplevel a configure at `size`, with the `resizing` state set or cleared. The
    /// `activated` state is preserved: a resize does not change which window has focus.
    fn send_resize_configure(&mut self, surface: SurfaceId, size: (i32, i32), resizing: bool) {
        let Some(toplevel) = self.toplevel_for(surface) else {
            return;
        };
        toplevel.with_pending_state(|state| {
            state.size = Some(size.into());
            if resizing {
                state.states.set(XdgToplevelState::Resizing);
            } else {
                state.states.unset(XdgToplevelState::Resizing);
            }
        });
        toplevel.send_configure();
    }

    /// The `ToplevelSurface` owning neutral surface `surface`, if it is still alive.
    pub(super) fn toplevel_for(&self, surface: SurfaceId) -> Option<ToplevelSurface> {
        self.xdg_shell
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| self.sid(toplevel.wl_surface()) == Some(surface))
            .cloned()
    }
}
