use super::*;
impl HlState {
    /// Apply one host/test-driven [`InputCommand`], routing it through the seat's pointer/keyboard.
    pub fn apply_input(&mut self, cmd: InputCommand) {
        // Latency trace: stamp the host-monotonic time this input was DISPATCHED into the compositor (the
        // start of the input→present cycle). Terse key=val, gated with the rest of `tag::WAYLAND` — pairs
        // with the `present_done … t_us=` line the engine logs when the resulting frame ships, so a trace
        // can subtract the two for the real input→present latency.
        hl_debug!(
            tag::WAYLAND,
            "input_dispatch t_us={}",
            self.engine.clock().now_nanos() / 1_000
        );
        match cmd {
            InputCommand::PointerMotion { x, y } => self.inject_pointer_motion(x, y),
            InputCommand::PointerMotionOn { window, x, y } => {
                self.inject_pointer_motion_on(Some(window), x, y)
            }
            InputCommand::PointerButton { button, pressed } => {
                self.inject_pointer_button(button, pressed)
            }
            InputCommand::PointerButtonOn {
                window,
                button,
                pressed,
                click_count,
            } => {
                self.last_pointer_click_count = click_count.max(1);
                self.set_keyboard_focus(self.engine.scene.window_root(window));
                self.inject_pointer_button(button, pressed);
            }
            InputCommand::ResizeSurface {
                surface,
                width,
                height,
                maximized,
                fullscreen,
                resizing,
            } => self
                .configure_native_resize(surface, width, height, maximized, fullscreen, resizing),
            InputCommand::ResizeSurfaceEnd { surface } => self.finish_native_resize(surface),
            InputCommand::PointerAxis {
                horizontal,
                vertical,
            } => self.inject_pointer_axis(horizontal, vertical),
            InputCommand::PointerAxisDiscrete {
                horizontal,
                vertical,
                h120,
                v120,
            } => self.inject_pointer_axis_discrete(horizontal, vertical, h120, v120),
            InputCommand::PointerAxisFinger {
                horizontal,
                vertical,
            } => self.inject_pointer_axis_finger(horizontal, vertical),
            InputCommand::PointerAxisStop {
                horizontal,
                vertical,
            } => self.inject_pointer_axis_stop(horizontal, vertical),
            InputCommand::MoveToplevelToPoint { index, x, y } => {
                self.move_toplevel_to_point(index, x, y)
            }
            InputCommand::Key { keycode, pressed } => self.inject_key(keycode, pressed),
            InputCommand::FocusTopmostKeyboard => {
                let target = self.topmost_toplevel();
                self.set_keyboard_focus(target);
            }
            InputCommand::FocusSurface(surface) => {
                let target = self.engine.scene.window_root(surface);
                self.set_keyboard_focus(target);
            }
            InputCommand::FocusToplevelIndex(n) => {
                let target = self.toplevel_at(n);
                self.set_keyboard_focus(target);
            }
            InputCommand::ClearKeyboardFocus => self.set_keyboard_focus(None),
            InputCommand::ImeCommitString(text) => self.inject_ime_commit_string(text),
            InputCommand::ImePreeditString {
                text,
                cursor_begin,
                cursor_end,
            } => self.inject_ime_preedit_string(text, cursor_begin, cursor_end),
            InputCommand::ImeDeleteSurrounding {
                before_length,
                after_length,
            } => self.inject_ime_delete_surrounding(before_length, after_length),
            InputCommand::CloseTopmostToplevel => self.close_topmost_toplevel(),
            InputCommand::CloseSurface(surface) => self.close_toplevel(surface),
            InputCommand::TouchDown { id, x, y } => self.inject_touch_down(id, x, y),
            InputCommand::TouchMotion { id, x, y } => self.inject_touch_motion(id, x, y),
            InputCommand::TouchUp { id } => self.inject_touch_up(id),
            InputCommand::TouchFrame => self.inject_touch_frame(),
            InputCommand::TouchCancel => self.inject_touch_cancel(),
            InputCommand::GestureSwipeBegin { fingers } => self.inject_gesture_swipe_begin(fingers),
            InputCommand::GestureSwipeUpdate { dx, dy } => self.inject_gesture_swipe_update(dx, dy),
            InputCommand::GestureSwipeEnd { cancelled } => self.inject_gesture_swipe_end(cancelled),
            InputCommand::GesturePinchBegin { fingers } => self.inject_gesture_pinch_begin(fingers),
            InputCommand::GesturePinchUpdate {
                dx,
                dy,
                scale,
                rotation,
            } => self.inject_gesture_pinch_update(dx, dy, scale, rotation),
            InputCommand::GesturePinchEnd { cancelled } => self.inject_gesture_pinch_end(cancelled),
            InputCommand::TabletToolProximityIn { x, y } => self.inject_tablet_proximity_in(x, y),
            InputCommand::TabletToolMotion { x, y, pressure } => {
                self.inject_tablet_motion(x, y, pressure)
            }
            InputCommand::TabletToolTipDown => self.inject_tablet_tip_down(),
            InputCommand::TabletToolTipUp => self.inject_tablet_tip_up(),
            InputCommand::TabletToolProximityOut => self.inject_tablet_proximity_out(),
            InputCommand::SessionLock => self.lock_session(),
            InputCommand::SessionUnlock => self.unlock_session(),
        }
    }

