use super::*;

/// Handles output membership and auxiliary Wayland protocol extensions.
impl OutputHandler for HlState {}

/// A client created a `wp_fractional_scale_v1` for a surface. Tell it the compositor's preferred
/// fractional render scale so it can rasterize crisply on HiDPI without integer-only
/// `wl_surface.set_buffer_scale`. We source the scale from the primary output's scale (consistent with the
/// legacy integer `wl_output.scale`); smithay serializes it as `round(scale × 120)`.
impl FractionalScaleHandler for HlState {
    fn new_fractional_scale(&mut self, surface: WlSurface) {
        // Source the preferred scale from the surface's OWN output (its selected output, else the primary),
        // so a surface already routed to a HiDPI output learns the larger scale — not just the primary's.
        match self.sid(&surface) {
            Some(sid) => self.send_preferred_fractional_scale(sid),
            None => {
                let scale = self
                    .engine
                    .scene
                    .primary_output()
                    .map(|o| o.scale.max(1))
                    .unwrap_or(1) as f64;
                with_states(&surface, |states| {
                    with_fractional_scale(states, |fractional| {
                        fractional.set_preferred_scale(scale);
                    });
                });
            }
        }
    }
}

/// Server-side handling of `zwp_primary_selection_device_manager_v1` (the middle-click PRIMARY selection).
/// Hands Smithay the held [`PrimarySelectionState`]; the default selection transfer is enough for a client
/// to set a `zwp_primary_selection_source_v1` while focused and for the next focused client to read it over
/// a real fd — exactly like the data-device clipboard, but on the primary selection. Focus follows the
/// keyboard via [`set_primary_focus`] (see [`HlState::set_keyboard_focus`]). See the
/// `primary_selection_roundtrip` demo.
impl PrimarySelectionHandler for HlState {
    fn primary_selection_state(&self) -> &PrimarySelectionState {
        &self.primary_selection
    }
}

/// Server-side policy for `zwp_pointer_constraints_v1` (pointer lock / confinement). The compositor decides
/// WHEN a constraint engages; the headless policy is the standard one: activate it as soon as it is created
/// on a surface that currently holds pointer focus. Activation sends the client
/// `zwp_locked_pointer_v1.locked` / `zwp_confined_pointer_v1.confined`; a LOCKED pointer then stops
/// receiving absolute motion (see [`HlState::inject_pointer_motion`]). `cursor_position_hint` (the client's
/// rendered-cursor position while locked) needs no action headless — there is no hardware cursor to warp.
impl PointerConstraintsHandler for HlState {
    fn new_constraint(&mut self, surface: &WlSurface, pointer: &PointerHandle<Self>) {
        // Engage immediately if the constrained surface already holds pointer focus (the common case: a
        // client locks the pointer while the cursor is over it). Otherwise the constraint stays dormant
        // until the surface next gains focus — smithay re-checks activation there is out of scope headless,
        // so a client that constrains before entry re-issues after `wl_pointer.enter`.
        let focused = pointer.current_focus().is_some_and(|f| &f == surface);
        if focused {
            with_pointer_constraint(surface, pointer, |constraint| {
                if let Some(constraint) = constraint {
                    constraint.activate();
                }
            });
        }
    }

    fn cursor_position_hint(
        &mut self,
        _surface: &WlSurface,
        _pointer: &PointerHandle<Self>,
        _location: Point<f64, Logical>,
    ) {
    }
}

/// Server-side policy for `xdg_activation_v1` (cross-client activation / focus request). A client that
/// obtained an activation token (optionally carrying the seat+serial of the input event that triggered it)
/// calls `activate(token, surface)`; the headless single-window policy honours EVERY activation of a known
/// toplevel by granting it keyboard focus — the standard "bring the target to the front / make it active"
/// behaviour, observable to the client as a `wl_keyboard.enter` on the activated surface (and the clipboard /
/// primary selection follow, via [`HlState::set_keyboard_focus`]). `token_created` keeps its default (every
/// token is accepted); a real compositor might reject stale or seat-less tokens here. The token stays in the
/// pool after use — we do not `remove_token`, so it can be inspected — mirroring Smithay's contract that the
/// compositor owns token lifetime.
impl XdgActivationHandler for HlState {
    fn activation_state(&mut self) -> &mut XdgActivationState {
        &mut self._xdg_activation
    }

