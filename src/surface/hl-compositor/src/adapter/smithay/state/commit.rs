use super::*;
use super::{buffer::BufferReader, output::Region};

impl HlState {
    /// Translates committed Smithay state into the neutral scene and drives presentation/frame pacing.
    pub(super) fn on_commit(&mut self, surface: &WlSurface) {
        let Some(sid) = self.sid(surface) else {
            return;
        };

        // Mirror Smithay's just-applied subsurface state (set_position offset, sync/desync, and the
        // place_above/place_below z-order) into the scene BEFORE the engine composes/paces this commit: a
        // parent commit atomically applies its synchronized children's buffered state, so refresh the whole
        // committed subtree, not just this surface. Without this the scene would composite a subsurface at a
        // stale offset (or present a sync child that should ship with its parent).
        self.sync_subsurface_tree(surface);

        // Record the surface's just-committed `wp_content_type_v1` hint (double-buffered like the buffer /
        // damage, applied at commit) into the shared observations. There is no reply event, so this is the
        // only way a test can assert the compositor read the exact hint the client set.
        self.record_content_type(surface);

        // Record the surface's just-committed `wp_tearing_control_v1` presentation hint (also double-buffered
        // and applied at commit) into the shared observations — the present path's honest read of whether the
        // client asked for `async` (tearing-allowed) vs `vsync` present.
        self.record_tearing_hint(surface);

        // Snapshot the committed state Smithay applied, taking ownership of the buffer assignment and
        // draining this commit's damage + frame callbacks (the compositor is expected to consume both).
        let (
            assignment,
            damage,
            scale,
            transform,
            frame_callbacks,
            viewport,
            feedbacks,
            input_region,
            opaque_region,
            buffer_damage,
            window_geometry,
            min_size,
            max_size,
        ) = with_states(surface, |states| {
            // Drain this commit's `wp_presentation_feedback` callbacks (double-buffered like the frame
            // callbacks): held until the frame they belong to actually presents, then answered
            // `presented`/`discarded` per the pacing outcome below.
            let feedbacks = std::mem::take(
                &mut states
                    .cached_state
                    .get::<PresentationFeedbackCachedState>()
                    .current()
                    .callbacks,
            );
            let mut attrs = states.cached_state.get::<SurfaceAttributes>();
            let cur = attrs.current();
            let assignment = cur.buffer.take();
            let committed_damage = std::mem::take(&mut cur.damage);
            let buffer_damage = committed_damage
                .iter()
                .map(|damage| match damage {
                    Damage::Buffer(r) => Some(Rect::new(r.loc.x, r.loc.y, r.size.w, r.size.h)),
                    Damage::Surface(_) => None,
                })
                .collect::<Option<Vec<_>>>();
            let damage: Vec<Rect> = committed_damage
                .iter()
                .map(|d| match d {
                    Damage::Surface(r) => Rect::new(r.loc.x, r.loc.y, r.size.w, r.size.h),
                    Damage::Buffer(r) => Rect::new(r.loc.x, r.loc.y, r.size.w, r.size.h),
                })
                .collect();
            let scale = cur.buffer_scale.max(1);
            // `wl_surface.set_buffer_transform` (double-buffered) — the rotation/flip the presenter applies
            // to the buffer so it displays upright. Always re-read so a reverted transform reverts too.
            let transform = BufferTransform::from(cur.buffer_transform);
            let callbacks = std::mem::take(&mut cur.frame_callbacks);
            // `wl_surface.set_input_region` / `set_opaque_region` (both double-buffered, applied at commit).
            // The neutral scene models each as a single logical `Rect` and USES them: the input region gates
            // pointer hit-testing (`surface_at` → `accepts_input_at`), and the opaque region drives the
            // occlusion present-skip (`is_tree_dirty` → `opaque_covers`). Re-read every commit (like the
            // buffer transform / viewport) so a client that CLEARS its region reverts to the default.
            let input_region = Region::new(&cur.input_region).input();
            let opaque_region = Region::new(&cur.opaque_region).opaque();
            drop(attrs);
            // The just-applied `wp_viewport` state (src crop in logical coords, dst logical size), mirrored
            // into the neutral scene so it resolves the on-screen logical size and the presenter samples the
            // cropped+scaled region. Always re-read (double-buffered) so a cleared viewport reverts too.
            let mut vp = states.cached_state.get::<ViewportCachedState>();
            let cur_vp = vp.current();
            let viewport = Viewport {
                src: cur_vp.src.map(|r| (r.loc.x, r.loc.y, r.size.w, r.size.h)),
                dst: cur_vp.dst.map(|s| (s.w, s.h)),
            };
            let mut xdg = states.cached_state.get::<XdgSurfaceCachedState>();
            let current_xdg = xdg.current();
            let window_geometry = current_xdg.geometry.map(|geometry| {
                Rect::new(
                    geometry.loc.x,
                    geometry.loc.y,
                    geometry.size.w,
                    geometry.size.h,
                )
            });
            let min_size = (
                (current_xdg.min_size.w > 0).then_some(current_xdg.min_size.w),
                (current_xdg.min_size.h > 0).then_some(current_xdg.min_size.h),
            );
            let max_size = (
                (current_xdg.max_size.w > 0).then_some(current_xdg.max_size.w),
                (current_xdg.max_size.h > 0).then_some(current_xdg.max_size.h),
            );
            (
                assignment,
                damage,
                scale,
                transform,
                callbacks,
                viewport,
                feedbacks,
                input_region,
                opaque_region,
                buffer_damage,
                window_geometry,
                min_size,
                max_size,
            )
        });

        // Build the neutral commit from the buffer assignment, depositing pixels for the presenter.
        let commit = match assignment {
            Some(BufferAssignment::NewBuffer(buffer)) => {
                // Try the shm read first (the common path); if the buffer is a `zwp_linux_dmabuf_v1`
                // buffer instead, CPU-import its LINEAR pixels by `pread`ing the plane fd. Either yields
                // tight top-left RGBA the presenter composites identically — the dmabuf pixels are
                // GENUINELY read from the client's fd (there is no GPU here), so the composited frame
                // matches the buffer EXACTLY, just like shm.
                let reader = BufferReader::new(&buffer);
                match reader
                    .shm_rgba()
                    .or_else(|| reader.dmabuf_rgba())
                    .or_else(|| reader.single_pixel_rgba())
                {
                    Some((mut stored, format)) => {
                        stored.damage = buffer_damage.filter(|damage| !damage.is_empty());
                        let state = BufferState {
                            tex_w: stored.width,
                            tex_h: stored.height,
                            format,
                            buffer_scale: scale,
                            gpu: false,
                        };
                        self.engine.presenter_mut().deposit(sid, stored);
                        // Synchronous CPU copy is done — release the buffer so the client may reuse it.
                        buffer.release();
                        let mut c = Commit::attach(state);
                        c.damage = damage;
                        c
                    }
                    // Neither an shm nor an importable dmabuf buffer (or malformed) — no-content commit.
                    None => Commit::default(),
                }
            }
            Some(BufferAssignment::Removed) => {
                self.engine.presenter_mut().forget(sid);
                Commit {
                    buffer: BufferChange::Removed,
                    ..Commit::default()
                }
            }
            None => Commit {
                buffer: BufferChange::Keep,
                damage,
                ..Commit::default()
            },
        };
        // Apply the just-read `wp_viewport` state and `wl_surface.set_buffer_transform` on every commit
        // (both double-buffered): the scene resolves the logical size from them and the presenter samples
        // the cropped+scaled or rotated/flipped region.
        let commit = Commit {
            viewport: Some(viewport),
            buffer_transform: Some(transform),
            // Apply the just-read regions on every commit (`Some(value)` = "this commit sets it"); smithay
            // reports the current applied state, so a cleared region reverts to the whole-surface default.
            input_region: Some(input_region),
            opaque_region: Some(opaque_region),
            window_geometry: Some(window_geometry),
            ..commit
        };

        // Hold this commit's `wl_surface.frame` callbacks until the frame they belong to actually reaches
        // the presenter. Firing them here — before the present decision — would tell the client "your
        // content is on screen, draw the next frame" even when the frame was throttled and NEVER shown,
        // which drops the just-committed content (the client overwrites it) or, if the client then idles,
        // strands stale content on screen forever. The neutral engine models callbacks as a per-surface
        // count; the adapter owns the concrete `wl_callback` objects and releases them per the pacing
        // outcome below.
        self.pending_callbacks
            .entry(sid)
            .or_default()
            .extend(frame_callbacks);
        // Hold this commit's presentation-feedback callbacks on the same terms: answered `presented` when
        // the frame reaches the screen, `discarded` if it is torn down unshown.
        if !feedbacks.is_empty() {
            self.pending_presentation
                .entry(sid)
                .or_default()
                .extend(feedbacks);
        }

        // Drive the neutral policy: apply + (unless cursor / sync-subsurface) compose, present, pace.
        hl_count!(tag::WAYLAND, "commits");
        let changed = self.engine.apply_commit(sid, commit);
        if let Some(surface) = self.engine.scene.get_mut(sid) {
            surface.min_size = min_size;
            surface.max_size = max_size;
        }
        self.reconcile_window(sid);
        let outcome = self.engine.complete_commit(sid, changed);
        let (cw, ch) = self
            .engine
            .scene
            .get(sid)
            .and_then(|s| s.logical_size())
            .unwrap_or((0, 0));
        hl_debug!(
            tag::WAYLAND,
            "commit surf={} {}x{} changed={}",
            sid.0,
            cw,
            ch,
            outcome.changed
        );

        // Release or retain the held callbacks — and schedule a repaint if the frame was withheld.
        match outcome.frame {
            Some(frame) => {
                let root = self.engine.scene.window_root(sid).unwrap_or(sid);
                self.settle_frame(root, &frame);
            }
            // No window present was driven this commit (a cursor image or a synchronized subsurface, which
            // ships atomically with its parent's next present). There is no per-frame boundary to gate on
            // here, so release immediately — matching the pre-existing behavior for these roles and
            // avoiding a stall if the parent never commits again.
            None => self.fire_callbacks_for(sid),
        }

        // Reflect this surface's tree onto the advertised `wl_output`: a toplevel that just mapped enters
        // the output (so a client learns which output — and thus scale — it is displayed on); one that
        // unmapped leaves it. Sent exactly once per map/unmap transition.
        self.update_output_membership(sid);
    }

