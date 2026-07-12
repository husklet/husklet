//! `xdg_shell` window management: the `xdg_surface` configure/ack handshake and the full `xdg_toplevel`
//! request surface (title/app_id, maximize/fullscreen/minimize, move/resize, min/max size, close).
//!
//! Smithay's `XdgShellState` owns the wire: it validates the ack-configure serial handshake, stores
//! `set_title`/`set_app_id`/`set_min_size`/`set_max_size`/window-geometry in the toplevel's role
//! attributes, and calls one handler method per request. Our job is compositor POLICY — which state to
//! set in the pending `ToplevelConfigure` and what on-screen size to hint — plus wiring `move` and the
//! host-window-resize reflow into the reused [`Presenter`] seam (mirroring `server.rs`'s `maybe_resize`
//! / `resize_focused` / `begin_interactive_move`).
//!
//! Configure/ack handshake: each `with_pending_state(..)` + `send_configure()` allocates a fresh serial
//! and emits `xdg_toplevel.configure(w,h,states)` paired with `xdg_surface.configure(serial)`. Smithay
//! only delivers the buffer as "acked" once the client echoes that serial via `ack_configure`, and
//! `ToplevelSurface::current_state()` reflects the last acked configure.

use smithay::{
    reexports::{
        wayland_protocols::xdg::shell::server::xdg_toplevel::{self, State as XdgState},
        wayland_server::protocol::{wl_output::WlOutput, wl_seat::WlSeat, wl_surface::WlSurface},
    },
    utils::Serial,
    wayland::{
        compositor::with_states,
        shell::xdg::{
            Configure, PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface,
            XdgShellHandler, XdgShellState, XdgToplevelSurfaceData,
        },
    },
};

use crate::{surface_id, DdState, INITIAL_TOPLEVEL_SIZE};

