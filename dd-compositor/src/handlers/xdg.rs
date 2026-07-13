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
        wayland_protocols::xdg::{
            decoration::zv1::server::zxdg_toplevel_decoration_v1::Mode as DecorationMode,
            shell::server::xdg_toplevel::{self, State as XdgState},
        },
        wayland_server::protocol::{wl_output::WlOutput, wl_seat::WlSeat, wl_surface::WlSurface},
    },
    utils::{Logical, Point, Rectangle, Serial, Size},
    wayland::{
        compositor::with_states,
        shell::xdg::{
            decoration::XdgDecorationHandler,
            Configure, PopupSurface, PositionerState, SurfaceCachedState, ToplevelSurface,
            XdgPopupSurfaceData, XdgShellHandler, XdgShellState, XdgToplevelSurfaceData,
        },
        xdg_activation::{
            XdgActivationHandler, XdgActivationState, XdgActivationToken, XdgActivationTokenData,
        },
    },
};

use crate::{DdState, INITIAL_TOPLEVEL_SIZE};

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
        let sid = self.surface_id(surface.wl_surface());
        let title = toplevel_title(&surface).unwrap_or_default();
        self.titles.insert(sid, title);
    }

    /// `set_app_id`: accepted; no host-side effect (the NSWindow is labelled by title). Present so the
    /// handler is explicit rather than an implicit default.
    fn app_id_changed(&mut self, _surface: ToplevelSurface) {}

    /// `xdg_toplevel.move(seat, serial)`: the client asks the compositor to start a user-driven window
    /// drag. Validate `serial` against a recent input event (the implicit pointer-button grab that begins
    /// the drag) so a window can't move itself without a real gesture, then turn it into a HOST NSWindow
    /// drag via the Presenter — the request-gated alternative to blanket movable-by-background.
    fn move_request(&mut self, surface: ToplevelSurface, _seat: WlSeat, serial: Serial) {
        if !self.is_recent_input_serial(serial) {
            return; // stale/spoofed serial: ignore (the xdg-shell "may be ignored" path).
        }
        self.presenter
            .begin_interactive_move(self.surface_id(surface.wl_surface()));
    }

    /// `xdg_toplevel.resize(seat, serial, edges)`: begin an interactive edge/corner resize. Validate the
    /// grab serial, mark the toplevel `Resizing` (+ `Activated`) and configure so the client knows a resize
    /// is in progress, then drive a HOST NSWindow resize anchored on the grabbed edge via the Presenter
    /// ([`Presenter::begin_interactive_resize`], which blocks for the gesture on the windowed backend). When
    /// the gesture ends, [`DdState::finish_interactive_resize`] clears `Resizing` and reconfigures at the
    /// final on-screen size (the platform loop's [`DdState::maybe_resize_focused`] also reflows live-edge
    /// drags that go through the native title bar instead).
    fn resize_request(
        &mut self,
        surface: ToplevelSurface,
        _seat: WlSeat,
        serial: Serial,
        edges: xdg_toplevel::ResizeEdge,
    ) {
        if !self.is_recent_input_serial(serial) {
            return; // stale/spoofed serial: ignore.
        }
        let (w, h) = self.toplevel_hint_size(&surface);
        surface.with_pending_state(|s| {
            s.size = Some((w, h).into());
            s.states.set(XdgState::Resizing);
            s.states.set(XdgState::Activated);
        });
        surface.send_configure();
        // Run the host-side resize (blocks for the drag on Cocoa; a no-op on the headless presenter), then
        // leave resize mode at the final size. `edges` IS the resize_edge bitmask (top=1/bottom=2/left=4/
        // right=8, corner = OR of two), so the Presenter can anchor the opposite edge.
        self.presenter
            .begin_interactive_resize(self.surface_id(surface.wl_surface()), u32::from(edges));
        self.finish_interactive_resize(&surface);
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
    fn fullscreen_request(&mut self, surface: ToplevelSurface, output: Option<WlOutput>) {
        if let Some(resource) = output {
            if let Some(target) = std::iter::once(&self.output)
                .chain(self.extra_outputs.iter())
                .find(|candidate| candidate.owns(&resource))
                .cloned()
            {
                let sid = self.surface_id(surface.wl_surface());
                self.route_surface_to_output(sid, &target.name());
            }
        }
        let selected = self.selected_output(surface.wl_surface());
        let scale = selected.current_scale().integer_scale().max(1);
        let size = selected.current_mode().map(|mode| (mode.size.w / scale, mode.size.h / scale)).unwrap_or_else(|| self.output_logical_size());
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

    /// `set_minimized`: no configure is owed. Stop native presentation and suppress focus/input until
    /// the host restores the window through `set_surface_visibility`.
    fn minimize_request(&mut self, surface: ToplevelSurface) {
        self.set_surface_visibility(
            surface.wl_surface(),
            dd_display::present::SurfaceVisibility::Minimized,
        );
    }

    /// A client acknowledged a configure serial. Smithay has already validated the serial and advanced
    /// `current_state()`; there is no extra bookkeeping the present path needs (commit presents the
    /// acked buffer directly), so this is intentionally empty.
    fn ack_configure(&mut self, _surface: WlSurface, _configure: Configure) {}

    fn toplevel_destroyed(&mut self, surface: ToplevelSurface) {
        let sid = self.surface_id(surface.wl_surface());
        self.titles.remove(&sid);
        self.drop_surface_window(sid);
        if self.focus.as_ref() == Some(surface.wl_surface()) {
            self.focus = None;
            self.last_cfg = None;
            // Text-input focus tracks keyboard focus: the focused text field is gone, so send `leave`.
            self.set_text_input_focus(None);
        }
    }

    /// A popup (menu / dropdown / combobox list / tooltip / context menu) was created via
    /// `xdg_surface.get_popup(parent, positioner)`. Resolve the positioner to a concrete geometry
    /// (anchor rect → anchor point → gravity → offset, then flip/slide/resize constraint adjustment
    /// against the output area) and complete the initial handshake: `xdg_popup.configure(x,y,w,h)`
    /// paired with `xdg_surface.configure(serial)`. Without this the client never maps the popup, so
    /// Chrome's menus/dropdowns/tooltips stay invisible or paint stale. Nested popups (submenu chains)
    /// take exactly this same path — each is its own `xdg_popup` whose positioner anchor rect is
    /// relative to its PARENT popup's window geometry, so nothing here is special-cased for depth.
    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        let geometry = self.constrain_popup(&positioner);
        surface.with_pending_state(|state| {
            state.positioner = positioner;
            state.geometry = geometry;
        });
        // The initial configure MUST be sent before the client attaches a buffer (xdg-shell mandates the
        // configure/ack handshake); Smithay allocates the serial and pairs the xdg_surface.configure.
        let _ = surface.send_configure();
    }

    /// `xdg_popup.grab(seat, serial)`: the client takes an explicit popup grab (Chrome does this for menus
    /// and context menus, but NOT for tooltips). Record the grab so that a click outside the popup chain
    /// dismisses the whole chain via [`DdState::dismiss_popup_grabs`] — the input path calls that. The
    /// grab stack is ordered outer→inner, so a submenu opened under an existing grab extends the chain.
    fn grab(&mut self, surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        if !self
            .popup_grabs
            .iter()
            .any(|p| p.wl_surface() == surface.wl_surface())
        {
            self.popup_grabs.push(surface);
        }
    }

    /// `xdg_popup.reposition(positioner, token)` (xdg-shell v3): the client wants the already-mapped popup
    /// moved to a new placement (e.g. a menu re-anchoring as the pointer walks a menu bar). Recompute the
    /// geometry from the NEW positioner and answer with `xdg_popup.repositioned(token)` + a fresh
    /// configure/ack, so the client repaints at the new location. The `token` round-trips so the client can
    /// correlate the reposition it asked for.
    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        let geometry = self.constrain_popup(&positioner);
        surface.with_pending_state(|state| {
            state.positioner = positioner;
            state.geometry = geometry;
        });
        // Answer with xdg_popup.repositioned(token) + a fresh configure/ack. The popup's *current*
        // geometry only advances once the client acks and re-commits, so the re-present happens on that
        // commit (via on_commit), not here — presenting now would draw the popup at its old position.
        surface.send_repositioned(token);
    }

    /// A popup was destroyed (the client tore the menu/tooltip down, or a grab dismissal was honoured).
    /// Forget its buffer + grab bookkeeping, drop any native window the presenter opened for it, and
    /// re-present the owning toplevel so the menu visibly disappears.
    fn popup_destroyed(&mut self, surface: PopupSurface) {
        let sid = self.surface_id(surface.wl_surface());
        self.buffers.remove(&sid);
        self.drop_surface_window(sid);
        self.popup_grabs
            .retain(|p| p.wl_surface() != surface.wl_surface());
        if let Some(root) = self.window_root(surface.wl_surface()) {
            self.present_render_root(&root);
        }
    }
}

