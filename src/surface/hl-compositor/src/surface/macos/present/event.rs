use super::*;

impl MacPresenter {
    pub(super) fn poll_native_events(&mut self) {
        let _span = hl_span!(tag::PRESENT, "macos_poll_events");
        let Some(mtm) = self.mtm else { return };
        let app = NSApplication::sharedApplication(mtm);
        // A nil `untilDate` tells AppKit to WAIT for the next event.  This method is called from the
        // Wayland/calloop serve loop and must only drain events already queued; blocking here prevents
        // subsequent client requests from being dispatched (a GTK client connects, then hangs before its
        // first surface can map). `distantPast` is AppKit's documented non-blocking poll deadline.
        let deadline = unsafe { NSDate::distantPast() };
        unsafe {
            while let Some(event) = app.nextEventMatchingMask_untilDate_inMode_dequeue(
                NSEventMask::Any,
                Some(&deadline),
                NSDefaultRunLoopMode,
                true,
            ) {
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
                            let point = event.locationInWindow();
                            let (x, y) = window.wayland_point(point.x, point.y);
                            let (x, y) = (x + input_origin.0, y + input_origin.1);
                            self.events.push(PresenterEvent::PointerMotion {
                                window: surface,
                                x,
                                y,
                            });
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
                                self.native_resize = None;
                                continue;
                            }
                            // Focus the native window that produced this event before routing its pointer
                            // coordinates; multi-window hit testing uses keyboard focus as its z-order key.
                            self.events.push(PresenterEvent::Focus(surface));
                            let point = event.locationInWindow();
                            let (x, y) = window.wayland_point(point.x, point.y);
                            let (x, y) = (x + input_origin.0, y + input_origin.1);
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
                        }
                        NSEventType::ScrollWheel => self.events.push(PresenterEvent::PointerAxis {
                            horizontal: -event.scrollingDeltaX(),
                            vertical: -event.scrollingDeltaY(),
                        }),
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
                if !consumed {
                    app.sendEvent(&event);
                }
            }
            // AppKit window operations are not event-only. Native full-screen transitions, animation,
            // notifications, and Space hand-off also run through main-run-loop sources and timers. Give
            // them a tightly bounded slice every compositor tick; without this `toggleFullScreen` creates
            // transition windows but never completes. One millisecond keeps Wayland input/render latency
            // bounded while allowing Cocoa to make forward progress.
            let until = NSDate::dateWithTimeIntervalSinceNow(0.001);
            NSRunLoop::mainRunLoop().runMode_beforeDate(NSDefaultRunLoopMode, &until);
        }
        for (&surface, state) in &mut self.surfaces {
            let Some(window) = state.window.as_ref() else {
                continue;
            };
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
            if state
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
