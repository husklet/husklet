use super::*;

impl HlState {
    /// Candidate window roots a pointer hit-test walks, best-first: focused window, then other toplevels.
    pub(super) fn candidate_roots(&self) -> Vec<SurfaceId> {
        let mut roots = Vec::new();
        if let Some(focus) = self.engine.scene.seat().keyboard_focus {
            if let Some(root) = self.engine.scene.window_root(focus) {
                roots.push(root);
            }
        }
        for tl in self.engine.scene.toplevels() {
            if !roots.contains(&tl) {
                roots.push(tl);
            }
        }
        roots
    }

    /// Hit-test the tree(s) at root-local logical `(x, y)`: the input-sensitive surface under the point
    /// and its window root + root-space origin `(ox, oy)`. Returns `(root, hit_surface, ox, oy)`.
    pub(super) fn hit_test(&self, x: f64, y: f64) -> Option<(SurfaceId, SurfaceId, i32, i32)> {
        let (ix, iy) = (x.floor() as i32, y.floor() as i32);
        for root in self.candidate_roots() {
            if let Some((sid, ox, oy)) = surface_at(&self.engine.scene, root, ix, iy) {
                return Some((root, sid, ox, oy));
            }
        }
        None
    }

    /// Move the pointer to root-local logical `(x, y)`. Hit-tests the surface under the point, updates
    /// the neutral seat, and drives smithay's `PointerHandle::motion` + `frame` with that focus so the
    /// client receives `wl_pointer.leave`/`enter` (on the surface under the cursor changing) and
    /// `wl_pointer.motion` at the correct surface-local coordinate (`(x, y)` minus the surface origin).
    pub fn inject_pointer_motion(&mut self, x: f64, y: f64) {
        self.inject_pointer_motion_on(None, x, y);
    }

    pub(super) fn inject_pointer_motion_on(&mut self, window: Option<SurfaceId>, x: f64, y: f64) {
        hl_debug!(tag::WAYLAND, "input motion x={:.0} y={:.0}", x, y);
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let hit = window
            .and_then(|window| self.engine.scene.window_root(window))
            .and_then(|root| {
                let (ix, iy) = (x.floor() as i32, y.floor() as i32);
                surface_at(&self.engine.scene, root, ix, iy)
                    .map(|(surface, ox, oy)| (root, surface, ox, oy))
            })
            .or_else(|| window.is_none().then(|| self.hit_test(x, y)).flatten());

        // Build smithay's focus: the concrete `WlSurface` + its origin in global (root) space.
        let focus = hit.and_then(|(_, sid, ox, oy)| {
            self.surfaces_by_id
                .get(&sid)
                .cloned()
                .map(|wl| (wl, Point::<f64, Logical>::from((ox as f64, oy as f64))))
        });
        // Whether the focused surface holds an ACTIVE `zwp_locked_pointer_v1` — if so the pointer is frozen
        // in place: no absolute `wl_pointer.motion` is delivered and the neutral seat location does not move.
        // The client drives purely off relative motion (below), the standard pointer-lock experience.
        let locked = focus
            .as_ref()
            .is_some_and(|(wl, _)| self.pointer_locked_on(wl));

        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();

        // Relative motion (delta since the last RAW injected position) — delivered to any bound
        // `zwp_relative_pointer_v1` on the focused surface. A no-op if the client bound no relative pointer.
        // Computed against the raw position (not the delivered absolute one) so a locked pointer still
        // reports the real per-move device delta while its absolute position stays frozen.
        let (old_x, old_y) = self.last_injected_pointer;
        self.last_injected_pointer = (x, y);
        let (dx, dy) = (x - old_x, y - old_y);
        if dx != 0.0 || dy != 0.0 {
            pointer.relative_motion(
                self,
                focus.clone(),
                &RelativeMotionEvent {
                    delta: (dx, dy).into(),
                    delta_unaccel: (dx, dy).into(),
                    utime: time as u64 * 1_000, // ms → µs (the relative-pointer protocol's unit)
                },
            );
        }

        if locked {
            // Pointer position is locked: skip absolute motion and leave the neutral seat frozen. Still
            // emit a `wl_pointer.frame` so the relative motion above is a complete, framed update.
            pointer.frame(self);
            return;
        }

        // Keep the neutral seat consistent with what we deliver over the wire (for inspection/tests). Log
        // the enter/leave transition (focus changed) so the pointer-focus handoff is traceable in a trace.
        let new_focus = hit.map(|(_, sid, _, _)| sid);
        let prev_focus = self.engine.scene.seat().pointer_focus;
        if new_focus != prev_focus {
            hl_debug!(
                tag::WAYLAND,
                "pointer focus from={:?} to={:?} at x={:.0} y={:.0}",
                prev_focus.map(|s| s.0),
                new_focus.map(|s| s.0),
                x,
                y
            );
        }
        self.engine.scene.seat_mut().pointer_location = (x, y);
        self.engine.scene.seat_mut().pointer_focus = new_focus;
        pointer.motion(
            self,
            focus,
            &MotionEvent {
                location: (x, y).into(),
                serial,
                time,
            },
        );
        pointer.frame(self);
    }

