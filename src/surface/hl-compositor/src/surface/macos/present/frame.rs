use super::*;

impl MacPresenter {
    pub(super) fn compose_frame(
        &mut self,
        frame: &crate::scene::port::PresentFrame,
    ) -> Result<(u32, u32), String> {
        let Some(_root) = frame
            .layers
            .iter()
            .find(|layer| layer.image.surface == frame.role)
        else {
            return Err("frame has no layers".to_string());
        };
        for layer in &frame.layers {
            self.compose(&layer.image, &layer.damage)?;
        }
        let size = self
            .surfaces
            .get(&frame.role)
            .and_then(|state| state.composite.as_ref())
            .map(|(width, height, _)| (*width, *height))
            .ok_or_else(|| "role source composite is missing".to_string())?;
        let replace = self
            .surfaces
            .get(&frame.role)
            .and_then(|state| state.frame.as_ref())
            .is_none_or(|(width, height, _)| (*width, *height) != size);
        if replace {
            let target = self.ctx.new_bgra_texture(size.0, size.1);
            self.surfaces.get_mut(&frame.role).unwrap().frame = Some((size.0, size.1, target));
        }
        let scale = self.backing_scale_for(frame.role);
        for (index, layer) in frame.layers.iter().enumerate() {
            let source = self
                .surfaces
                .get(&layer.image.surface)
                .and_then(|state| state.composite.as_ref())
                .map(|(_, _, texture)| texture.clone())
                .ok_or_else(|| "layer composite is missing".to_string())?;
            let destination = self
                .surfaces
                .get(&frame.role)
                .and_then(|state| state.frame.as_ref())
                .map(|(_, _, texture)| texture.clone())
                .ok_or_else(|| "role composite is missing".to_string())?;
            let viewport = Rect::new(
                (f64::from(layer.x) * scale).round() as i32,
                (f64::from(layer.y) * scale).round() as i32,
                (f64::from(layer.image.width) * scale).round() as i32,
                (f64::from(layer.image.height) * scale).round() as i32,
            );
            self.ctx
                .compose_into(
                    &self.pipeline,
                    &source,
                    &destination,
                    [0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0],
                    layer.image.format.is_opaque(),
                    None,
                    Some(viewport),
                    index == 0,
                    self.mtm.is_none() && index + 1 == frame.layers.len(),
                )
                .map_err(str::to_owned)?;
        }
        Ok(size)
    }

    pub(super) fn capture_requested_frame(&self) {
        let Some(capture) = self
            .requested_capture
            .as_ref()
            .filter(|capture| capture.pending())
        else {
            return;
        };
        let visible = self.surfaces.iter().find_map(|(&sid, state)| {
            (state.composite.is_some()
                && state
                    .window
                    .as_ref()
                    .is_some_and(|window| window.is_visible()))
            .then_some(sid)
        });
        let Some(sid) = visible.or_else(|| {
            self.surfaces
                .iter()
                .find_map(|(&sid, state)| state.composite.as_ref().is_some().then_some(sid))
        }) else {
            return;
        };

        // A request can arrive after a static frame's asynchronous native present. Put a barrier behind
        // that already-submitted work before reading its shared texture from the CPU.
        self.ctx.wait_idle();
        match self
            .surfaces
            .get(&sid)
            .and_then(|state| state.content.as_ref())
        {
            Some(Content::IoSurface(surface)) => {
                let (source_width, source_height, _) = surface.dimensions();
                match surface.read_bgra() {
                    Ok(bytes) => {
                        let stats = PixelStats::from_bytes(&bytes);
                        hl_log::hl_log!(
                            tag::PRESENT,
                            Level::Warn,
                            "capture pixel boundary=source kind=iosurface sid={} iosurface={} size={}x{} bytes={} min={} max={} nonzero={}",
                            sid.0,
                            surface.id(),
                            source_width,
                            source_height,
                            bytes.len(),
                            stats.min,
                            stats.max,
                            stats.nonzero
                        );
                    }
                    Err(error) => hl_log::hl_log!(
                        tag::PRESENT,
                        Level::Warn,
                        "capture pixel boundary=source kind=iosurface sid={} iosurface={} size={}x{} read_error={error}",
                        sid.0,
                        surface.id(),
                        source_width,
                        source_height
                    ),
                }
            }
            Some(Content::Bgra { bgra, w, h }) => {
                let stats = PixelStats::from_bytes(bgra);
                hl_log::hl_log!(
                    tag::PRESENT,
                    Level::Warn,
                    "capture pixel boundary=source kind=shm sid={} size={}x{} bytes={} min={} max={} nonzero={}",
                    sid.0,
                    w,
                    h,
                    bgra.len(),
                    stats.min,
                    stats.max,
                    stats.nonzero
                );
            }
            None => {}
        }
        let Some((width, height, rgba)) = self.last_rgba(sid) else {
            return;
        };
        let stats = PixelStats::from_bytes(&rgba);
        hl_log::hl_log!(
            tag::PRESENT,
            Level::Warn,
            "capture pixel boundary=destination sid={} size={}x{} bytes={} min={} max={} nonzero={}",
            sid.0,
            width,
            height,
            rgba.len(),
            stats.min,
            stats.max,
            stats.nonzero
        );
        if capture.claim() {
            if let Err(error) = capture.write(sid, width, height, &rgba) {
                eprintln!("[macos-surface] requested capture sid={sid:?}: {error}");
            }
        }
    }

