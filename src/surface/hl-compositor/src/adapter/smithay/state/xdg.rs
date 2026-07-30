use super::*;

/// Server-side handling of data-device state and XDG shell windows.
impl DataDeviceHandler for HlState {
    fn data_device_state(&self) -> &DataDeviceState {
        &self.data_device
    }
}

impl XdgShellHandler for HlState {
    fn xdg_shell_state(&mut self) -> &mut XdgShellState {
        &mut self.xdg_shell
    }

    /// A toplevel was created: assign the scene `Toplevel` role and send the initial configure with a
    /// floating size + output bounds. The new window is the pending activation target; when its first
    /// buffer maps, `set_keyboard_focus` completes the transition and deactivates the previous toplevel.
    fn new_toplevel(&mut self, surface: ToplevelSurface) {
        if let Some(sid) = self.sid(surface.wl_surface()) {
            self.engine.scene.set_role(sid, SurfaceRole::Toplevel);
            self.reconcile_window(sid);
        }
        let bounds = self.engine.scene.output_logical_size();
        surface.with_pending_state(|s| {
            s.size = Some(INITIAL_TOPLEVEL_SIZE.into());
            // `configure_bounds` (xdg-shell v4+): the largest a client should size itself to. Toolkits
            // clamp their preferred size to this; without it a client can pick a size larger than the
            // output. Sourced from the scene's primary output logical size, same as maximize.
            s.bounds = Some(bounds.into());
            s.states.set(XdgToplevelState::Activated);
        });
        surface.send_configure();
    }

    fn title_changed(&mut self, surface: ToplevelSurface) {
        let Some(sid) = self.sid(surface.wl_surface()) else {
            return;
        };
        let title = with_states(surface.wl_surface(), |states| {
            states
                .data_map
                .get::<XdgToplevelSurfaceData>()
                .and_then(|data| data.lock().ok()?.title.clone())
                .unwrap_or_default()
        });
        if let Some(scene_surface) = self.engine.scene.get_mut(sid) {
            scene_surface.title.clone_from(&title);
        }
        self.reconcile_window(sid);
    }

    fn parent_changed(&mut self, surface: ToplevelSurface) {
        let Some(sid) = self.sid(surface.wl_surface()) else {
            return;
        };
        let parent = surface.parent().and_then(|parent| self.sid(&parent));
        if let Some(scene_surface) = self.engine.scene.get_mut(sid) {
            scene_surface.transient_parent = parent;
        }
        self.reconcile_window(sid);
    }

    /// A toplevel set its app id. Stored by smithay; accepted here (no launcher/grouping policy headless).
    fn app_id_changed(&mut self, _surface: ToplevelSurface) {}

    /// The client asked to be maximized. A headless compositor grants it against the primary output's
    /// logical size and reconfigures with the `Maximized` state (kept `Activated`) so the client redraws
    /// to fill the output and drops its resize affordances. `set_min_size`/`set_max_size` land in
    /// smithay's committed `SurfaceCachedState` automatically; they are not re-sent (they are client→server
    /// hints, not part of the configure).
    fn maximize_request(&mut self, surface: ToplevelSurface) {
        let (w, h) = self.engine.scene.output_logical_size();
        let titlebar_double_click = self.last_pointer_click_count >= 2;
        self.last_pointer_click_count = 1;
        let Some(sid) = self.sid(surface.wl_surface()) else {
            return;
        };
        if titlebar_double_click {
            self.host_fullscreen.remove(&sid);
        } else {
            self.host_fullscreen.insert(sid);
        }
        surface.with_pending_state(|s| {
            s.size = Some((w, h).into());
            // Both variants use the maximized guest layout so GTK retains its client-side header.
            // `host_fullscreen` independently chooses a native Space for the single-click control.
            s.states.set(XdgToplevelState::Maximized);
            s.states.unset(XdgToplevelState::Fullscreen);
            s.states.set(XdgToplevelState::Activated);
        });
        surface.send_configure();
    }