    /// Whether `surface` currently holds an ACTIVE `zwp_locked_pointer_v1` constraint on this seat's
    /// pointer — the check [`Self::inject_pointer_motion`] uses to freeze the absolute pointer position.
    pub(super) fn pointer_locked_on(&self, surface: &WlSurface) -> bool {
        let Some(pointer) = self.seat.get_pointer() else {
            return false;
        };
        with_pointer_constraint(
            surface,
            &pointer,
            |constraint| matches!(constraint, Some(c) if c.is_active() && matches!(&*c, PointerConstraint::Locked(_))),
        )
    }

    /// Press or release a pointer button. Uses the pointer's CURRENT focus (from the last motion), which
    /// smithay tracks internally — so a button lands on whatever surface the cursor is over.
    pub fn inject_pointer_button(&mut self, button: u32, pressed: bool) {
        hl_debug!(tag::WAYLAND, "input button={} pressed={}", button, pressed);
        // An explicit popup grab (menu / context-menu) dismisses on a press that lands outside the whole
        // popup chain — the click-outside-closes-the-menu semantics. Uses the pointer's last known location
        // (set by the preceding `inject_pointer_motion`). The button itself is still delivered below.
        if pressed {
            let (x, y) = self.engine.scene.seat().pointer_location;
            if self.press_outside_grab_chain(x, y) {
                self.dismiss_popup_grabs();
            }
        }
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        let state = if pressed {
            ButtonState::Pressed
        } else {
            ButtonState::Released
        };
        pointer.button(
            self,
            &ButtonEvent {
                serial,
                time,
                button,
                state,
            },
        );
        pointer.frame(self);
    }

