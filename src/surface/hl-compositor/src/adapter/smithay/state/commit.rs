use super::*;
use super::{buffer::BufferReader, output::Region};

mod membership;
#[cfg(feature = "macos-surface")]
mod region;

impl HlState {
    /// Translates committed Smithay state into the neutral scene and drives presentation/frame pacing.
    pub(super) fn on_commit(&mut self, surface: &WlSurface) {
        let Some(sid) = self.sid(surface) else {
            return;
        };
        let was_mapped = self
            .engine
            .scene
            .get(sid)
            // Content, not a buffer: a zero-copy surface is mapped without ever attaching one.
            .is_some_and(|surface| surface.has_content());

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

        #[cfg(feature = "macos-surface")]
        let mut external = None;
        #[cfg(feature = "macos-surface")]
        let mut native_buffer = None;
        #[cfg(feature = "macos-surface")]
        let mut native_external = None;
        #[cfg(feature = "macos-surface")]
        let mut native_metadata = None;
        #[cfg(feature = "macos-surface")]
        let replaces_buffer = assignment.is_some();

        // Build the neutral commit from the buffer assignment, depositing pixels for the presenter.
        let commit = match assignment {
            Some(BufferAssignment::NewBuffer(buffer)) => {
                // Try the shm read first (the common path); if the buffer is a `zwp_linux_dmabuf_v1`
                // buffer instead, CPU-import its LINEAR pixels by `pread`ing the plane fd. Either yields
                // tight top-left RGBA the presenter composites identically — the dmabuf pixels are
                // GENUINELY read from the client's fd (there is no GPU here), so the composited frame
                // matches the buffer EXACTLY, just like shm.
                let reader = BufferReader::new(&buffer);
                #[cfg(feature = "macos-surface")]
                match reader.external() {
                    super::buffer::External::Pending(metadata)
                    | super::buffer::External::Published(metadata)
                        if self.native_frames_enabled() =>
                    {
                        external = Some((metadata.token, metadata.serial));
                        native_buffer = Some(buffer.clone());
                        native_external = get_dmabuf(&buffer).ok().cloned();
                        native_metadata = Some(super::native::Metadata {
                            width: metadata.width,
                            height: metadata.height,
                            stride: metadata.stride,
                            format: metadata.format,
                        });
                        let mut commit = Commit::attach(BufferState {
                            tex_w: metadata.width,
                            tex_h: metadata.height,
                            format: metadata.format,
                            buffer_scale: scale,
                            gpu: true,
                        });
                        commit.damage = buffer_damage
                            .as_deref()
                            .map(|damage| {
                                region::surface(
                                    damage,
                                    metadata.width,
                                    metadata.height,
                                    scale,
                                    transform,
                                )
                            })
                            .unwrap_or_else(|| damage.clone());
                        commit
                    }
                    super::buffer::External::Pending(_)
                    | super::buffer::External::Published(_)
                    | super::buffer::External::Malformed => {
                        buffer.release();
                        Commit::default()
                    }
                    super::buffer::External::Linear => {
                        self.read_buffer(sid, &buffer, reader, buffer_damage, scale, &damage)
                    }
                }
                #[cfg(not(feature = "macos-surface"))]
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
                        self.stash_cursor_pixels(sid, &stored, scale);
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
                self.drop_cursor_pixels(sid);
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

        // `wp_viewport.set_source` must lie inside the surface's content area; otherwise the protocol
        // mandates `wp_viewport.error.out_of_buffer`. Smithay caches the crop but leaves the check to the
        // compositor, and the presenter CLAMPS an out-of-range sample to the buffer edge — so unenforced,
        // a client with a bad crop sees smeared pixels instead of the error it earned. The error kills the
        // client, so this commit is abandoned rather than composited.
        if !self.viewport_within_content(surface, sid, &commit, transform) {
            return;
        }

        #[cfg(feature = "macos-surface")]
        let association = self.take_commit_association(sid, external);
        #[cfg(feature = "macos-surface")]
        if replaces_buffer {
            self.replace_native_commit(sid);
        }
        #[cfg(feature = "macos-surface")]
        if let Some((token, serial)) = association {
            if self.native_frames_enabled() {
                if external.is_some() {
                    hl_count!(tag::WAYLAND, "private_dmabuf_commits");
                }
                let deferred = super::native::Deferred {
                    surface: sid,
                    commit,
                    was_mapped,
                    min_size,
                    max_size,
                    frame_callbacks,
                    feedbacks,
                    buffer: native_buffer,
                    external: native_external,
                    metadata: native_metadata,
                };
                let result = if external.is_some() {
                    self.defer_external_commit(token, serial, deferred)
                } else {
                    self.defer_native_commit(token, serial, deferred)
                };
                match result {
                    super::native::Defer::Ready(ready) => self.finish_native(ready),
                    super::native::Defer::Reuse(deferred) => self.reuse_native(deferred),
                    super::native::Defer::Waiting => {}
                }
                return;
            }
        }
        #[cfg(not(feature = "macos-surface"))]
        let _association = self.take_surface_association(sid);

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

        self.finish_commit(sid, commit, was_mapped, min_size, max_size);
    }

    /// Whether this commit's `wp_viewport` source crop lies inside the content area of the buffer the
    /// surface will present. Posts `wp_viewport.error.out_of_buffer` (via Smithay, which owns the viewport
    /// resource) and answers `false` when it does not. A surface with no committed buffer bounds nothing,
    /// so the crop is accepted until content arrives.
    fn viewport_within_content(
        &self,
        surface: &WlSurface,
        sid: SurfaceId,
        commit: &Commit,
        transform: BufferTransform,
    ) -> bool {
        let buffer = match commit.buffer {
            BufferChange::New(buffer) => Some(buffer),
            BufferChange::Removed => None,
            BufferChange::Keep => self.engine.scene.get(sid).and_then(|s| s.buffer),
        };
        let Some(buffer) = buffer else {
            return true;
        };
        // The content area in surface-local logical coordinates — the buffer oriented by its transform,
        // divided by its buffer scale. This is exactly the space `wp_viewport.set_source` speaks.
        let scale = buffer.buffer_scale.max(1);
        let (w, h) = transform.surface_size(buffer.tex_w, buffer.tex_h);
        let content = Size::<i32, Logical>::from((w / scale, h / scale));
        with_states(surface, |states| ensure_viewport_valid(states, content))
    }

    #[cfg(feature = "macos-surface")]
    fn read_buffer(
        &mut self,
        sid: SurfaceId,
        buffer: &WlBuffer,
        reader: BufferReader<'_>,
        buffer_damage: Option<Vec<Rect>>,
        scale: i32,
        damage: &[Rect],
    ) -> Commit {
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
                self.stash_cursor_pixels(sid, &stored, scale);
                self.engine.presenter_mut().deposit(sid, stored);
                buffer.release();
                let mut commit = Commit::attach(state);
                commit.damage = damage.to_vec();
                commit
            }
            None => Commit::default(),
        }
    }

    pub(super) fn finish_commit(
        &mut self,
        sid: SurfaceId,
        commit: Commit,
        was_mapped: bool,
        min_size: (Option<i32>, Option<i32>),
        max_size: (Option<i32>, Option<i32>),
    ) {
        // Drive the neutral policy: apply + (unless cursor / sync-subsurface) compose, present, pace.
        hl_count!(tag::WAYLAND, "commits");
        let changed = self.engine.apply_commit(sid, commit);
        let mapped_toplevel = !was_mapped
            && self.engine.scene.get(sid).is_some_and(|surface| {
                surface.has_content() && matches!(surface.role, SurfaceRole::Toplevel)
            });
        if let Some(surface) = self.engine.scene.get_mut(sid) {
            surface.set_size_limits(min_size, max_size);
        }
        if mapped_toplevel {
            self.set_keyboard_focus(Some(sid));
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
}