    fn request_activation(
        &mut self,
        _token: XdgActivationToken,
        _token_data: XdgActivationTokenData,
        surface: WlSurface,
    ) {
        // Honour the activation by focusing the target toplevel. Only a known toplevel root is activated (a
        // popup/subsurface is not a focus target); an unknown or non-toplevel surface is ignored.
        let Some(sid) = self.sid(&surface) else {
            return;
        };
        let is_toplevel = matches!(
            self.engine.scene.get(sid).map(|s| &s.role),
            Some(SurfaceRole::Toplevel)
        );
        if is_toplevel {
            self.set_keyboard_focus(Some(sid));
        }
    }
}

/// Server-side handling of `zwp_idle_inhibit_manager_v1`. A client creating a `zwp_idle_inhibitor_v1` on a
/// surface asks the compositor to keep the system awake (no screensaver / DPMS) while that surface is
/// visible. There is no reply event — the compositor simply tracks it — so headless the handler records the
/// inhibited surface in the shared [`Observations`] (and drops it on the inhibitor's `destroy`), which is
/// the exact state a test asserts. A real host would additionally suppress its idle timer while the set is
/// non-empty and the surface is mapped.
impl IdleInhibitHandler for HlState {
    fn inhibit(&mut self, surface: WlSurface) {
        self.observations
            .lock()
            .unwrap()
            .idle_inhibited
            .insert(surface.id().protocol_id());
    }

    fn uninhibit(&mut self, surface: WlSurface) {
        self.observations
            .lock()
            .unwrap()
            .idle_inhibited
            .remove(&surface.id().protocol_id());
    }
}

/// Server-side policy for `zwp_keyboard_shortcuts_inhibit_v1` (key-grab). A client (a terminal, an
/// embedded VNC/RDP viewer, a game) that must receive EVERY key — including combos the compositor would
/// otherwise swallow as its own shortcuts — creates an inhibitor for its surface + this seat. Headless
/// there is no compositor shortcut table to suppress, so the policy is to ALWAYS grant: each new inhibitor
/// is immediately [`activate`](KeyboardShortcutsInhibitor::activate)d — which sends the client the `active`
/// event (the client-visible proof the grab took) and flips
/// [`Seat::keyboard_shortcuts_inhibited`](smithay::input::Seat) so a real shortcut handler could consult it
/// — and the inhibited surface is recorded into [`Observations`]. Destroying the inhibitor (or the client
/// vanishing) removes it again.
impl KeyboardShortcutsInhibitHandler for HlState {
    fn keyboard_shortcuts_inhibit_state(&mut self) -> &mut KeyboardShortcutsInhibitState {
        &mut self.keyboard_shortcuts_inhibit
    }

    fn new_inhibitor(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        // Grant the grab: activate it (delivers `active` to the client) and track the surface so a test can
        // assert BOTH the wire event and the server-side record.
        inhibitor.activate();
        let sid = inhibitor.wl_surface().id().protocol_id();
        hl_debug!(
            tag::WAYLAND,
            "zwp_keyboard_shortcuts_inhibit: activated for surface {sid}"
        );
        self.observations
            .lock()
            .unwrap()
            .shortcuts_inhibited
            .insert(sid);
    }

    fn inhibitor_destroyed(&mut self, inhibitor: KeyboardShortcutsInhibitor) {
        let sid = inhibitor.wl_surface().id().protocol_id();
        self.observations
            .lock()
            .unwrap()
            .shortcuts_inhibited
            .remove(&sid);
    }
}

/// Compositor-side hooks for `zwp_input_method_v2` popups (the candidate-list window an IME draws near the
/// cursor). Headless there is no on-screen IME popup surface to place or composite — the text-input round
/// trip a test drives (preedit/commit) needs no popup — so `parent_geometry` reports a zero rectangle and
/// the popup lifecycle callbacks are honest no-ops. The text-input DELIVERY path (enter + commit/preedit +
/// done) does not depend on any of these; they exist only so `delegate_input_method_manager!` can bind.
impl InputMethodHandler for HlState {
    fn new_popup(&mut self, _surface: ImePopupSurface) {}
    fn dismiss_popup(&mut self, _surface: ImePopupSurface) {}
    fn popup_repositioned(&mut self, _surface: ImePopupSurface) {}
    fn parent_geometry(&self, _parent: &WlSurface) -> Rectangle<i32, Logical> {
        Rectangle::default()
    }
}

