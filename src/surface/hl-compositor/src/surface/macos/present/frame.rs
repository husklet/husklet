use super::*;

impl MacPresenter {
    pub(super) fn present_native(
        &mut self,
        _output: OutputId,
        image: &PresentableImage,
        _damage: &[Rect],
        _timing: PresentTiming,
    ) -> PresentationFeedback {
        let _span = hl_span!(tag::PRESENT, "macos_present");
        let sid = image.surface;
        let (w, h) = match self.compose(image) {
            Ok(dims) => dims,
            Err(err) => {
                eprintln!("[macos-surface] present sid={sid:?}: {err}");
                // No content / unresolvable device resource: retryable — the compositor keeps pacing so a
                // re-attached buffer next cycle can succeed.
                return PresentationFeedback {
                    outcome: PresentOutcome::RetryableFailure,
                };
            }
        };
        self.frames += 1;

        // Windowed mode: open the window lazily (sized to the image's logical points), size its drawable
        // to the composite's device pixels, and blit. If no window/session is up the frame stays offscreen.
        let has_window_role = self
            .surfaces
            .get(&sid)
            .is_some_and(|state| state.desired.is_some());
        let shown = if let (Some(mtm), true) = (self.mtm, has_window_role) {
            let desired = self
                .surfaces
                .get(&sid)
                .and_then(|state| state.desired.clone());
            let popup = desired.as_ref().and_then(|window| match window.kind {
                WindowKind::Popup { parent, position } => Some((parent, position)),
                WindowKind::Toplevel { .. } => None,
            });
            let transient_parent = desired.as_ref().and_then(|window| match window.kind {
                WindowKind::Toplevel { parent } => parent,
                WindowKind::Popup { .. } => None,
            });
            let content_origin = desired
                .as_ref()
                .and_then(|window| window.geometry)
                .map(|geometry| (f64::from(geometry.x), f64::from(geometry.y)))
                .unwrap_or((0.0, 0.0));
            let visible_size = desired
                .as_ref()
                .and_then(|window| {
                    window
                        .geometry
                        .map(|geometry| (geometry.w, geometry.h))
                        .or(window.logical_size)
                })
                .unwrap_or((image.width, image.height));
            let popup_origin = popup.and_then(|(parent, (x, y))| {
                self.surfaces
                    .get(&parent)
                    .and_then(|parent| parent.window.as_ref())
                    .map(|parent| parent.popup_origin(x, y, visible_size.1.max(1) as u32))
            });
            let root_origin = popup.map_or((0.0, 0.0), |(parent_id, (x, y))| {
                let parent = self
                    .surfaces
                    .get(&parent_id)
                    .map(|state| state.input_origin)
                    .unwrap_or((0.0, 0.0));
                (parent.0 + f64::from(x), parent.1 + f64::from(y))
            });
            let input_origin = (
                root_origin.0 + content_origin.0,
                root_origin.1 + content_origin.1,
            );
            let toplevel_index = if popup.is_none() && transient_parent.is_none() {
                self.surfaces
                    .values()
                    .filter(|state| state.window.is_some() && state.input_origin == (0.0, 0.0))
                    .count()
            } else {
                0
            };
            let created = {
                let st = self.surfaces.get_mut(&sid).unwrap();
                st.input_origin = input_origin;
                if st.window.is_none() {
                    let title = if desired
                        .as_ref()
                        .is_none_or(|window| window.title.is_empty())
                    {
                        format!("hl surface {}", sid.0)
                    } else {
                        desired.as_ref().unwrap().title.clone()
                    };
                    let window = if popup.is_some() {
                        MetalWindow::new_popup(
                            mtm,
                            &self.ctx,
                            visible_size.0.max(1) as u32,
                            visible_size.1.max(1) as u32,
                            &title,
                        )
                    } else {
                        MetalWindow::new(
                            mtm,
                            &self.ctx,
                            visible_size.0.max(1) as u32,
                            visible_size.1.max(1) as u32,
                            &title,
                        )
                    };
                    window.set_size_constraints(
                        desired.as_ref().map_or((None, None), |w| w.min_size),
                        desired.as_ref().map_or((None, None), |w| w.max_size),
                    );
                    if let Some(desired) = desired.as_ref() {
                        window.set_mode(desired.maximized, desired.fullscreen);
                    }
                    let visibility = desired
                        .as_ref()
                        .map_or(Visibility::Visible, |window| window.visibility);
                    if visibility != Visibility::Visible {
                        window.set_visibility(visibility);
                    }
                    if popup.is_none() && transient_parent.is_none() {
                        window.cascade(toplevel_index);
                    }
                    st.window = Some(window);
                    true
                } else {
                    false
                }
            };
            if created {
                let parent = popup.map(|(parent, _)| parent).or(transient_parent);
                if let Some(parent) = parent {
                    if let (Some(parent), Some(child)) = (
                        self.surfaces
                            .get(&parent)
                            .and_then(|state| state.window.as_ref()),
                        self.surfaces
                            .get(&sid)
                            .and_then(|state| state.window.as_ref()),
                    ) {
                        parent.add_child(child);
                    }
                }
            }
            let st = self.surfaces.get_mut(&sid).unwrap();
            let win = st.window.as_ref().unwrap();
            if let Some(origin) = popup_origin {
                win.set_screen_origin(origin);
            }
            let desired_native_size = (visible_size.0.max(1) as u32, visible_size.1.max(1) as u32);
            match st.native_resize_pending {
                Some((width, height, _, _, _)) if (width, height) != desired_native_size => {
                    // AppKit owns the live bounds until the client commits the configure. Contents gravity
                    // preserves the old drawable's aspect instead of stretching it in the interim.
                }
                _ => {
                    st.native_resize_pending = None;
                    st.native_resize_last_sent = None;
                    win.set_logical_size(desired_native_size.0, desired_native_size.1);
                    st.reported_native_size = Some(desired_native_size);
                }
            }
            win.set_drawable_size(w, h);
            let composite = st.composite.as_ref().unwrap().2.clone();
            win.present(&self.ctx, &composite)
        } else {
            false
        };

        if let Some(capture) = &self.capture {
            if let Some((capture_w, capture_h, rgba)) = self.last_rgba(sid) {
                if let Err(err) = capture.write(sid, capture_w, capture_h, &rgba) {
                    eprintln!("[macos-surface] capture sid={sid:?}: {err}");
                }
            }
        }

        if shown {
            self.serial += 1;
            PresentationFeedback::delivered(self.serial, None)
        } else {
            // Composited into the backing target but not visibly shown (headless, or window not yet on
            // screen). Honest `Offscreen` so the schedule service does not advance pacing as if it shipped.
            PresentationFeedback::offscreen()
        }
    }
}
