use super::*;

impl MacPresenter {
    pub(super) fn compose(
        &mut self,
        image: &PresentableImage,
        damage: &[Rect],
    ) -> Result<(u32, u32), String> {
        let sid = image.surface;
        let format = image.format;
        let backing_scale = self.backing_scale_for(sid);
        let capture_pending = self
            .requested_capture
            .as_ref()
            .is_some_and(Capture::pending);
        let st = self.surfaces.get_mut(&sid).ok_or("no state for surface")?;
        let content = st.content.as_ref().ok_or("no content attached")?;

        // Resolve the source texture (and its device-pixel size) from the attached content.
        let native = matches!(content, Content::IoSurface(_));
        let (src, w, h) = match content {
            Content::Bgra { bgra, w, h } => {
                let expected = usize::try_from(*w)
                    .ok()
                    .and_then(|width| {
                        usize::try_from(*h)
                            .ok()
                            .and_then(|height| width.checked_mul(height))
                    })
                    .and_then(|pixels| pixels.checked_mul(4))
                    .ok_or("BGRA dimensions overflow address space")?;
                if *w == 0 || *h == 0 {
                    return Err("BGRA content has a zero dimension".to_string());
                }
                if bgra.len() != expected {
                    return Err(format!(
                        "BGRA content length {} does not match {}x{}x4 ({expected})",
                        bgra.len(),
                        w,
                        h
                    ));
                }
                if st
                    .uploads
                    .first()
                    .is_some_and(|(uw, uh, _)| (*uw, *uh) != (*w, *h))
                {
                    st.uploads.clear();
                    st.upload_cursor = 0;
                }
                let tex = if st.uploads.len() < 3 {
                    let tex = self.ctx.upload_bgra(bgra, *w, *h);
                    st.uploads.push((*w, *h, tex.clone()));
                    tex
                } else {
                    let index = st.upload_cursor % st.uploads.len();
                    let texture = st.uploads[index].2.clone();
                    // Damage is relative to the immediately preceding client buffer, but this rotating
                    // texture last held the frame from three commits ago. Applying only the latest damage
                    // would leave intervening changes stale. Upload the complete current plane into only
                    // the selected slot: one coherent upload rather than three uploads every frame.
                    self.ctx.update_bgra(&texture, bgra, *w, *h);
                    texture
                };
                st.upload_cursor = (st.upload_cursor + 1) % 3;
                (tex, *w, *h)
            }
            Content::IoSurface(surface) => {
                let (sw, sh, _) = surface.dimensions();
                if capture_pending {
                    match surface.read_bgra() {
                        Ok(bytes) => {
                            let stats = PixelStats::from_bytes(&bytes);
                            hl_log::hl_log!(
                                tag::PRESENT,
                                Level::Warn,
                                "capture pixel boundary=source sid={} iosurface={} size={}x{} bytes={} min={} max={} nonzero={}",
                                sid.0,
                                surface.id(),
                                sw,
                                sh,
                                bytes.len(),
                                stats.min,
                                stats.max,
                                stats.nonzero
                            );
                        }
                        Err(error) => hl_log::hl_log!(
                            tag::PRESENT,
                            Level::Warn,
                            "capture pixel boundary=source sid={} iosurface={} size={}x{} read_error={error}",
                            sid.0,
                            surface.id(),
                            sw,
                            sh
                        ),
                    }
                }
                let tex = self
                    .ctx
                    .texture_from_iosurface(surface, sw as u32, sh as u32)?;
                (tex, sw as u32, sh as u32)
            }
        };

        // Start with wp_viewport's buffer-pixel source rectangle, then map xdg window geometry from
        // logical surface coordinates into that rectangle. This composition is essential on Retina GTK:
        // its dst-only viewport describes the complete 2x buffer while xdg geometry removes client-side
        // shadow margins. Ignoring either contract leaves a non-integral drawable that AppKit resamples.
        let crop = st
            .desired
            .as_ref()
            .and_then(|window| window.geometry.zip(window.logical_size))
            .and_then(|(geometry, logical)| {
                if geometry.w <= 0 || geometry.h <= 0 || logical.0 <= 0 || logical.1 <= 0 {
                    return None;
                }
                let (surface_w, surface_h) = image.transform.surface_size(w as i32, h as i32);
                let (base_x, base_y, base_w, base_h) =
                    image
                        .present_crop
                        .unwrap_or((0.0, 0.0, surface_w as f64, surface_h as f64));
                if base_w <= 0.0 || base_h <= 0.0 {
                    return None;
                }
                let x0 = (base_x + geometry.x as f64 * base_w / logical.0 as f64)
                    .round()
                    .clamp(0.0, surface_w as f64) as u32;
                let y0 = (base_y + geometry.y as f64 * base_h / logical.1 as f64)
                    .round()
                    .clamp(0.0, surface_h as f64) as u32;
                let x1 = (base_x
                    + (f64::from(geometry.x) + f64::from(geometry.w)) * base_w
                        / f64::from(logical.0))
                .round()
                .clamp(0.0, surface_w as f64) as u32;
                let y1 = (base_y
                    + (f64::from(geometry.y) + f64::from(geometry.h)) * base_h
                        / f64::from(logical.1))
                .round()
                .clamp(0.0, surface_h as f64) as u32;
                (x1 > x0 && y1 > y0).then_some((x0, y0, x1 - x0, y1 - y0))
            });
        let sampling = Sampling::new(
            image.transform,
            (w, h),
            crop.map(|(x, y, width, height)| (x as f64, y as f64, width as f64, height as f64))
                .or(image.present_crop),
        );
        let destination = st
            .desired
            .as_ref()
            .and_then(|window| window.geometry)
            .map(|geometry| (geometry.w, geometry.h))
            .unwrap_or((image.width, image.height));
        let (out_w, out_h) = destination_pixels(destination, backing_scale)?;
        let damage_origin = st
            .desired
            .as_ref()
            .map(|window| {
                let geometry =
                    window
                        .geometry
                        .unwrap_or(Rect::new(0, 0, destination.0, destination.1));
                match window.kind {
                    WindowKind::Popup { position, .. } => (
                        position.0.saturating_add(geometry.x),
                        position.1.saturating_add(geometry.y),
                    ),
                    WindowKind::Toplevel { .. } => (geometry.x, geometry.y),
                }
            })
            .unwrap_or((0, 0));
        let mut damage = Damage::map(damage, damage_origin, destination, (out_w, out_h));
        let uv = sampling.map;
        let key = PresentationKey::new(
            image,
            crop.map(|(x, y, width, height)| (x as f64, y as f64, width as f64, height as f64))
                .or(image.present_crop),
            out_w,
            out_h,
        );
        if native && st.native_submission == Some(key) {
            return st
                .composite
                .as_ref()
                .map(|(width, height, _)| (*width, *height))
                .ok_or_else(|| "native content submitted without a composite".to_string());
        }
        // Reuse a persistent composite target sized to the destination. The source crop controls only
        // sampling; wp_viewport's destination size and the native backing scale control rasterization.
        let need_new = match &self.surfaces.get(&sid).unwrap().composite {
            Some((cw, ch, _)) => *cw != out_w || *ch != out_h,
            None => true,
        };
        if need_new {
            let tex = self.ctx.new_bgra_texture(out_w, out_h);
            self.surfaces.get_mut(&sid).unwrap().composite = Some((out_w, out_h, tex));
            damage = Damage::Full;
        }
        let dst = self
            .surfaces
            .get(&sid)
            .unwrap()
            .composite
            .as_ref()
            .unwrap()
            .2
            .clone();
        self.ctx.compose_into(
            &self.pipeline,
            &src,
            &dst,
            uv,
            format.is_opaque(),
            damage.regions(),
            None,
            false,
            // A native present enqueues its drawable blit on the same Metal command queue immediately
            // after this render, so queue ordering is the required synchronization and a CPU fence only
            // adds latency. Offscreen verification and diagnostic capture read pixels synchronously and
            // therefore still require completion here.
            self.mtm.is_none()
                || self.capture.is_some()
                || self
                    .requested_capture
                    .as_ref()
                    .is_some_and(Capture::pending),
        )?;
        if native {
            let state = self.surfaces.get_mut(&sid).unwrap();
            state.native_submission = Some(key);
        }
        if capture_pending {
            let bytes = self.ctx.readback_bgra(&dst, out_w, out_h);
            let stats = PixelStats::from_bytes(&bytes);
            hl_log::hl_log!(
                tag::PRESENT,
                Level::Warn,
                "capture pixel boundary=destination sid={} size={}x{} bytes={} min={} max={} nonzero={}",
                sid.0,
                out_w,
                out_h,
                bytes.len(),
                stats.min,
                stats.max,
                stats.nonzero
            );
        }
        Ok((out_w, out_h))
    }
}