/// Server-side policy for `zwp_tablet_manager_v2`. The single advertised tablet + pen tool live on the
/// seat's tablet-seat (added in [`HlState::new`]); a client that binds `get_tablet_seat` receives them and
/// the host stylus seam drives the tool. `tablet_tool_image` (the client asking the compositor to set a
/// hardware cursor for the tool) keeps its default no-op — headless there is no hardware cursor to warp.
impl TabletSeatHandler for HlState {}

/// Server-side handling of `ext_session_lock_manager_v1` (screen lock). A client's `lock` request lands in
/// [`Self::lock`]: the compositor HIDES every normal toplevel (so their content stops presenting — the
/// screen "blanks") and confirms the lock with [`SessionLocker::lock`], which sends the client the `locked`
/// event. The client then creates a lock surface per output ([`Self::new_surface`]); the adapter gives it a
/// toplevel role so its committed buffer composites + presents through the ordinary path, and configures it
/// to the output size. `unlock` restores every normal toplevel to visible and re-presents it. The lock/unlock
/// transition is mirrored into [`Observations::session_locked`](super::present::Observations) for the test.
impl SessionLockHandler for HlState {
    fn lock_state(&mut self) -> &mut SessionLockManagerState {
        &mut self._session_lock
    }

    fn lock(&mut self, confirmation: SessionLocker) {
        // Hide the normal surfaces first, THEN confirm — the client must not observe `locked` before the
        // compositor has actually stopped presenting protected content.
        self.set_session_locked(true);
        confirmation.lock();
    }

    fn unlock(&mut self) {
        self.set_session_locked(false);
    }

    fn new_surface(&mut self, surface: LockSurface, _output: WlOutput) {
        // Give the lock surface a toplevel role so its committed buffer composes + presents as a window
        // root (the neutral scene has no dedicated lock layer; a full-output toplevel is the faithful
        // reduction). Track it so `set_session_locked` never occludes it.
        if let Some(sid) = self.sid(surface.wl_surface()) {
            self.engine.scene.set_role(sid, SurfaceRole::Toplevel);
            self.engine.scene.set_visibility(sid, Visibility::Visible);
            self.reconcile_window(sid);
            if !self.lock_surfaces.contains(&sid) {
                self.lock_surfaces.push(sid);
            }
            // The lock surface takes keyboard focus (a real lock screen receives the unlock passphrase).
            self.set_keyboard_focus(Some(sid));
        }
        // Configure it to the output's logical size, as the protocol requires, so the client draws.
        let (w, h) = self.engine.scene.output_logical_size();
        surface.with_pending_state(|state| {
            state.size = Some((w.max(1) as u32, h.max(1) as u32).into());
        });
        surface.send_configure();
    }
}

smithay::delegate_pointer_gestures!(HlState);
smithay::delegate_tablet_manager!(HlState);
smithay::delegate_session_lock!(HlState);
smithay::delegate_text_input_manager!(HlState);
smithay::delegate_input_method_manager!(HlState);
smithay::delegate_compositor!(HlState);
smithay::delegate_shm!(HlState);
smithay::delegate_dmabuf!(HlState);
smithay::delegate_xdg_shell!(HlState);
smithay::delegate_xdg_decoration!(HlState);
smithay::delegate_output!(HlState);
smithay::delegate_seat!(HlState);
smithay::delegate_data_device!(HlState);
smithay::delegate_primary_selection!(HlState);
smithay::delegate_relative_pointer!(HlState);
smithay::delegate_pointer_constraints!(HlState);
smithay::delegate_presentation!(HlState);
smithay::delegate_viewporter!(HlState);
smithay::delegate_fractional_scale!(HlState);
smithay::delegate_xdg_activation!(HlState);
smithay::delegate_idle_inhibit!(HlState);
smithay::delegate_content_type!(HlState);
smithay::delegate_cursor_shape!(HlState);
smithay::delegate_single_pixel_buffer!(HlState);
smithay::delegate_keyboard_shortcuts_inhibit!(HlState);
