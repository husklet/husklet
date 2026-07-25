use super::*;

impl HlState {
    pub(super) fn close_toplevel(&mut self, surface: SurfaceId) {
        let Some(sid) = self.engine.scene.window_root(surface) else {
            return;
        };
        let Some(wl) = self.surfaces_by_id.get(&sid).cloned() else {
            return;
        };
        let toplevel = self
            .xdg_shell
            .toplevel_surfaces()
            .iter()
            .find(|t| t.wl_surface() == &wl)
            .cloned();
        if let Some(toplevel) = toplevel {
            hl_debug!(tag::WAYLAND, "toplevel close surf={}", sid.0);
            toplevel.send_close();
        }
    }

    // ------------------------------------ wl_touch ------------------------------------------
    //
    // The touch seam mirrors the pointer seam: each touch point (a "slot", keyed by the client-facing
    // touch id) hit-tests independently against the surface tree, and smithay's `TouchTarget` impl for
    // `WlSurface` serializes down/motion/up/frame/cancel. Multiple live ids coexist (real multi-touch);
    // the caller closes each atomic batch with `TouchFrame`.

    /// Deliver `wl_touch.down` for point `id` at root-local logical `(x, y)`: hit-test the surface under
    /// the point and hand smithay's `TouchHandle::down` the focus + surface-local coordinate.
    pub fn inject_touch_down(&mut self, id: i32, x: f64, y: f64) {
        hl_debug!(
            tag::WAYLAND,
            "input touch down id={} x={:.0} y={:.0}",
            id,
            x,
            y
        );
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        let focus = self.touch_focus(x, y);
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        touch.down(
            self,
            focus,
            &TouchDownEvent {
                slot: TouchSlot::from(Some(id as u32)),
                location: (x, y).into(),
                serial,
                time,
            },
        );
    }

    /// Deliver `wl_touch.motion` for point `id` at root-local logical `(x, y)`.
    pub fn inject_touch_motion(&mut self, id: i32, x: f64, y: f64) {
        hl_debug!(
            tag::WAYLAND,
            "input touch motion id={} x={:.0} y={:.0}",
            id,
            x,
            y
        );
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        let focus = self.touch_focus(x, y);
        let time = self.input_time_ms();
        touch.motion(
            self,
            focus,
            &TouchMotionEvent {
                slot: TouchSlot::from(Some(id as u32)),
                location: (x, y).into(),
                time,
            },
        );
    }

    /// Deliver `wl_touch.up` for point `id` (the finger lifted).
    pub fn inject_touch_up(&mut self, id: i32) {
        hl_debug!(tag::WAYLAND, "input touch up id={}", id);
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        touch.up(
            self,
            &TouchUpEvent {
                slot: TouchSlot::from(Some(id as u32)),
                serial,
                time,
            },
        );
    }

    /// Deliver `wl_touch.frame` — close the current atomic touch batch.
    pub fn inject_touch_frame(&mut self) {
        hl_debug!(tag::WAYLAND, "input touch frame");
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        touch.frame(self);
    }

    /// Deliver `wl_touch.cancel` — the compositor took the whole touch sequence over.
    pub fn inject_touch_cancel(&mut self) {
        hl_debug!(tag::WAYLAND, "input touch cancel");
        let Some(touch) = self.seat.get_touch() else {
            return;
        };
        touch.cancel(self);
    }

    /// The touch focus `(WlSurface, origin)` under root-local logical `(x, y)`, matching the pointer
    /// hit-test — the surface the touch point lands on and its origin in root space.
    pub(super) fn touch_focus(&self, x: f64, y: f64) -> Option<(WlSurface, Point<f64, Logical>)> {
        self.hit_test(x, y).and_then(|(_, sid, ox, oy)| {
            self.surfaces_by_id
                .get(&sid)
                .cloned()
                .map(|wl| (wl, Point::<f64, Logical>::from((ox as f64, oy as f64))))
        })
    }

    // ------------------------------ zwp_pointer_gestures_v1 ---------------------------------
    //
    // Trackpad pinch/swipe. These target the pointer's CURRENT focus (set by the last `PointerMotion`),
    // so a demo positions the pointer over the surface first, then drives the gesture. smithay routes each
    // to the focused surface's bound `zwp_pointer_gesture_{swipe,pinch}_v1`.