    /// The client asked to leave maximized: drop the state and return to the floating size.
    fn unmaximize_request(&mut self, surface: ToplevelSurface) {
        if let Some(sid) = self.sid(surface.wl_surface()) {
            self.host_fullscreen.remove(&sid);
        }
        surface.with_pending_state(|s| {
            s.states.unset(XdgToplevelState::Maximized);
            // A zero-size configure lets the client restore its own last floating size. Forcing the
            // compositor's initial 800x600 here discards the user's/app's pre-full-screen geometry.
            s.size = None;
        });
        surface.send_configure();
    }

    /// The client asked for fullscreen. Grant it at the output's logical size with the `Fullscreen` state
    /// (the headless compositor has one output; the requested `output` hint is not needed).
    fn fullscreen_request(
        &mut self,
        surface: ToplevelSurface,
        _output: Option<smithay::reexports::wayland_server::protocol::wl_output::WlOutput>,
    ) {
        let (w, h) = self.engine.scene.output_logical_size();
        surface.with_pending_state(|s| {
            s.size = Some((w, h).into());
            s.states.set(XdgToplevelState::Fullscreen);
            s.states.set(XdgToplevelState::Activated);
        });
        surface.send_configure();
    }

    /// The client asked to leave fullscreen: drop the state and return to the floating size.
    fn unfullscreen_request(&mut self, surface: ToplevelSurface) {
        if let Some(sid) = self.sid(surface.wl_surface()) {
            self.host_fullscreen.remove(&sid);
        }
        surface.with_pending_state(|s| {
            s.states.unset(XdgToplevelState::Fullscreen);
            s.size = Some(INITIAL_TOPLEVEL_SIZE.into());
        });
        surface.send_configure();
    }

    /// `xdg_toplevel.move` — the client asked to start an interactive, pointer-driven window MOVE (dragging
    /// its titlebar). HONEST INTENTIONAL NO-OP: the neutral headless scene models no global window position
    /// (every toplevel roots its own tree at `(0, 0)` — there is no desktop plane to slide a window across)
    /// and there is no live user drag to track. The request carries no reply event, so nothing is acked or
    /// faked; a real on-screen compositor would begin a move grab here.
    fn move_request(&mut self, surface: ToplevelSurface, _seat: WlSeat, _serial: Serial) {
        if self.last_pointer_click_count >= 2 {
            self.last_pointer_click_count = 1;
            let Some(sid) = self.sid(surface.wl_surface()) else {
                return;
            };
            self.host_fullscreen.remove(&sid);
            let (width, height) = self.engine.scene.output_logical_size();
            surface.with_pending_state(|state| {
                state.size = Some((width, height).into());
                state.states.set(XdgToplevelState::Maximized);
                state.states.unset(XdgToplevelState::Fullscreen);
                state.states.set(XdgToplevelState::Activated);
            });
            surface.send_configure();
            return;
        }
        if let Some(sid) = self.sid(surface.wl_surface()) {
            self.engine
                .presenter_mut()
                .begin_interaction(sid, crate::scene::model::WindowInteraction::Move);
        }
    }

    /// `xdg_toplevel.resize` — the client asked to start an interactive, pointer-driven RESIZE (dragging a
    /// window edge). HONEST INTENTIONAL NO-OP for the same reason as [`Self::move_request`]: an interactive
    /// resize is driven by a live user drag the headless compositor has no input source for, and it carries
    /// no reply event. (Programmatic sizing IS honored — `maximize_request` / `fullscreen_request` send real
    /// `xdg_toplevel.configure`s with a new size.)
    fn resize_request(
        &mut self,
        _surface: ToplevelSurface,
        _seat: WlSeat,
        _serial: Serial,
        _edges: smithay::reexports::wayland_protocols::xdg::shell::server::xdg_toplevel::ResizeEdge,
    ) {
    }

    /// Forward client-side minimize controls to the host presenter. Headless presenters intentionally
    /// ignore this port; native presenters map it to their platform's taskbar/dock operation.
    fn minimize_request(&mut self, surface: ToplevelSurface) {
        let Some(root) = self.sid(surface.wl_surface()) else {
            return;
        };
        let mut windows = vec![root];
        windows.extend(
            self.engine
                .scene
                .collect_popups_for_root(root)
                .into_iter()
                .map(|(popup, _, _)| popup),
        );
        for window in windows {
            self.engine
                .scene
                .set_visibility(window, Visibility::Minimized);
            self.reconcile_window(window);
        }
        if self.engine.scene.seat().keyboard_focus == Some(root) {
            self.set_keyboard_focus(None);
        }
    }