    /// Scroll the pointer by logical `horizontal`/`vertical` amounts (wheel source). A zero component is
    /// omitted so the client only sees the axes that actually moved.
    pub fn inject_pointer_axis(&mut self, horizontal: f64, vertical: f64) {
        hl_debug!(
            tag::WAYLAND,
            "input axis h={:.1} v={:.1}",
            horizontal,
            vertical
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let time = self.input_time_ms();
        let mut frame = AxisFrame::new(time).source(AxisSource::Wheel);
        if horizontal != 0.0 {
            frame = frame.value(Axis::Horizontal, horizontal);
        }
        if vertical != 0.0 {
            frame = frame.value(Axis::Vertical, vertical);
        }
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    /// Scroll by DISCRETE wheel steps: emit the smooth `horizontal`/`vertical` value AND the discrete
    /// `h120`/`v120` step counts (120 units = one detent) in one framed `wl_pointer` axis event, so a
    /// client that reads discrete notches (`wl_pointer.axis_value120` on v8+, `axis_discrete` on v5-7)
    /// sees the exact step count + sign alongside the smooth value — what a real mouse wheel delivers.
    pub fn inject_pointer_axis_discrete(
        &mut self,
        horizontal: f64,
        vertical: f64,
        h120: i32,
        v120: i32,
    ) {
        hl_debug!(
            tag::WAYLAND,
            "input axis h={:.1} v={:.1} h120={} v120={}",
            horizontal,
            vertical,
            h120,
            v120
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let time = self.input_time_ms();
        let mut frame = AxisFrame::new(time).source(AxisSource::Wheel);
        if horizontal != 0.0 {
            frame = frame.value(Axis::Horizontal, horizontal);
        }
        if vertical != 0.0 {
            frame = frame.value(Axis::Vertical, vertical);
        }
        if h120 != 0 {
            frame = frame.v120(Axis::Horizontal, h120);
        }
        if v120 != 0 {
            frame = frame.v120(Axis::Vertical, v120);
        }
        pointer.axis(self, frame);
        pointer.frame(self);
    }

    /// Give keyboard focus to `sid` (or clear it with `None`). Drives smithay's `KeyboardHandle::set_focus`
    /// — which emits `wl_keyboard.leave` to the old focus and `wl_keyboard.enter` (+ the keymap already
    /// sent at bind) to the new — and mirrors the change into the neutral seat.
    pub fn set_keyboard_focus(&mut self, sid: Option<SurfaceId>) {
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let surface = sid.and_then(|s| self.surfaces_by_id.get(&s).cloned());
        // Follow the keyboard focus with the clipboard (data-device) focus so the newly focused client's
        // `wl_data_device` receives selection offers and its `set_selection` is honored — the standard
        // Wayland "clipboard follows keyboard focus" rule. `None` clears it (no client owns the clipboard).
        let focus_client = surface
            .as_ref()
            .and_then(|s| self.display.get_client(s.id()).ok());
        set_data_device_focus(&self.display, &self.seat, focus_client.clone());
        // The PRIMARY (middle-click) selection follows keyboard focus by the same rule, so the newly focused
        // client's `zwp_primary_selection_device_v1` receives the current primary offer and its
        // `set_selection` is honored.
        set_primary_focus(&self.display, &self.seat, focus_client);
        // Mirror into the neutral seat so scene focus bookkeeping stays truthful.
        let prev = self.engine.scene.seat().keyboard_focus;
        self.engine.scene.seat_mut().keyboard_focus = sid;
        let serial = SERIAL_COUNTER.next_serial();
        hl_debug!(
            tag::WAYLAND,
            "focus kbd from={:?} to={:?} serial={:?}",
            prev.map(|s| s.0),
            sid.map(|s| s.0),
            serial
        );
        keyboard.set_focus(self, surface, serial);
    }

    /// Press or release a key by EVDEV keycode. smithay's keymap is keyed on X11 keycodes (evdev + 8),
    /// and its `KeyboardTarget` impl sends `evdev` back to the client (`raw - 8`); we add the 8 here so
    /// the caller speaks Linux `input-event-codes` and the client receives the same value. Modifiers are
    /// tracked by smithay's xkb state across presses. No compositor keybinding filter — always forward.
    pub fn inject_key(&mut self, keycode: u32, pressed: bool) {
        hl_debug!(tag::WAYLAND, "input key={} pressed={}", keycode, pressed);
        let Some(keyboard) = self.seat.get_keyboard() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        let state = if pressed {
            KeyState::Pressed
        } else {
            KeyState::Released
        };
        keyboard.input::<(), _>(
            self,
            Keycode::new(keycode + 8),
            state,
            serial,
            time,
            |_, _, _| FilterResult::Forward,
        );
    }

    /// Deliver an IME `commit_string` (+ `done`) to the focused, enabled `zwp_text_input_v3` — the host
    /// text-entry seam, mirroring [`Self::inject_key`] but for composed text. Smithay routes text-input
    /// through the seat's [`TextInputHandle`](smithay::wayland::text_input); `with_active_text_input` targets
    /// the text-input instance the focused client ENABLED (which requires an input method instance to exist,
    /// hence the advertised `zwp_input_method_manager_v2`), and `done(false)` stamps the correct serial (the
    /// client's own commit count, tracked by Smithay) so the client applies the change. A no-op if no
    /// text-input is focused+active.
    pub fn inject_ime_commit_string(&mut self, text: String) {
        hl_debug!(tag::WAYLAND, "input ime commit {:?}", text);
        let ti = self.seat.text_input();
        ti.with_active_text_input(|obj, _surface| obj.commit_string(Some(text.clone())));
        ti.done(false);
    }

    /// Deliver an IME `preedit_string` (composing text, with byte-offset cursor) + `done` to the focused,
    /// enabled `zwp_text_input_v3`. See [`Self::inject_ime_commit_string`] for the routing; the preedit is
    /// transient text the client shows before a commit replaces it.
    pub fn inject_ime_preedit_string(&mut self, text: String, cursor_begin: i32, cursor_end: i32) {
        hl_debug!(
            tag::WAYLAND,
            "input ime preedit {:?} [{},{}]",
            text,
            cursor_begin,
            cursor_end
        );
        let ti = self.seat.text_input();
        ti.with_active_text_input(|obj, _surface| {
            obj.preedit_string(Some(text.clone()), cursor_begin, cursor_end)
        });
        ti.done(false);
    }

    /// Deliver an IME `delete_surrounding_text` (+ `done`) to the focused, enabled `zwp_text_input_v3` —
    /// delete `before`/`after` bytes around the cursor. See [`Self::inject_ime_commit_string`] for routing.
    pub fn inject_ime_delete_surrounding(&mut self, before: u32, after: u32) {
        hl_debug!(
            tag::WAYLAND,
            "input ime delete_surrounding before={} after={}",
            before,
            after
        );
        let ti = self.seat.text_input();
        ti.with_active_text_input(|obj, _surface| obj.delete_surrounding_text(before, after));
        ti.done(false);
    }

    /// Ask the topmost mapped toplevel to close (`xdg_toplevel.close`). The compositor-initiated close
    /// request a window-manager close affordance sends: the client receives `close` and typically destroys
    /// the toplevel (there is no reply event, so nothing is acked). A no-op if no toplevel is mapped or its
    /// Smithay `ToplevelSurface` is no longer live. Resolves the concrete [`ToplevelSurface`] by matching the
    /// topmost neutral surface's `wl_surface` against the shell's live toplevels (which own `send_close`).
    pub fn close_topmost_toplevel(&mut self) {
        let Some(sid) = self.topmost_toplevel() else {
            return;
        };
        self.close_toplevel(sid);
    }
}