    /// `zwp_pointer_gesture_swipe_v1.begin` with `fingers` fingers.
    pub fn inject_gesture_swipe_begin(&mut self, fingers: u32) {
        hl_debug!(
            tag::WAYLAND,
            "input gesture swipe begin fingers={}",
            fingers
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        pointer.gesture_swipe_begin(
            self,
            &GestureSwipeBeginEvent {
                serial,
                time,
                fingers,
            },
        );
    }

    /// `zwp_pointer_gesture_swipe_v1.update` by logical center delta `(dx, dy)`.
    pub fn inject_gesture_swipe_update(&mut self, dx: f64, dy: f64) {
        hl_debug!(
            tag::WAYLAND,
            "input gesture swipe update dx={:.1} dy={:.1}",
            dx,
            dy
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let time = self.input_time_ms();
        pointer.gesture_swipe_update(
            self,
            &GestureSwipeUpdateEvent {
                time,
                delta: (dx, dy).into(),
            },
        );
    }

    /// `zwp_pointer_gesture_swipe_v1.end` (`cancelled` = aborted).
    pub fn inject_gesture_swipe_end(&mut self, cancelled: bool) {
        hl_debug!(
            tag::WAYLAND,
            "input gesture swipe end cancelled={}",
            cancelled
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        pointer.gesture_swipe_end(
            self,
            &GestureSwipeEndEvent {
                serial,
                time,
                cancelled,
            },
        );
    }

    /// `zwp_pointer_gesture_pinch_v1.begin` with `fingers` fingers.
    pub fn inject_gesture_pinch_begin(&mut self, fingers: u32) {
        hl_debug!(
            tag::WAYLAND,
            "input gesture pinch begin fingers={}",
            fingers
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        pointer.gesture_pinch_begin(
            self,
            &GesturePinchBeginEvent {
                serial,
                time,
                fingers,
            },
        );
    }

    /// `zwp_pointer_gesture_pinch_v1.update`: center delta `(dx, dy)`, absolute `scale`, `rotation` degrees.
    pub fn inject_gesture_pinch_update(&mut self, dx: f64, dy: f64, scale: f64, rotation: f64) {
        hl_debug!(
            tag::WAYLAND,
            "input gesture pinch update scale={:.2} rot={:.1}",
            scale,
            rotation
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let time = self.input_time_ms();
        pointer.gesture_pinch_update(
            self,
            &GesturePinchUpdateEvent {
                time,
                delta: (dx, dy).into(),
                scale,
                rotation,
            },
        );
    }

    /// `zwp_pointer_gesture_pinch_v1.end` (`cancelled` = aborted).
    pub fn inject_gesture_pinch_end(&mut self, cancelled: bool) {
        hl_debug!(
            tag::WAYLAND,
            "input gesture pinch end cancelled={}",
            cancelled
        );
        let Some(pointer) = self.seat.get_pointer() else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        pointer.gesture_pinch_end(
            self,
            &GesturePinchEndEvent {
                serial,
                time,
                cancelled,
            },
        );
    }

    // -------------------------------- zwp_tablet_tool_v2 ------------------------------------
    //
    // The pen. `proximity_in` hovers the tool over the surface under a point; `motion` (carrying a queued
    // `pressure`) tracks it; `tip_down`/`tip_up` are contact; `proximity_out` ends the hover. smithay
    // auto-frames each action and serializes the `zwp_tablet_tool_v2` wire events to the focused client.

    /// `zwp_tablet_tool_v2.proximity_in` for the surface under root-local logical `(x, y)`.
    pub fn inject_tablet_proximity_in(&mut self, x: f64, y: f64) {
        hl_debug!(
            tag::WAYLAND,
            "input tablet proximity_in x={:.0} y={:.0}",
            x,
            y
        );
        let Some((wl, origin)) = self.touch_focus(x, y) else {
            return;
        };
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        self.tablet_tool
            .proximity_in((x, y).into(), (wl, origin), &self.tablet, serial, time);
    }

    /// `zwp_tablet_tool_v2.motion` (+ queued `pressure`) at root-local logical `(x, y)`.
    pub fn inject_tablet_motion(&mut self, x: f64, y: f64, pressure: f64) {
        hl_debug!(
            tag::WAYLAND,
            "input tablet motion x={:.0} y={:.0} p={:.2}",
            x,
            y,
            pressure
        );
        let focus = self.touch_focus(x, y);
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        // Pressure is queued and shipped alongside the motion below.
        self.tablet_tool.pressure(pressure);
        self.tablet_tool
            .motion((x, y).into(), focus, &self.tablet, serial, time);
    }

    /// `zwp_tablet_tool_v2.down` — the tip made contact.
    pub fn inject_tablet_tip_down(&mut self) {
        hl_debug!(tag::WAYLAND, "input tablet tip_down");
        let serial = SERIAL_COUNTER.next_serial();
        let time = self.input_time_ms();
        self.tablet_tool.tip_down(serial, time);
    }

    /// `zwp_tablet_tool_v2.up` — the tip lifted.
    pub fn inject_tablet_tip_up(&mut self) {
        hl_debug!(tag::WAYLAND, "input tablet tip_up");
        let time = self.input_time_ms();
        self.tablet_tool.tip_up(time);
    }

    /// `zwp_tablet_tool_v2.proximity_out` — the pen left proximity.
    pub fn inject_tablet_proximity_out(&mut self) {
        hl_debug!(tag::WAYLAND, "input tablet proximity_out");
        let time = self.input_time_ms();
        self.tablet_tool.proximity_out(time);
    }

    // ---------------------------- ext_session_lock (screen lock) ---------------------------

    /// Host-initiated session lock (the symmetric seam to the client-driven `ext_session_lock`).
    pub fn lock_session(&mut self) {
        self.set_session_locked(true);
    }

    /// Host-initiated session unlock.
    pub fn unlock_session(&mut self) {
        self.set_session_locked(false);
    }

    /// Apply a lock/unlock transition: occlude (hide) or restore every NORMAL toplevel, mirror the state
    /// into [`Observations`], and — on unlock — mark the restored surfaces dirty + arm a repaint so they
    /// visibly return at the next refresh boundary even if their clients are idle. Lock surfaces (tracked in
    /// `lock_surfaces`) are never occluded: they are what stays on screen while locked.
    pub(super) fn set_session_locked(&mut self, locked: bool) {
        hl_info!(tag::WAYLAND, "session lock={}", locked);
        self.session_locked = locked;
        let vis = if locked {
            Visibility::Occluded
        } else {
            Visibility::Visible
        };
        let toplevels: Vec<SurfaceId> = self.engine.scene.toplevels().collect();
        for tl in toplevels {
            if self.lock_surfaces.contains(&tl) {
                continue;
            }
            self.engine.scene.set_visibility(tl, vis);
            self.reconcile_window(tl);
            if !locked {
                // Restore: force a fresh present so the unhidden surface reappears.
                self.engine.scene.mark_dirty(tl);
                self.arm_repaint(tl);
            }
        }
        self.observations.lock().unwrap().session_locked = locked;
    }
}

// ------------------------------------- protocol handlers -------------------------------------