    /// `xdg_toplevel.show_window_menu` — the client asked the compositor to show ITS server-side window
    /// menu (maximize/minimize/close/…). HONEST INTENTIONAL NO-OP: this compositor draws no server-side
    /// decorations or menus (it composites the client buffer verbatim — see [`XdgDecorationHandler`]), so
    /// there is no menu to show. The request carries no reply event, so nothing is acked or faked.
    fn show_window_menu(
        &mut self,
        _surface: ToplevelSurface,
        _seat: WlSeat,
        _serial: Serial,
        _location: Point<i32, Logical>,
    ) {
    }

    /// An `xdg_popup` mapped (`xdg_surface.get_popup(parent, positioner)`): a menu / dropdown / combo-box
    /// list / tooltip / context menu. Resolve the client's `xdg_positioner` to a concrete on-screen
    /// geometry via the scene's placement math (anchor rect → anchor point → gravity → offset, then the
    /// flip/slide/resize constraint adjustment against the output area), register the popup in the scene's
    /// popup registry linked to its parent (another popup for a submenu chain, or the owning toplevel), and
    /// complete the initial handshake with `xdg_popup.configure(x,y,w,h)` + the paired
    /// `xdg_surface.configure(serial)`. The popup's committed buffer then routes into the scene through the
    /// ordinary commit path: `window_root` climbs it to its toplevel, whose present composites every popup
    /// in its tree at the resolved offset (`Scene::collect_popups_for_root`).
    fn new_popup(&mut self, surface: PopupSurface, positioner: PositionerState) {
        let neutral = Positioner::from(&positioner);
        let parent = surface.get_parent_surface().and_then(|p| self.sid(&p));
        let geometry = parent
            .map(|parent| {
                self.engine
                    .scene
                    .constrain_popup_for_parent(parent, &neutral)
            })
            .unwrap_or_default();
        // Link the scene popup to its parent (toplevel or parent popup). Without a mapped parent we still
        // configure the client, but it cannot composite until its parent exists.
        if let (Some(sid), Some(parent)) = (self.sid(surface.wl_surface()), parent) {
            self.engine.scene.set_role(
                sid,
                SurfaceRole::Popup(PopupState {
                    parent,
                    positioner: neutral,
                    geometry,
                    grabbed: false,
                }),
            );
            self.reconcile_window(sid);
        }
        // Tell the client where it was placed (Smithay emits `xdg_popup.configure` from this pending
        // geometry, paired with `xdg_surface.configure`). MUST precede the client's first buffer attach.
        surface.with_pending_state(|state| {
            state.positioner = positioner;
            state.geometry = Rectangle::new(
                (geometry.x, geometry.y).into(),
                (geometry.w, geometry.h).into(),
            );
        });
        surface.send_configure().ok();
    }

    /// `xdg_popup.grab(seat, serial)`: the client takes an explicit popup grab (menus / context menus do;
    /// tooltips do not). Record the popup in the grab chain and flag it in the scene so a press outside the
    /// chain dismisses it (see [`HlState::inject_pointer_button`] → [`HlState::dismiss_popup_grabs`]). The
    /// chain is ordered outer → inner, so a submenu opened under an existing grab extends it.
    fn grab(&mut self, surface: PopupSurface, _seat: WlSeat, _serial: Serial) {
        if let Some(sid) = self.sid(surface.wl_surface()) {
            if let Some(SurfaceRole::Popup(p)) = self.engine.scene.get_mut(sid).map(|s| &mut s.role)
            {
                p.grabbed = true;
            }
        }
        if !self
            .popup_grabs
            .iter()
            .any(|p| p.wl_surface() == surface.wl_surface())
        {
            self.popup_grabs.push(surface);
        }
    }