    /// Emit `wl_surface.enter` / `wl_surface.leave` as the toplevel root owning `sid` maps (gains a
    /// committed buffer), unmaps (loses it), or is routed to a different output. The target output is the
    /// root's SELECTED output (its position-based route, else the primary — see
    /// [`crate::scene::model::Scene::selected_output`]). Subsurfaces/popups follow their root, so only the
    /// toplevel root is tracked. Sent exactly once per transition: a mapped surface whose selected output
    /// changed gets a `leave` for the old `wl_output` and an `enter` for the new one; an unmapped surface
    /// gets a `leave` for whichever it was on. A no-op when the client has not (yet) bound the target
    /// `wl_output` beyond the bookkeeping — smithay re-sends `enter` for tracked surfaces on a later bind.
    pub(super) fn update_output_membership(&mut self, sid: SurfaceId) {
        let Some(root) = self.engine.scene.window_root(sid) else {
            return;
        };
        if !matches!(
            self.engine.scene.get(root).map(|s| &s.role),
            Some(SurfaceRole::Toplevel)
        ) {
            return;
        }
        let Some(wl_surface) = self.surfaces_by_id.get(&root).cloned() else {
            return;
        };
        let mapped = self.engine.scene.get(root).and_then(|s| s.buffer).is_some();
        let current = self.entered_outputs.get(&root).copied();
        let target = self.engine.scene.selected_output(root).map(|o| o.id);
        if mapped {
            let Some(target) = target else { return };
            if current != Some(target) {
                // Leave the output we were on (if any) before entering the new one, so a client observes a
                // clean handoff (leave A, then enter B) rather than being on two outputs at once.
                if let Some(cur) = current {
                    if let Some(handle) = self.wl_output_handle(cur) {
                        handle.leave(&wl_surface);
                    }
                }
                if let Some(handle) = self.wl_output_handle(target) {
                    handle.enter(&wl_surface);
                }
                self.entered_outputs.insert(root, target);
            }
        } else if let Some(cur) = current {
            if let Some(handle) = self.wl_output_handle(cur) {
                handle.leave(&wl_surface);
            }
            self.entered_outputs.remove(&root);
        }
    }