// ---- zxdg_decoration_manager_v1: server-side vs client-side decoration negotiation -------------------
//
// Toolkits split on decorations: Chrome and GTK draw their own client-side decorations (CSD — the app
// paints its title bar into the surface), while many Qt/GTK apps ask the compositor for server-side
// decorations (SSD). Our host window is borderless by default (the guest's CSD fills it), and opts into a
// native macOS title bar only when DD_DISPLAY_WINDOW_DECORATIONS is set (the `present_cocoa` seam). So our
// policy honours the client's requested mode WHERE THE HOST WINDOW CAN RENDER IT: CSD is always granted
// (the borderless window shows the client's own decorations); SSD is granted only when the native title
// bar exists, otherwise we answer CSD (a truthful mode the client can actually draw) rather than promising
// server decorations we won't paint. This is exactly what a client needs to avoid a double title bar.
impl XdgDecorationHandler for DdState {
    /// The client created a decoration object without stating a preference yet: answer with our default
    /// (SSD when the native title bar is enabled, else CSD).
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        self.configure_decoration(&toplevel, None);
    }

    /// The client asked for a specific mode (`set_mode`): honour it within what the host window can render.
    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        self.configure_decoration(&toplevel, Some(mode));
    }

    /// The client dropped its preference (`unset_mode`): fall back to our default.
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        self.configure_decoration(&toplevel, None);
    }
}