    pub(super) fn configure_native_resize(
        &mut self,
        surface: SurfaceId,
        width: u32,
        height: u32,
        maximized: bool,
        fullscreen: bool,
        resizing: bool,
    ) {
        let toplevel = self
            .xdg_shell
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| self.sid(toplevel.wl_surface()) == Some(surface))
            .cloned();
        let Some(toplevel) = toplevel else { return };
        let host_fullscreen = self.host_fullscreen.contains(&surface);
        if !fullscreen {
            self.host_fullscreen.remove(&surface);
        }
        hl_debug!(
            tag::WAYLAND,
            "native resize configure surface={} size={}x{}",
            surface.0,
            width,
            height
        );
        let size = (
            width.clamp(1, i32::MAX as u32) as i32,
            height.clamp(1, i32::MAX as u32) as i32,
        );
        toplevel.with_pending_state(|state| {
            state.size = Some(size.into());
            let guest_maximized = maximized || (fullscreen && host_fullscreen);
            let guest_fullscreen = fullscreen && !host_fullscreen;
            if guest_maximized {
                state.states.set(XdgToplevelState::Maximized);
            } else {
                state.states.unset(XdgToplevelState::Maximized);
            }
            if guest_fullscreen {
                state.states.set(XdgToplevelState::Fullscreen);
            } else {
                state.states.unset(XdgToplevelState::Fullscreen);
            }
            state.states.set(XdgToplevelState::Activated);
            if resizing {
                state.states.set(XdgToplevelState::Resizing);
            } else {
                state.states.unset(XdgToplevelState::Resizing);
            }
        });
        toplevel.send_configure();
    }

    pub(super) fn finish_native_resize(&mut self, surface: SurfaceId) {
        let toplevel = self
            .xdg_shell
            .toplevel_surfaces()
            .iter()
            .find(|toplevel| self.sid(toplevel.wl_surface()) == Some(surface))
            .cloned();
        let Some(toplevel) = toplevel else { return };
        toplevel.with_pending_state(|state| {
            state.states.unset(XdgToplevelState::Resizing);
        });
        toplevel.send_configure();
        hl_debug!(tag::WAYLAND, "native resize end surface={}", surface.0);
    }

    /// The most recently mapped toplevel (highest surface id) — the "topmost" window an input-focus
    /// intent targets. `None` if no toplevel is mapped. A stand-in for real z-order/stacking, which the
    /// neutral scene does not model.
    pub fn topmost_toplevel(&self) -> Option<SurfaceId> {
        self.engine.scene.toplevels().max()
    }

    /// The toplevel at index `n` in ascending surface-id order (0 = earliest-mapped). `None` if `n` is
    /// out of range. Backs [`InputCommand::FocusToplevelIndex`] — a stable, inspectable way for a
    /// host/test to target a specific window in a multi-window stack (`toplevels()` is unordered, so it
    /// is sorted here).
    pub fn toplevel_at(&self, n: usize) -> Option<SurfaceId> {
        let mut tls: Vec<SurfaceId> = self.engine.scene.toplevels().collect();
        tls.sort();
        tls.get(n).copied()
    }

    /// The current-frame timestamp (ms) events are stamped with — the same host-monotonic clock the
    /// frame callbacks read, so input and frame time share one timeline.
    pub(super) fn input_time_ms(&self) -> u32 {
        (self.engine.clock().now_nanos() / 1_000_000) as u32
    }
}
