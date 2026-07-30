use super::*;
impl Clipboard for MacPresenter {
    fn set_clipboard_text(&mut self, text: &str) {
        let pasteboard = unsafe { NSPasteboard::generalPasteboard() };
        unsafe {
            pasteboard.clearContents();
            pasteboard.setString_forType(&NSString::from_str(text), NSPasteboardTypeString);
            self.pasteboard_change = pasteboard.changeCount();
        }
    }

    fn take_clipboard_text(&mut self) -> Option<String> {
        let pasteboard = unsafe { NSPasteboard::generalPasteboard() };
        let change = unsafe { pasteboard.changeCount() };
        if change == self.pasteboard_change {
            return None;
        }
        self.pasteboard_change = change;
        unsafe { pasteboard.stringForType(NSPasteboardTypeString) }.map(|text| text.to_string())
    }
}

impl Windows for MacPresenter {
    fn reconcile_window(&mut self, desired: &WindowState) {
        self.reconcile_native_window(desired);
    }

    fn destroy_window(&mut self, surface: SurfaceId) {
        self.destroy(surface);
    }

    /// Adopt the guest's cursor request. Headless there is no AppKit cursor to set, and setting one
    /// off the main thread is not allowed, so the request is dropped rather than half-applied.
    fn set_cursor(&mut self, cursor: &crate::scene::port::HostCursor) {
        if self.mtm.is_some() {
            self.cursor.request(cursor);
        }
    }

    fn begin_interaction(&mut self, surface: SurfaceId, interaction: WindowInteraction) {
        if interaction == WindowInteraction::Move {
            if let (Some(window), Some(event)) = (
                self.surfaces
                    .get(&surface)
                    .and_then(|state| state.window.as_ref()),
                self.drag_event.take(),
            ) {
                window.drag(&event);
            }
        }
    }
}

impl HostEvents for MacPresenter {
    fn set_wake(&mut self, wake: Arc<dyn Wake>) {
        self.wake = Some(wake);
    }

    fn poll_events(&mut self) {
        self.reap_native_presents();
        self.poll_native_events();
        self.capture_requested_frame();
    }

    fn take_events(&mut self) -> Vec<PresenterEvent> {
        std::mem::take(&mut self.events)
    }
}

impl Presenter for MacPresenter {
    fn present_frame(&mut self, frame: &crate::scene::port::PresentFrame) -> PresentationFeedback {
        let Some(root) = frame
            .layers
            .iter()
            .find(|layer| layer.image.surface == frame.role)
        else {
            return PresentationFeedback::offscreen();
        };
        if let Err(error) = self.compose_frame(frame) {
            hl_log::hl_error!(
                tag::PRESENT,
                "composite role={} error={error}",
                frame.role.0
            );
            return PresentationFeedback {
                outcome: PresentOutcome::RetryableFailure,
            };
        }
        self.present_native(frame.output, &root.image, &root.damage, frame.timing, false)
    }

    fn cancel(&mut self, role: SurfaceId) {
        self.present_surfaces.retain(|_, surface| *surface != role);
    }
}