impl XdgShellHandler for DdState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell
    }

    /// A toplevel mapped. Send the initial configure carrying a floating size + `Activated` + the
    /// `configure_bounds` (output logical size) so the client draws its first frame, then take focus.
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        let bounds = self.output_logical_size();
        surface.with_pending_state(|s| {
            s.size = Some(INITIAL_TOPLEVEL_SIZE.into());
            s.bounds = Some(bounds.into());
            s.states.set(XdgState::Activated);
        });
        surface.send_configure();
        self.focus_surface(surface.wl_surface().clone());
    }

    /// `set_title`: Smithay has already stored it in the role attributes; mirror it into `titles` so the
    /// Presenter can label the NSWindow (server.rs kept the same surface→title map).
    fn title_changed(&mut self, surface: ToplevelSurface) {
        let sid = surface_id(surface.wl_surface());
        let title = toplevel_title(&surface).unwrap_or_default();
        self.titles.insert(sid, title);
    }

    /// `set_app_id`: accepted; no host-side effect (the NSWindow is labelled by title). Present so the
    /// handler is explicit rather than an implicit default.
    fn app_id_changed(&mut self, _surface: ToplevelSurface) {}

    /// `xdg_toplevel.move(seat, serial)`: the client asks the compositor to start a user-driven window
    /// drag. Turn it into a HOST NSWindow drag via the Presenter — invoked ONLY on an explicit request
    /// (the request-gated alternative to blanket movable-by-background).
    fn move_request(&mut self, surface: ToplevelSurface, _seat: WlSeat, _serial: Serial) {
        self.presenter
            .begin_interactive_move(surface_id(surface.wl_surface()));
    }

    /// `xdg_toplevel.resize(seat, serial, edges)`: enter an interactive resize. Mark the toplevel
    /// `Resizing` (+ `Activated`) and re-send its current size so the client knows a resize is in
    /// progress; subsequent host window-edge drags reflow it via [`DdState::maybe_resize_focused`], and
    /// [`DdState::end_interactive_resize`] clears the state when the drag ends.
    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: WlSeat,
        _serial: Serial,
        _edges: xdg_toplevel::ResizeEdge,
    ) {
        let (w, h) = self.toplevel_hint_size(&surface);
        surface.with_pending_state(|s| {
            s.size = Some((w, h).into());
            s.states.set(XdgState::Resizing);
            s.states.set(XdgState::Activated);
        });
        surface.send_configure();
    }

    /// `set_maximized`: MUST answer with a configure carrying `Maximized` so the client repaints at the
    /// maximized (output-logical) size.
    fn maximize_request(&mut self, surface: ToplevelSurface) {
        let size = self.output_logical_size();
        surface.with_pending_state(|s| {
            s.size = Some(size.into());
            s.states.set(XdgState::Maximized);
            s.states.set(XdgState::Activated);
        });
        surface.send_configure();
    }

    /// `unset_maximized` → back to floating at the last known on-screen size.
    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        let (w, h) = self.toplevel_hint_size(&surface);
        surface.with_pending_state(|s| {
            s.states.unset(XdgState::Maximized);
            s.size = Some((w, h).into());
            s.states.set(XdgState::Activated);
        });
        surface.send_configure();
    }

    /// `set_fullscreen(output)`: configure to the output size with `Fullscreen` set.
    fn fullscreen_request(&mut self, surface: ToplevelSurface, _output: Option<WlOutput>) {
        let size = self.output_logical_size();
        surface.with_pending_state(|s| {
            s.size = Some(size.into());
            s.states.set(XdgState::Fullscreen);
            s.states.set(XdgState::Activated);
        });
        surface.send_configure();
    }

    /// `unset_fullscreen` → back to floating.
    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        let (w, h) = self.toplevel_hint_size(&surface);
        surface.with_pending_state(|s| {
            s.states.unset(XdgState::Fullscreen);
            s.size = Some((w, h).into());
            s.states.set(XdgState::Activated);
        });
        surface.send_configure();
    }

    /// `set_minimized`: no configure is owed (the surface simply stops being shown). We have no
    /// off-screen window model, so this is accepted as a no-op, matching server.rs.
    fn minimize_request(&mut self, _surface: ToplevelSurface) {}

    /// A client acknowledged a configure serial. Smithay has already validated the serial and advanced
    /// `current_state()`; there is no extra bookkeeping the present path needs (commit presents the
    /// acked buffer directly), so this is intentionally empty.
    fn ack_configure(&mut self, _surface: WlSurface, _configure: Configure) {}

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let sid = surface_id(surface.wl_surface());
        self.titles.remove(&sid);
        self.presenter.drop_window(sid);
        if self.focus.as_ref() == Some(surface.wl_surface()) {
            self.focus = None;
            self.last_cfg = None;
        }
    }

    fn new_popup(&mut self, _surface: PopupSurface, _positioner: PositionerState) {}
    fn grab(&mut self, _surface: PopupSurface, _seat: WlSeat, _serial: Serial) {}
    fn reposition_request(
        &mut self,
        _surface: PopupSurface,
        _positioner: PositionerState,
        _token: u32,
    ) {
    }
}

impl DdState {
    /// The host window backing the focused toplevel was resized by the user: reconfigure the focused
    /// toplevel to the live content size so the client repaints at the new size. Debounced — the first
    /// observation is a baseline (no send); a later CHANGE emits exactly one configure (mirrors
    /// `server.rs`'s `maybe_resize`). The macOS present loop calls this each iteration.
    pub fn maybe_resize_focused(&mut self) {
        let Some(focus) = self.focus.clone() else {
            return;
        };
        let sid = surface_id(&focus);
        let Some((w, h)) = self.presenter.window_content_size(sid) else {
            return;
        };
        if w <= 0 || h <= 0 {
            return;
        }
        match self.last_cfg {
            None => self.last_cfg = Some((w, h)),
            Some(prev) if prev == (w, h) => {}
            Some(_) => {
                self.last_cfg = Some((w, h));
                self.resize_focused(w, h);
            }
        }
    }

    /// Send `xdg_toplevel.configure(w,h,[Activated])` + the paired `xdg_surface.configure(serial)` to
    /// the focused toplevel, clamped to its `set_min_size`/`set_max_size`. The client acks and commits a
    /// resized buffer. Mirrors `server.rs`'s `resize_focused`.
    pub fn resize_focused(&mut self, w: i32, h: i32) {
        let Some(focus) = self.focus.clone() else {
            return;
        };
        let Some(toplevel) = self.toplevel_for_surface(&focus) else {
            return;
        };
        let (min, max) = toplevel_min_max(&toplevel);
        let w = clamp_axis(w, min.0, max.0);
        let h = clamp_axis(h, min.1, max.1);
        toplevel.with_pending_state(|s| {
            s.size = Some((w, h).into());
            s.states.set(XdgState::Activated);
        });
        toplevel.send_configure();
    }