    /// The smithay `wl_output` handle for a neutral [`OutputId`], if advertised.
    pub(super) fn wl_output_handle(&self, id: OutputId) -> Option<&WlOutputHandle> {
        self.outputs
            .iter()
            .find(|(oid, _)| *oid == id)
            .map(|(_, h)| h)
    }

    /// The primary output's `wl_output` handle (the first advertised) — the fallback the presentation
    /// feedback names when a surface has no resolvable selected output.
    pub(super) fn primary_wl_output(&self) -> Option<&WlOutputHandle> {
        self.outputs.first().map(|(_, h)| h)
    }

    /// The integer scale of the output surface `sid`'s window root is displayed on (its selected output,
    /// else the primary). Sources the fractional-scale hint so a surface on a HiDPI output learns a larger
    /// preferred scale than one on a scale-1 output.
    pub(super) fn output_scale_for(&self, sid: SurfaceId) -> i32 {
        let root = self.engine.scene.window_root(sid).unwrap_or(sid);
        self.engine
            .scene
            .selected_output(root)
            .map(|o| o.scale.max(1))
            .unwrap_or(1)
    }

    /// (Re)send `wp_fractional_scale_v1.preferred_scale` for `sid` from its current output's scale. A no-op
    /// if the client created no `wp_fractional_scale_v1` on the surface, or if the value is unchanged
    /// (smithay's `set_preferred_scale` dedups) — so it is safe to call on every route change.
    pub(super) fn send_preferred_fractional_scale(&self, sid: SurfaceId) {
        let scale = self.output_scale_for(sid) as f64;
        if let Some(surface) = self.surfaces_by_id.get(&sid) {
            with_states(surface, |states| {
                with_fractional_scale(states, |fractional| {
                    fractional.set_preferred_scale(scale);
                });
            });
        }
    }

