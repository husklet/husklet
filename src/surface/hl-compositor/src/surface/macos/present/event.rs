use super::*;

impl MacPresenter {
    pub(super) fn poll_native_events(&mut self) {
        // Momentum scrolling and pointer tracking can keep AppKit's queue non-empty indefinitely.
        // Bound each drain so Wayland clients, frame callbacks, and the translated events below are
        // dispatched before returning for another native batch.
        const EVENT_BUDGET: usize = 256;

        let _span = hl_span!(tag::PRESENT, "macos_poll_events");
        let Some(mtm) = self.mtm else { return };
        let app = NSApplication::sharedApplication(mtm);
        // A nil `untilDate` tells AppKit to WAIT for the next event.  This method is called from the
        // Wayland/calloop serve loop and must only drain events already queued; blocking here prevents
        // subsequent client requests from being dispatched (a GTK client connects, then hangs before its
        // first surface can map). `distantPast` is AppKit's documented non-blocking poll deadline.
        let deadline = unsafe { NSDate::distantPast() };
        unsafe {
            for _ in 0..EVENT_BUDGET {
                let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
                    NSEventMask::Any,
                    Some(&deadline),
                    NSDefaultRunLoopMode,
                    true,
                ) else {
                    break;
                };
                let mut consumed = false;
                let event_type = event.r#type();
                let window_number = event.windowNumber();
                let window = self.surfaces.iter().find_map(|(&surface, state)| {
                    state.window.as_ref().and_then(|window| {
                        (window.number() == window_number).then_some((
                            surface,
                            window,
                            state.input_origin,
                        ))
                    })
                });
                hl_log::hl_log!(
                    tag::PRESENT,
                    Level::Trace,
                    "native event type={event_type:?} window={window_number} matched={}",
                    window.is_some()
                );
                if let Some((surface, window, input_origin)) = window {
                    match event_type {
                        NSEventType::MouseMoved
                        | NSEventType::LeftMouseDragged
                        | NSEventType::RightMouseDragged
                        | NSEventType::OtherMouseDragged => {
                            if event_type == NSEventType::LeftMouseDragged {
                                if let Some((resize_surface, drag)) = &self.native_resize {
                                    if *resize_surface == surface {
                                        window.update_resize(drag);
                                        continue;
                                    }
                                }
                            }
                            // AppKit re-resolves the cursor as the pointer moves over a view that owns no
                            // cursor rect, so the guest's requested cursor is re-asserted here. This event
                            // belongs to one of our windows, which is exactly when the cursor is ours.
                            self.cursor.apply();
                            let point = event.locationInWindow();
                            let (x, y) = window.wayland_point(point.x, point.y);
                            let (x, y) = (x + input_origin.0, y + input_origin.1);
                            self.events.push(PresenterEvent::PointerMotion {
                                window: surface,
                                x,
                                y,
                            });
                            consumed = true;
                        }
                        NSEventType::LeftMouseDown
                        | NSEventType::LeftMouseUp
                        | NSEventType::RightMouseDown
                        | NSEventType::RightMouseUp
                        | NSEventType::OtherMouseDown
                        | NSEventType::OtherMouseUp => {
                            if event_type == NSEventType::LeftMouseDown {
                                if let Some(drag) = window.begin_resize(event.locationInWindow()) {
                                    self.native_resize = Some((surface, drag));
                                    continue;
                                }
                            }
                            if event_type == NSEventType::LeftMouseUp
                                && self
                                    .native_resize
                                    .as_ref()
                                    .is_some_and(|(resize_surface, _)| *resize_surface == surface)
                            {
                                if let Some((_, drag)) = &self.native_resize {
                                    window.update_resize(drag);
                                }
                                self.native_resize = None;
                                let (width, height) = window.logical_size();
                                let state = self.surfaces.get_mut(&surface).unwrap();
                                let maximized = state
                                    .desired
                                    .as_ref()
                                    .is_some_and(|desired| desired.maximized);
                                let fullscreen = state.observed_native_fullscreen.unwrap_or(false);
                                state.native_resize_pending =
                                    Some((width, height, maximized, fullscreen, false));
                                state.native_resize_changed_at = None;
                                state.native_resize_sent_at = None;
                                state.native_resize_last_sent =
                                    Some((width, height, maximized, fullscreen, false));
                                self.events.push(PresenterEvent::Resize {
                                    surface,
                                    width,
                                    height,
                                    maximized,
                                    fullscreen,
                                    resizing: false,
                                });
                                self.events.push(PresenterEvent::ResizeEnd { surface });
                                continue;
                            }
                            // Focus the native window that produced this event before routing its pointer
                            // coordinates; multi-window hit testing uses keyboard focus as its z-order key.
                            self.events.push(PresenterEvent::Focus(surface));
                            let point = event.locationInWindow();
                            let (x, y) = window.wayland_point(point.x, point.y);
                            let (x, y) = (x + input_origin.0, y + input_origin.1);
                            hl_log::hl_log!(
                                tag::PRESENT,
                                Level::Trace,
                                "pointer button surface={} native=({:.1},{:.1}) origin=({:.1},{:.1}) wayland=({x:.1},{y:.1})",
                                surface.0,
                                point.x,
                                point.y,
                                input_origin.0,
                                input_origin.1,
                            );
                            self.events.push(PresenterEvent::PointerMotion {
                                window: surface,
                                x,
                                y,
                            });
                            let pressed = matches!(
                                event_type,
                                NSEventType::LeftMouseDown
                                    | NSEventType::RightMouseDown
                                    | NSEventType::OtherMouseDown
                            );
                            if pressed && event_type == NSEventType::LeftMouseDown {
                                self.drag_event = Some(event.clone());
                            }
                            let button = 0x110 + event.buttonNumber().max(0) as u32;
                            self.events.push(PresenterEvent::PointerButton {
                                window: surface,
                                button,
                                pressed,
                                click_count: event.clickCount().clamp(1, u8::MAX as isize) as u8,
                            });
                            if !pressed {
                                self.drag_event = None;
                            }
                            consumed = true;
                        }
                        NSEventType::ScrollWheel => {
                            // Precise deltas mean a trackpad: a FINGER-source scroll, which `wl_pointer`
                            // requires to end with an `axis_stop` (AppKit reports that as the ended /
                            // cancelled phase). A classic wheel instead reports line-quantized
                            // `deltaX`/`deltaY`, one detent per line — that is the `axis_value120` step
                            // count a client needs to scroll by notches rather than by pixels.
                            let (horizontal, vertical) =
                                (-event.scrollingDeltaX(), -event.scrollingDeltaY());
                            if event.hasPreciseScrollingDeltas() {
                                if horizontal != 0.0 || vertical != 0.0 {
                                    self.events.push(PresenterEvent::PointerAxis {
                                        horizontal,
                                        vertical,
                                        source: ScrollSource::Finger,
                                        h120: 0,
                                        v120: 0,
                                    });
                                }
                                if event
                                    .phase()
                                    .intersects(NSEventPhase::Ended | NSEventPhase::Cancelled)
                                {
                                    self.events.push(PresenterEvent::PointerAxisStop {
                                        horizontal: true,
                                        vertical: true,
                                    });
                                }
                            } else {
                                self.events.push(PresenterEvent::PointerAxis {
                                    horizontal,
                                    vertical,
                                    source: ScrollSource::Wheel,
                                    h120: (-event.deltaX() * 120.0).round() as i32,
                                    v120: (-event.deltaY() * 120.0).round() as i32,
                                });
                            }
                            consumed = true;
                        }
                        NSEventType::KeyDown | NSEventType::KeyUp => {
                            self.sync_key_modifiers(event.modifierFlags());
                            if let Some(event) = KeyCode::from(event.keyCode()).event(
                                event_type == NSEventType::KeyDown,
                                event_type == NSEventType::KeyDown && event.isARepeat(),
                            ) {
                                self.events.push(event);
                            }
                            consumed = true;
                        }
                        NSEventType::FlagsChanged => {
                            self.sync_key_modifiers(event.modifierFlags());
                            consumed = true;
                        }
                        NSEventType::Swipe => {
                            self.events.extend(self.gestures.swipe(
                                event.phase(),
                                event.deltaX(),
                                event.deltaY(),
                            ));
                            consumed = true;
                        }
                        NSEventType::Magnify => {
                            self.events.extend(self.gestures.pinch(
                                event.phase(),
                                event.deltaX(),
                                event.deltaY(),
                                event.magnification(),
                                0.0,
                            ));
                            consumed = true;
                        }
                        NSEventType::Rotate => {
                            self.events.extend(self.gestures.pinch(
                                event.phase(),
                                event.deltaX(),
                                event.deltaY(),
                                0.0,
                                f64::from(event.rotation()),
                            ));
                            consumed = true;
                        }
                        NSEventType::EndGesture => {
                            self.events.extend(self.gestures.end(false));
                            consumed = true;
                        }
                        NSEventType::TabletProximity => {
                            let point = event.locationInWindow();
                            let (x, y) = window.wayland_point(point.x, point.y);
                            self.events.extend(self.tablet.proximity(
                                event.isEnteringProximity(),
                                x + input_origin.0,
                                y + input_origin.1,
                            ));
                            consumed = true;
                        }
                        NSEventType::TabletPoint => {
                            let point = event.locationInWindow();
                            let (x, y) = window.wayland_point(point.x, point.y);
                            self.events.extend(self.tablet.point(
                                x + input_origin.0,
                                y + input_origin.1,
                                f64::from(event.pressure()).clamp(0.0, 1.0),
                            ));
                            consumed = true;
                        }
                        _ => {}
                    }
                }
                if consumed {
                    // Report how long this event waited between AppKit stamping it and becoming a seat
                    // event. Only translated events are measured; AppKit's own events never reach a guest.
                    let kind = match event_type {
                        NSEventType::MouseMoved
                        | NSEventType::LeftMouseDragged
                        | NSEventType::RightMouseDragged
                        | NSEventType::OtherMouseDragged => "pointer_motion",
                        NSEventType::LeftMouseDown
                        | NSEventType::LeftMouseUp
                        | NSEventType::RightMouseDown
                        | NSEventType::RightMouseUp
                        | NSEventType::OtherMouseDown
                        | NSEventType::OtherMouseUp => "pointer_button",
                        NSEventType::ScrollWheel => "pointer_axis",
                        NSEventType::KeyDown | NSEventType::KeyUp => "key",
                        NSEventType::FlagsChanged => "modifiers",
                        NSEventType::TabletPoint | NSEventType::TabletProximity => "tablet",
                        _ => "gesture",
                    };
                    HostInput::stamped(event.timestamp()).dispatched(kind);
                } else {
                    app.sendEvent(&event);
                }
            }
            // `NSRunLoop::runMode` dispatches queued mouse/key events through AppKit itself. Our plain
            // Metal view does not own those events, so running it every tick races the manual drain above
            // and turns the visible Wayland surface into an unresponsive image. AppKit needs a run-loop
            // slice only while `toggleFullScreen` is performing its asynchronous Space transition.
            let fullscreen_transition = self.surfaces.values().any(|state| {
                let Some(window) = state.window.as_ref() else {
                    return false;
                };
                state
                    .desired
                    .as_ref()
                    .is_some_and(|desired| desired.fullscreen != window.native_fullscreen())
            });
            if fullscreen_transition {
                let until = NSDate::dateWithTimeIntervalSinceNow(0.001);
                NSRunLoop::mainRunLoop().runMode_beforeDate(NSDefaultRunLoopMode, &until);
            }
        }
        for (&surface, state) in &mut self.surfaces {
            let Some(backing_scale) = state.window.as_ref().map(MetalWindow::backing_scale) else {
                continue;
            };
            if state.observe_backing_scale(backing_scale) {
                hl_debug!(
                    tag::PRESENT,
                    "native backing scale changed surface={} scale={backing_scale}",
                    surface.0
                );
                self.events.push(PresenterEvent::Repaint(surface));
            }
            let window = state.window.as_ref().expect("window checked above");
            if !matches!(
                state.desired.as_ref().map(|window| window.kind),
                Some(WindowKind::Toplevel { .. })
            ) {
                continue;
            }
            let size = window.logical_size();
            let native_fullscreen = window.native_fullscreen();
            let fullscreen_changed = state
                .observed_native_fullscreen
                .replace(native_fullscreen)
                .is_some_and(|previous| previous != native_fullscreen);
            if state.reported_native_size.is_none() {
                state.reported_native_size = Some(size);
            } else if state.reported_native_size != Some(size) {
                hl_debug!(
                    tag::PRESENT,
                    "native frame changed surface={} size={}x{}",
                    surface.0,
                    size.0,
                    size.1
                );
                state.reported_native_size = Some(size);
                // While entering full-screen AppKit briefly reports intermediate frames before the
                // FullScreen style becomes authoritative. The client already has its XDG configure;
                // do not turn those animation frames into contradictory windowed configures.
                if state
                    .desired
                    .as_ref()
                    .is_some_and(|desired| desired.fullscreen)
                    && !native_fullscreen
                    && !fullscreen_changed
                {
                    continue;
                }
                let live_resize = self
                    .native_resize
                    .as_ref()
                    .is_some_and(|(resize_surface, _)| *resize_surface == surface);
                let maximized = !live_resize
                    && !native_fullscreen
                    && state
                        .desired
                        .as_ref()
                        .is_some_and(|desired| desired.maximized);
                state.native_resize_pending =
                    Some((size.0, size.1, maximized, native_fullscreen, live_resize));
                state.native_resize_changed_at = Some(Instant::now());
            } else if fullscreen_changed {
                // A native full-screen exit can finish without a final size delta. Still acknowledge
                // the mode change so stale XDG state cannot request full-screen again.
                state.native_resize_pending =
                    Some((size.0, size.1, false, native_fullscreen, false));
                state.native_resize_changed_at = Some(Instant::now());
            }
        }
        let now = Instant::now();
        for (&surface, state) in &mut self.surfaces {
            // Coalesce native geometry changes to at most one configure per display frame. Always keep
            // `native_resize_pending` at the newest AppKit size; XDG permits clients to acknowledge the
            // latest configure directly, and rendering superseded sizes creates seconds of resize debt.
            if let Some((width, height, maximized, fullscreen, resizing)) =
                state.native_resize_pending
            {
                let due = state
                    .native_resize_sent_at
                    .is_none_or(|sent| now.duration_since(sent) >= Duration::from_millis(8));
                if due
                    && state.native_resize_last_sent
                        != Some((width, height, maximized, fullscreen, resizing))
                {
                    state.native_resize_sent_at = Some(now);
                    state.native_resize_last_sent =
                        Some((width, height, maximized, fullscreen, resizing));
                    self.events.push(PresenterEvent::Resize {
                        surface,
                        width,
                        height,
                        maximized,
                        fullscreen,
                        resizing,
                    });
                }
            }
            if self
                .native_resize
                .as_ref()
                .is_none_or(|(resize_surface, _)| *resize_surface != surface)
                && state
                    .native_resize_changed_at
                    .is_some_and(|changed| now.duration_since(changed) >= Duration::from_millis(75))
            {
                state.native_resize_changed_at = None;
                // If the drag ended between pacing slots, emit its exact final size before clearing the
                // XDG resizing state. This prevents the last few pixels from arriving after ResizeEnd.
                if let Some((width, height, maximized, fullscreen, resizing)) =
                    state.native_resize_pending
                {
                    if state.native_resize_last_sent
                        != Some((width, height, maximized, fullscreen, resizing))
                    {
                        self.events.push(PresenterEvent::Resize {
                            surface,
                            width,
                            height,
                            maximized,
                            fullscreen,
                            resizing,
                        });
                    }
                }
                state.native_resize_sent_at = None;
                state.native_resize_last_sent = None;
                self.events.push(PresenterEvent::ResizeEnd { surface });
            }
        }
    }
}