    /// End an interactive resize: clear the `Resizing` state on the focused toplevel and re-configure at
    /// its current size so the client leaves resize mode. The platform loop calls this on pointer-up.
    pub fn end_interactive_resize(&mut self) {
        let Some(focus) = self.focus.clone() else {
            return;
        };
        let Some(toplevel) = self.toplevel_for_surface(&focus) else {
            return;
        };
        if !toplevel.current_state().states.contains(XdgState::Resizing) {
            return;
        }
        toplevel.with_pending_state(|s| {
            s.states.unset(XdgState::Resizing);
            s.states.set(XdgState::Activated);
        });
        toplevel.send_configure();
    }

    /// Ask the client owning `surface` to close (`xdg_toplevel.close`). The host window-manager close
    /// button drives this; the client tears its toplevel down in response. Mirrors `server.rs`'s
    /// `request_close`.
    pub fn request_close(&mut self, surface: &WlSurface) {
        if let Some(toplevel) = self.toplevel_for_surface(surface) {
            toplevel.send_close();
        }
    }

    /// Ask the currently focused toplevel to close.
    pub fn request_close_focused(&mut self) {
        if let Some(focus) = self.focus.clone() {
            self.request_close(&focus);
        }
    }

    /// The `ToplevelSurface` whose backing `wl_surface` is `surface`, if any.
    fn toplevel_for_surface(&self, surface: &WlSurface) -> Option<ToplevelSurface> {
        self.xdg_shell
            .toplevel_surfaces()
            .iter()
            .find(|t| t.wl_surface() == surface)
            .cloned()
    }

    /// Size hint `(w,h)` to put in a state-change configure: the live window content size if the
    /// Presenter knows it, else the toplevel's last acked size, else the floating default. This keeps a
    /// maximize→unmaximize round-trip from collapsing the window to `(0,0)`.
    fn toplevel_hint_size(&self, surface: &ToplevelSurface) -> (i32, i32) {
        let sid = surface_id(surface.wl_surface());
        if let Some((w, h)) = self.presenter.window_content_size(sid) {
            if w > 0 && h > 0 {
                return (w, h);
            }
        }
        match surface.current_state().size {
            Some(sz) if sz.w > 0 && sz.h > 0 => (sz.w, sz.h),
            _ => INITIAL_TOPLEVEL_SIZE,
        }
    }
}

/// Read the toplevel's `set_title` from its role attributes (Smithay stored it there when it decoded the
/// request). `None` if the surface has no toplevel role or no title was set.
fn toplevel_title(surface: &ToplevelSurface) -> Option<String> {
    with_states(surface.wl_surface(), |states| {
        states
            .data_map
            .get::<XdgToplevelSurfaceData>()
            .and_then(|d| d.lock().unwrap().title.clone())
    })
}

/// Read the toplevel's `set_min_size` / `set_max_size` (each `0` on an axis ⇒ unbounded). Smithay stores
/// them double-buffered in `SurfaceCachedState`, applied on commit.
fn toplevel_min_max(surface: &ToplevelSurface) -> ((i32, i32), (i32, i32)) {
    with_states(surface.wl_surface(), |states| {
        let mut cached = states.cached_state.get::<SurfaceCachedState>();
        let cur = cached.current();
        (
            (cur.min_size.w, cur.min_size.h),
            (cur.max_size.w, cur.max_size.h),
        )
    })
}

/// Clamp one axis to `[min, max]`, treating `0` as "unbounded" on that end (the xdg_toplevel convention
/// for `set_min_size` / `set_max_size`). Mirrors `server.rs`'s `clamp_axis`.
fn clamp_axis(v: i32, min: i32, max: i32) -> i32 {
    let mut v = v;
    if max > 0 {
        v = v.min(max);
    }
    if min > 0 {
        v = v.max(min);
    }
    v.max(1)
}