    /// Route the toplevel at index `n` (ascending surface-id order) to the output whose logical rectangle
    /// contains global logical point `(x, y)`, then emit the resulting `wl_surface.leave`/`enter` and
    /// refresh its preferred fractional scale. The host/window-manager seam a multi-output demo drives to
    /// "place" a window on a monitor: real position-based routing (the compositor decides which output a
    /// window is on from where it sits), reduced to the smallest correct form — a point tested against each
    /// output's `logical_rect`. A point outside every output, or an out-of-range index, is ignored.
    pub(super) fn move_toplevel_to_point(&mut self, n: usize, x: i32, y: i32) {
        let Some(root) = self.toplevel_at(n) else {
            return;
        };
        let Some(output_id) = self.output_at_point(x, y) else {
            return;
        };
        self.engine.scene.route_surface_to_output(root, output_id);
        self.update_output_membership(root);
        self.send_preferred_fractional_scale(root);
    }

    /// The neutral [`OutputId`] whose logical rectangle contains global logical point `(x, y)`, if any.
    pub(super) fn output_at_point(&self, x: i32, y: i32) -> Option<OutputId> {
        self.engine
            .scene
            .outputs()
            .iter()
            .find(|o| o.contains_point(x, y))
            .map(|o| o.id)
    }
}