// ---- xdg_activation_v1: focus/raise on request ------------------------------------------------------
//
// A client presents an activation token (minted by another surface, carrying the input serial + seat that
// justified it) and asks for a toplevel to be activated. Compositors may refuse tokens without a recent
// serial to defeat focus-stealing; our single-window-per-surface host has no cross-app focus-steal risk and
// no notion of "deny focus", so we honour every activation by focusing + raising the target window — which
// is what a launcher-spawned window, or an app raising its own window, expects.
impl XdgActivationHandler for DdState {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self.xdg_activation
    }

    fn request_activation(
        &mut self,
        _token: XdgActivationToken,
        _token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        self.activate_surface(surface);
    }
}

impl DdState {
    /// Resolve `requested` against what the host window can actually render and send the decoration
    /// `configure(mode)` (Smithay emits it from `send_configure` when `decoration_mode` changes). See the
    /// policy note above the [`XdgDecorationHandler`] impl.
    fn configure_decoration(&mut self, toplevel: &ToplevelSurface, requested: Option<DecorationMode>) {
        let ssd_available = std::env::var_os("DD_DISPLAY_WINDOW_DECORATIONS").is_some();
        let mode = match requested {
            Some(DecorationMode::ClientSide) => DecorationMode::ClientSide,
            Some(DecorationMode::ServerSide) if ssd_available => DecorationMode::ServerSide,
            // SSD requested but no native title bar to draw, or no explicit preference: use our default.
            _ if ssd_available => DecorationMode::ServerSide,
            _ => DecorationMode::ClientSide,
        };
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(mode);
        });
        toplevel.send_configure();
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
        let sid = self.surface_id(&focus);
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

    /// Finish a client-initiated interactive resize (the counterpart to [`Self::resize_request`]): clear
    /// `Resizing` and reconfigure `toplevel` at the final on-screen content size, clamped to its
    /// `set_min_size`/`set_max_size`. Because [`Presenter::begin_interactive_resize`] owns the pointer for
    /// the whole drag on the windowed backend (the platform loop is paused, so `maybe_resize_focused` does
    /// not run mid-gesture), this is where the guest is told the window's new size once the drag ends.
    pub(crate) fn finish_interactive_resize(&mut self, toplevel: &ToplevelSurface) {
        let (w, h) = self.toplevel_hint_size(toplevel);
        let (min, max) = toplevel_min_max(toplevel);
        let w = clamp_axis(w, min.0, max.0);
        let h = clamp_axis(h, min.1, max.1);
        toplevel.with_pending_state(|s| {
            s.states.unset(XdgState::Resizing);
            s.states.set(XdgState::Activated);
            s.size = Some((w, h).into());
        });
        toplevel.send_configure();
        // Adopt the size so the loop's debounced `maybe_resize_focused` doesn't emit a duplicate configure.
        self.last_cfg = Some((w, h));
    }

    /// Focus and raise the toplevel backing `surface` — the effect of an `xdg_activation_v1` activation
    /// request (or any compositor-driven "bring this window to the front"). Raises the host NSWindow via the
    /// Presenter, takes keyboard focus (which also moves the clipboard/data-device focus), and re-sends an
    /// `Activated` configure so the client repaints as focused.
    pub(crate) fn activate_surface(&mut self, surface: WlSurface) {
        self.presenter.raise_window(self.surface_id(&surface));
        if let Some(toplevel) = self.toplevel_for_surface(&surface) {
            toplevel.with_pending_state(|s| {
                s.states.set(XdgState::Activated);
            });
            toplevel.send_configure();
        }
        self.focus_surface(surface);
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

    /// Resolve an `xdg_positioner` to the popup's on-screen geometry (relative to its parent's window
    /// geometry). Smithay's [`PositionerState::get_geometry`] applies the anchor/gravity/offset placement;
    /// [`PositionerState::get_unconstrained_geometry`] additionally honours `constraint_adjustment`
    /// (flip → slide → resize) so a menu anchored near the output edge flips/slides back on-screen instead
    /// of being clipped. The constraint target is the output logical area (the parent typically fills or
    /// nearly fills it), which is the common Chrome case.
    pub(crate) fn constrain_popup(&self, positioner: &PositionerState) -> Rectangle<i32, Logical> {
        let (ow, oh) = self.output_logical_size();
        let target = Rectangle::new(Point::from((0, 0)), Size::from((ow.max(1), oh.max(1))));
        // PositionerState is Copy; get_unconstrained_geometry consumes it.
        (*positioner).get_unconstrained_geometry(target)
    }

    /// Dismiss the whole active popup grab chain: send `xdg_popup.popup_done` from the innermost popup
    /// outward (so a submenu closes before its parent menu), then clear the grab stack. The macOS input
    /// loop calls this when a click lands outside the grabbing popup chain (the click-outside-dismisses
    /// semantics of an explicit popup grab). Returns how many popups were dismissed.
    pub fn dismiss_popup_grabs(&mut self) -> usize {
        let chain: Vec<PopupSurface> = std::mem::take(&mut self.popup_grabs);
        let n = chain.len();
        for popup in chain.into_iter().rev() {
            popup.send_popup_done();
        }
        n
    }

    /// The popup's parent `wl_surface`, if `surface` carries the `xdg_popup` role. The parent is another
    /// popup (a submenu chain) or the owning toplevel.
    pub(crate) fn popup_parent(&self, surface: &WlSurface) -> Option<WlSurface> {
        with_states(surface, |states| {
            states
                .data_map
                .get::<XdgPopupSurfaceData>()
                .and_then(|d| d.lock().unwrap().parent.clone())
        })
    }

    /// The popup's current (last-committed) geometry `(x, y, w, h)`, relative to its parent's window
    /// geometry origin — the placement its positioner resolved to. `None` if `surface` is not a popup.
    pub(crate) fn popup_geometry(&self, surface: &WlSurface) -> Option<(i32, i32, i32, i32)> {
        with_states(surface, |states| {
            states.data_map.get::<XdgPopupSurfaceData>().map(|d| {
                let g = d.lock().unwrap().current.geometry;
                (g.loc.x, g.loc.y, g.size.w, g.size.h)
            })
        })
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
        let sid = self.surface_id(surface.wl_surface());
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