    /// `xdg_popup.reposition(positioner, token)` (xdg-shell v3): a mapped popup is re-anchored (e.g. a menu
    /// re-placing as the pointer walks a menu bar). Recompute the geometry from the NEW positioner, update
    /// the scene popup in place, and answer `xdg_popup.repositioned(token)` (which also emits the fresh
    /// configure/ack). The scene composites at the new offset once the client acks and re-commits.
    fn reposition_request(
        &mut self,
        surface: PopupSurface,
        positioner: PositionerState,
        token: u32,
    ) {
        let neutral = Positioner::from(&positioner);
        let sid = self.sid(surface.wl_surface());
        let geometry = sid
            .and_then(|sid| self.engine.scene.popup_parent(sid))
            .map(|parent| {
                self.engine
                    .scene
                    .constrain_popup_for_parent(parent, &neutral)
            })
            .unwrap_or_default();
        if let Some(sid) = sid {
            if let Some(SurfaceRole::Popup(p)) = self.engine.scene.get_mut(sid).map(|s| &mut s.role)
            {
                p.positioner = neutral;
                p.geometry = geometry;
            }
            self.reconcile_window(sid);
        }
        surface.with_pending_state(|state| {
            state.positioner = positioner;
            state.geometry = Rectangle::new(
                (geometry.x, geometry.y).into(),
                (geometry.w, geometry.h).into(),
            );
        });
        surface.send_repositioned(token);
    }

    /// A popup's role was destroyed (the client tore the menu/tooltip down, or honoured a grab dismissal).
    /// Drop it from the grab chain; the scene surface + popup-registry entry are reclaimed when its
    /// `wl_surface` is destroyed (`teardown_surface`), and the owning toplevel re-presents on its next
    /// commit so the menu visibly disappears.
    fn popup_destroyed(&mut self, surface: PopupSurface) {
        self.popup_grabs
            .retain(|p| p.wl_surface() != surface.wl_surface());
        if let Some(sid) = self.sid(surface.wl_surface()) {
            let root = self.engine.scene.window_root(sid);
            // Destroying xdg_popup unmaps the transient even when the client keeps the underlying
            // wl_surface object alive briefly. AppKit child windows retain their children, so waiting for
            // wl_surface.destroy leaves a visually stuck menu across navigation changes.
            self.engine.presenter_mut().destroy_window(sid);
            self.engine.scene.clear_role(sid);
            if let Some(root) = root {
                self.engine.scene.mark_dirty(root);
                self.arm_repaint(root);
            }
        }
    }
}

/// Server-side handling of `zxdg_decoration_manager_v1`. The negotiation contract: whenever a client
/// creates a decoration object or expresses a preference, it MUST receive a `configure(mode)` telling it
/// whether the compositor draws the frame (server-side) or the client must draw its own (client-side).
/// A toolkit that never hears back stalls before mapping. This headless compositor composites the client
/// buffer verbatim (it draws no frame of its own), so it honors the client's preferred mode when one is
/// given and defaults to server-side (i.e. "no client CSD needed") otherwise. Smithay's
/// `ToplevelSurface::send_configure` emits the `zxdg_toplevel_decoration_v1.configure` from the pending
/// `decoration_mode` for us — we only set the mode and configure.
impl XdgDecorationHandler for HlState {
    /// The client attached a decoration object (`get_toplevel_decoration`) without yet stating a
    /// preference: answer with the server-side default so it knows not to draw CSD.
    fn new_decoration(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(DecorationMode::ServerSide);
        });
        toplevel.send_configure();
    }

    /// The client stated a preferred mode (`set_mode`): honor it and re-configure. GTK/Chrome request
    /// `ClientSide` to draw their own titlebar; granting exactly what they ask avoids a mode fight (the
    /// double-decoration / no-decoration hangs a mismatched reply causes).
    fn request_mode(&mut self, toplevel: ToplevelSurface, mode: DecorationMode) {
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(mode);
        });
        toplevel.send_configure();
    }

    /// The client withdrew its preference (`unset_mode`): fall back to the server-side default.
    fn unset_mode(&mut self, toplevel: ToplevelSurface) {
        toplevel.with_pending_state(|s| {
            s.decoration_mode = Some(DecorationMode::ServerSide);
        });
        toplevel.send_configure();
    }
}