    pub(super) fn present_native(
        &mut self,
        _output: OutputId,
        image: &PresentableImage,
        damage: &[Rect],
        _timing: PresentTiming,
        compose: bool,
    ) -> PresentationFeedback {
        let _span = hl_span!(tag::PRESENT, "macos_present");
        let sid = image.surface;
        let (mut w, mut h) = match if compose {
            self.compose(image, damage)
        } else {
            self.surfaces
                .get(&image.surface)
                .and_then(|state| state.frame.as_ref())
                .map(|(width, height, _)| (*width, *height))
                .ok_or_else(|| "role composite is missing".to_string())
        } {
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
        let (shown, retry) = if let (Some(mtm), true) = (self.mtm, has_window_role) {
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
                    .filter(|state| {
                        state.window.is_some()
                            && state.desired.as_ref().is_some_and(|window| {
                                matches!(window.kind, WindowKind::Toplevel { parent: None })
                            })
                    })
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
            if let Some(origin) = popup_origin {
                if let Some(window) = self
                    .surfaces
                    .get(&sid)
                    .and_then(|state| state.window.as_ref())
                {
                    window.set_screen_origin(origin);
                }
            }
            let actual_scale = self
                .surfaces
                .get(&sid)
                .and_then(|state| state.window.as_ref())
                .map(MetalWindow::backing_scale)
                .unwrap_or(1.0);
            let scale_changed = self
                .surfaces
                .get_mut(&sid)
                .is_some_and(|state| state.observe_backing_scale(actual_scale));
            let expected_pixels = destination_pixels(visible_size, actual_scale);
            let size_changed = match expected_pixels {
                Ok(pixels) => pixels != (w, h),
                Err(_) => true,
            };
            if scale_changed || size_changed {
                if let Some(state) = self.surfaces.get_mut(&sid) {
                    state.native_submission = None;
                }
                if !compose {
                    return PresentationFeedback {
                        outcome: PresentOutcome::RetryableFailure,
                    };
                }
                match self.compose(image, damage) {
                    Ok(size) => (w, h) = size,
                    Err(error) => {
                        eprintln!(
                            "[macos-surface] present sid={sid:?} after backing-scale change: {error}"
                        );
                        return PresentationFeedback {
                            outcome: PresentOutcome::RetryableFailure,
                        };
                    }
                }
            }
            let st = self.surfaces.get_mut(&sid).unwrap();
            let win = st.window.as_ref().unwrap();
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
            let Some(composite) = st.frame.as_ref().map(|(_, _, texture)| texture.clone()) else {
                return PresentationFeedback {
                    outcome: PresentOutcome::RetryableFailure,
                };
            };
            let Some(next) = self.next_submission.checked_add(1) else {
                return PresentationFeedback {
                    outcome: PresentOutcome::TerminalFailure,
                };
            };
            let submission = PresentationId(self.next_submission);
            self.next_submission = next;
            match win.present(
                &self.ctx,
                &composite,
                st.native_presents.len(),
                submission,
                self.presentation_tx.clone(),
                self.wake.clone(),
            ) {
                PresentAttempt::Submitted(present) => {
                    st.native_presents.push_back(present);
                    self.present_surfaces.insert(submission, sid);
                    (Some(submission), false)
                }
                PresentAttempt::Retry => (None, true),
                PresentAttempt::Terminal => {
                    return PresentationFeedback {
                        outcome: PresentOutcome::TerminalFailure,
                    };
                }
            }
        } else {
            (None, false)
        };

        if retry {
            if !self.events.contains(&PresenterEvent::Repaint(sid)) {
                self.events.push(PresenterEvent::Repaint(sid));
            }
            return PresentationFeedback {
                outcome: PresentOutcome::RetryableFailure,
            };
        }

        if let Some(capture) = &self.capture {
            if let Some((capture_w, capture_h, rgba)) = self.last_rgba(sid) {
                if let Err(err) = capture.write(sid, capture_w, capture_h, &rgba) {
                    eprintln!("[macos-surface] capture sid={sid:?}: {err}");
                }
            }
        }
        self.capture_requested_frame();

        if let Some(id) = shown {
            PresentationFeedback {
                outcome: PresentOutcome::Pending { id },
            }
        } else {
            // Composited into the backing target but not visibly shown (headless, or window not yet on
            // screen). Honest `Offscreen` so the schedule service does not advance pacing as if it shipped.
            PresentationFeedback::offscreen()
        }
    }
}
