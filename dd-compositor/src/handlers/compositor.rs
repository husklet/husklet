//! `wl_compositor` / `wl_subcompositor` / `wl_shm` handlers and the commit → present path.
//!
//! `commit()` snapshots the committed surface state (buffer, viewport, frame + presentation-feedback
//! callbacks), repacks the `wl_shm` pixels into dd-display's tight-BGRA [`SurfaceBuffer`], hands it to
//! the boxed [`Presenter`], fires the frame callbacks so the client keeps drawing, and answers
//! `wp_presentation` feedback so Chrome/viz's BeginFrameSource keeps ticking. This is the exact seam
//! `server.rs` drives; the difference is Smithay decoded the wire for us.

use std::time::Duration;

use dd_display::present::{GpuCompositeNode, PopupPlacement, SurfaceBuffer};

use smithay::{
    reexports::{
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::{
            protocol::{
                wl_buffer::WlBuffer, wl_callback::WlCallback, wl_output::Transform, wl_shm,
                wl_surface::WlSurface,
            },
            Client,
        },
    },
    utils::Size,
    wayland::{
        buffer::BufferHandler,
        compositor::{
            get_children, get_parent, is_sync_subsurface, with_states, BufferAssignment,
            CompositorClientState, CompositorHandler, CompositorState, Damage, SubsurfaceCachedState,
            SurfaceAttributes,
        },
        presentation::{PresentationFeedbackCachedState, Refresh},
        shm::{with_buffer_contents, ShmHandler, ShmState},
        viewporter::ViewportCachedState,
    },
};

use crate::{BufferUseKind, ClientState, DdState};

/// How a presented tree should advance its per-surface frame pacing, derived from what actually
/// happened to the frame. Replaces the old `did_present: bool` that conflated "nothing to present"
/// (a clean tree) with "present FAILED" — two cases that must pace differently:
///   - [`FramePacing::Presented`]: a new frame reached the screen → fire `wl_surface.frame` callbacks
///     and answer `wp_presentation` feedback with `presented`.
///   - [`FramePacing::Skipped`]: the tree was clean (a no-damage / frame-callback-only commit); the
///     previously delivered frame still stands → fire frame callbacks (the client may draw again) but
///     `discard` the feedback (no NEW content was shown this cycle).
///   - [`FramePacing::Failed`]: the present was attempted but did not reach the screen (the presenter
///     returned `Offscreen` or an error) → RETAIN the frame callbacks (the client must not be told its
///     frame shipped and recycle a buffer we still need to retry) and `discard` the feedback.
#[derive(Clone, Copy, PartialEq)]
pub(crate) enum FramePacing {
    Presented,
    Skipped,
    Failed,
}

struct PresentedFrame {
    output: smithay::output::Output,
    serial: u64,
    time: Duration,
    refresh: Refresh,
    flags: wp_presentation_feedback::Kind,
}

impl PresentedFrame {
    fn from_timing(
        output: smithay::output::Output,
        serial: u64,
        timing: dd_display::present::PresentTiming,
    ) -> Self {
        Self {
            output,
            serial,
            time: Duration::from_nanos(timing.present_ns),
            refresh: if timing.refresh_ns == 0 {
                Refresh::Unknown
            } else {
                Refresh::fixed(Duration::from_nanos(timing.refresh_ns))
            },
            flags: if timing.vsync {
                wp_presentation_feedback::Kind::Vsync
            } else {
                wp_presentation_feedback::Kind::empty()
            },
        }
    }
}

/// Bounded terminal policy for callbacks retained across failed presents: a permanently-dead presenter
/// must not grow a surface's retained-callback queue without limit. Once a surface has this many
/// callbacks retained (never delivered because every present kept failing), the oldest are dropped —
/// their `wl_callback` is released without `done`, which is the correct terminal signal for a frame that
/// will never be presented.
const MAX_RETAINED_CALLBACKS: usize = 16;

impl CompositorHandler for DdState {
    fn compositor_state(&mut self) -> &mut CompositorState {
        &mut self.compositor
    }
    fn client_compositor_state<'a>(&self, client: &'a Client) -> &'a CompositorClientState {
        &client.get_data::<ClientState>().unwrap().compositor
    }
    fn new_surface(&mut self, surface: &WlSurface) {
        self.register_surface(surface);
    }
    fn commit(&mut self, surface: &WlSurface) {
        self.on_commit(surface);
    }
    fn destroyed(&mut self, surface: &WlSurface) {
        self.teardown_surface(surface);
    }
}

impl BufferHandler for DdState {
    fn buffer_destroyed(&mut self, buffer: &WlBuffer) {
        self.forget_destroyed_buffer(buffer);
    }
}

impl ShmHandler for DdState {
    fn shm_state(&self) -> &ShmState {
        &self.shm
    }

    fn new_shm_pool_quota(
        &mut self,
        client: &smithay::reexports::wayland_server::backend::ClientId,
        size: usize,
    ) -> Option<Box<dyn smithay::wayland::shm::ShmPoolQuota>> {
        self.reserve_shm_pool(client, size)
    }
}

impl DdState {
    /// The commit → present path. Smithay has already applied the surface's double-buffered state (and, on
    /// a parent commit, its synchronized subsurface children's cached state) before calling this. We:
    ///   1. remember the surface's latest `wl_shm` buffer, so a later re-composite can redraw a
    ///      subsurface/popup that did not re-attach a buffer this frame;
    ///   2. present the WINDOW ROOT (the toplevel that owns this surface's subsurface/popup tree),
    ///      compositing every mapped subsurface child and every popup at its parent-relative offset;
    ///   3. advance frame pacing for the whole presented tree (frame callbacks + `wp_presentation`
    ///      feedback), so Chrome/viz's BeginFrameSource keeps ticking.
    ///
    /// A *synchronized* subsurface commit does not present on its own — its state is applied atomically
    /// with the parent, so we defer to the parent's commit (Smithay invokes this handler again for the
    /// parent within the same transaction). Every other commit (toplevel, popup, or a *desynchronized*
    /// subsurface) presents the composited window root.
    pub(crate) fn on_commit(&mut self, surface: &WlSurface) {
        // Snapshot the surface's committed wp_content_type hint (photo/video/game) — composed from the
        // vendored Smithay content_type module; stored for the present/tearing policy to read.
        self.record_content_type(surface);

        // Ingest this commit's buffer + damage into the per-surface repack cache and mark the surface
        // dirty if its pixels changed. This is the CPU half of damage tracking: only the damaged rows of
        // a re-attached buffer are copied, instead of repacking the whole buffer on every commit.
        self.ingest_buffer(surface);

        // A custom cursor surface (`wl_pointer.set_cursor`) is NOT a window: turn its just-committed buffer
        // into the host cursor (handlers::seat) instead of presenting it as a tiny window. Handles animated /
        // updated cursors (each re-commit refreshes the host cursor).
        if self.is_cursor_surface(surface) {
            // A cursor is turned into a host cursor, never presented as a window, so it must not linger in
            // the dirty set or the repack cache — a stale cursor sid there would force the skip-present
            // fast-path to scan the tree on every unrelated commit. Its buffer stays in `self.buffers`
            // (that is where `update_cursor_surface` reads it from).
            let sid = self.surface_id(surface);
            self.dirty.remove(&sid);
            self.remove_repack_cache(sid);
            self.update_cursor_surface(surface);
            return;
        }

        // A synchronized subsurface is presented as part of its parent's atomic commit; do not present now
        // (its buffer is already remembered above and its frame callbacks are drained when the root
        // presents). Presenting here would show a half-applied tree.
        if is_sync_subsurface(surface) {
            return;
        }

        // The tree to present for this commit. By default a popup composites into its owning toplevel's
        // frame (`window_root` climbs popup parents to the toplevel). With native popup windows enabled
        // (`DD_DISPLAY_POPUP_WINDOWS`), a popup is instead its OWN present root — presented as a separate
        // native window at the positioner anchor (parity with the legacy `server.rs`/`present_cocoa` path),
        // so a menu/dropdown that extends past the toplevel edge is not clipped. See `present_root`.
        let root = if popup_windows_enabled() {
            self.present_root(surface)
        } else {
            self.window_root(surface)
        };
        let Some(root) = root else {
            return;
        };

        if !visibility_allows_present(
            self.visibility
                .get(&self.surface_id(&root))
                .copied()
                .or_else(|| self.presenter.surface_visibility(self.surface_id(&root)))
                .unwrap_or(dd_display::present::SurfaceVisibility::Visible),
        ) {
            // Keep the latest repacked content dirty for a single reveal repaint. Failed pacing
            // withholds/bounds frame callbacks and discards feedback because no frame was displayed.
            self.pace_tree(&root, FramePacing::Failed, None);
            return;
        }

        // Skip a redundant present: if NOTHING in the presented tree changed since it was last shown, the
        // composited frame is byte-for-byte what is already on screen — re-compositing and re-uploading it
        // is pure waste. Skip the present, but STILL fire the tree's `wl_surface.frame` callbacks: a client
        // that committed only to obtain a frame callback must not stall. Pacing the whole tree (not just
        // the committed surface) matches the full-present path's callback breadth, so no surface's callback
        // is ever left pending. No needed repaint is dropped — any changed surface anywhere in the tree
        // (this one, a sibling, or a sync subsurface that committed into this atomic parent commit) leaves
        // the tree dirty and forces the present below instead. Presentation feedback is `discarded` because
        // no new content reached the screen this cycle.
        if !self.tree_dirty(&root) {
            // Nothing changed: the previously presented frame still stands. Fire frame callbacks (so a
            // frame-callback-only commit never stalls) but discard feedback — this is a Skip, not a
            // failed present.
            self.pace_tree(&root, FramePacing::Skipped, None);
            return;
        }
        self.present_render_root(&root);
    }

    /// Ingest a commit's buffer + damage, keeping the per-surface tight-BGRA repack cache in sync with the
    /// latest committed content and marking the surface dirty iff its pixels changed. Returns whether the
    /// surface changed (a genuinely new buffer, an explicit detach, or damage against the current buffer);
    /// a commit that changed nothing — e.g. one made only to request a frame callback — returns `false` so
    /// the present can be skipped.
    ///
    /// We take Smithay's committed buffer assignment so its generic release-on-next-attach policy cannot
    /// race our CPU/GPU use. `self.buffers` retains the last content for bufferless commits; damage is
    /// drained here so it reflects only this commit.
    fn ingest_buffer(&mut self, surface: &WlSurface) -> bool {
        let sid = self.surface_id(surface);
        let (buffer, removed, damage, scale, surface_safe) = with_states(surface, |states| {
            let mut attrs = states.cached_state.get::<SurfaceAttributes>();
            let cur = attrs.current();
            // Take ownership: release is emitted only by the explicit BufferUse completion path.
            let (buffer, removed) = match cur.buffer.take() {
                Some(BufferAssignment::NewBuffer(b)) => (Some(b), false),
                Some(BufferAssignment::Removed) => (None, true),
                None => (None, false),
            };
            // Drain the accumulated damage (Smithay only extends it; the compositor is expected to consume
            // it, exactly as `pace_surface` drains the frame callbacks). `Damage` is not `Clone`, so lift
            // each rect to a plain `(y, h, surface_space)` we own — only the vertical extent drives the
            // row-band copy, and the flag records surface-space (`wl_surface.damage`, needs `* scale`) vs
            // buffer-space (`damage_buffer`, already in buffer pixels) damage.
            let damage: Vec<(i32, i32, bool)> = std::mem::take(&mut cur.damage)
                .iter()
                .map(|d| match d {
                    Damage::Buffer(r) => (r.loc.y, r.size.h, false),
                    Damage::Surface(r) => (r.loc.y, r.size.h, true),
                })
                .collect();
            let scale = cur.buffer_scale.max(1);
            let transform_normal = cur.buffer_transform
                == smithay::reexports::wayland_server::protocol::wl_output::Transform::Normal;
            // Surface-space damage maps to buffer rows by a plain `* buffer_scale` ONLY when there is no
            // buffer transform and no `wp_viewport` source crop; otherwise the mapping is non-linear and we
            // fall back to a full repack (see `damage_to_rows`).
            let mut vp = states.cached_state.get::<ViewportCachedState>();
            let has_src = vp.current().src.is_some();
            (buffer, removed, damage, scale, transform_normal && !has_src)
        });

        if removed {
            // Explicit detach. Only a change the first time (the assignment persists, so later commits keep
            // reporting `Removed` — treat those as no-ops).
            let had = self.buffers.remove(&sid).is_some();
            self.retire_buffer_use(sid);
            self.remove_repack_cache(sid);
            if had {
                self.dirty.insert(sid);
            }
            return had;
        }
        match buffer {
            Some(b) => {
                self.buffers.insert(sid, b.clone());
                let kind = if smithay::wayland::dmabuf::get_dmabuf(&b).is_ok() {
                    BufferUseKind::ZeroCopy
                } else {
                    BufferUseKind::ShmCopy
                };
                self.begin_buffer_use(sid, b.clone(), kind);
                match kind {
                    BufferUseKind::ShmCopy => {
                        if self.repack_shm(sid, &b, &damage, scale, surface_safe) {
                            self.complete_buffer_use(sid);
                        } else {
                            self.retire_buffer_use(sid);
                        }
                    }
                    BufferUseKind::ZeroCopy => self.remove_repack_cache(sid),
                }
                self.dirty.insert(sid);
                true
            }
            // No buffer has ever been attached (bufferless pre-map commit) — nothing to present, and
            // damage against a non-existent buffer changes nothing visible.
            None => false,
        }
    }

    /// Repack a committed `wl_shm` buffer into the surface's tight-BGRA cache, honouring damage. When the
    /// cache already holds the previous frame at the same size/format and this commit carries a mappable
    /// damage region, only the changed rows are copied — the rest of the cache already equals the new
    /// buffer, since a client guarantees the undamaged region is unchanged from the previously committed
    /// buffer. A first upload, a resize, a format change, or unmappable damage repacks the whole buffer. A
    /// dmabuf/IOSurface buffer has no CPU pixels (`with_buffer_contents` fails) and drops any stale cache;
    /// it presents zero-copy through [`Self::dmabuf_surface_buffer`].
    fn repack_shm(
        &mut self,
        sid: u32,
        buffer: &WlBuffer,
        damage: &[(i32, i32, bool)],
        scale: i32,
        surface_safe: bool,
    ) -> bool {
        let mut copied = false;
        let res = with_buffer_contents(buffer, |ptr, len, data| {
            let w = data.width;
            let h = data.height;
            let stride = data.stride;
            let src_off = data.offset;
            let fmt = match data.format {
                wl_shm::Format::Xrgb8888 => 1u32, // opaque (dd-display convention: format==1 ⇒ XRGB)
                _ => 0u32,                        // ARGB8888 (and anything else): honour alpha
            };
            // ROBUSTNESS (defense-in-depth, preserved from the pre-restructure `build_surface_buffer`):
            // width/height/stride/offset are client-controlled. Reject degenerate or malformed geometry,
            // and — crucially — never read past the actual mapping. Smithay's shm handler validates a
            // buffer fits its pool, but this guards a hostile or buggy client: without it a bad
            // stride/offset/height would `ptr.offset()` out of bounds (crash / info-leak) and an oversized
            // `w`/`h` would overflow the `tight * h` allocation. A rejected buffer drops any stale cache so
            // nothing garbage is presented. This one check (highest byte read = src_off + (h-1)*stride +
            // w*4) covers BOTH copy paths below: the partial path only ever reads a sub-band of rows
            // `[0, h)`, so the full-height bound bounds it too.
            let tight = match w.checked_mul(4).map(|t| t as usize) {
                Some(t) if w > 0 && h > 0 && stride >= w * 4 && src_off >= 0 => t,
                _ => {
                    self.remove_repack_cache(sid);
                    return;
                }
            };
            let last_row_start = src_off as usize + (h as usize - 1) * stride as usize;
            match last_row_start.checked_add(tight) {
                Some(max_read) if max_read <= len => {}
                _ => {
                    self.remove_repack_cache(sid);
                    return;
                }
            }
            // Reuse (and partially update) the cache only if it describes the same backing texture.
            let reusable = matches!(
                self.repacks.get(&sid),
                Some(c) if c.tex_w == w && c.tex_h == h && c.format == fmt
                    && c.bgra.len() == tight * h as usize
            );
            let rows = if reusable {
                damage_to_rows(damage, scale, h, surface_safe)
            } else {
                None
            };
            match rows {
                // Partial: copy ONLY the damaged rows into the existing cache (the win — the rest already
                // matches this buffer). Full-width rows keep the copy contiguous and always correct.
                Some((y0, y1)) => {
                    let cache = self.repacks.get_mut(&sid).unwrap();
                    for row in y0..y1 {
                        let src =
                            unsafe { ptr.offset(src_off as isize + row as isize * stride as isize) };
                        let dstart = row as usize * tight;
                        unsafe {
                            std::ptr::copy_nonoverlapping(src, cache.bgra[dstart..].as_mut_ptr(), tight);
                        }
                    }
                    cache.damage = Some((0, y0, w, y1 - y0));
                    copied = true;
                }
                // Full repack (first upload / resize / format change / unmappable damage).
                None => {
                    let Some(new_len) = tight.checked_mul(h as usize) else {
                        return;
                    };
                    let old_len = self.repacks.get(&sid).map_or(0, |c| c.bgra.len());
                    if !self.replace_cache_charge(sid, old_len, new_len) {
                        self.reject_budget_exhaustion(sid, "CPU repack cache");
                        return;
                    }
                    let mut bgra = Vec::new();
                    if bgra.try_reserve_exact(new_len).is_err() {
                        let _ = self.replace_cache_charge(sid, new_len, old_len);
                        self.reject_budget_exhaustion(sid, "CPU repack cache allocation");
                        return;
                    }
                    bgra.resize(new_len, 0);
                    for row in 0..h as isize {
                        let src = unsafe { ptr.offset(src_off as isize + row * stride as isize) };
                        let dstart = row as usize * tight;
                        unsafe {
                            std::ptr::copy_nonoverlapping(src, bgra[dstart..].as_mut_ptr(), tight);
                        }
                    }
                    self.repacks.insert(
                        sid,
                        RepackCache { bgra, tex_w: w, tex_h: h, format: fmt, damage: None },
                    );
                    copied = true;
                }
            }
        });
        if res.is_err() {
            // Not a `wl_shm` buffer (e.g. a dmabuf) — no CPU pixels to cache.
            self.remove_repack_cache(sid);
        }
        res.is_ok() && copied
    }

    /// The toplevel that owns `surface`'s composite tree: walk up subsurface parents, then popup parents,
    /// to the surface that is neither a subsurface nor a popup. That surface is the window presented to the
    /// screen; every subsurface/popup in its tree composites into its frame at a parent-relative offset.
    pub(crate) fn window_root(&self, surface: &WlSurface) -> Option<WlSurface> {
        let mut cur = surface.clone();
        // Bounded to defend against a pathological cycle in the parent links.
        for _ in 0..256 {
            if let Some(p) = get_parent(&cur) {
                cur = p; // subsurface → its parent surface
                continue;
            }
            match self.popup_parent(&cur) {
                Some(p) => cur = p, // popup → its parent (another popup, or the toplevel)
                None => return Some(cur),
            }
        }
        Some(cur)
    }

    /// The present root for `surface` when native popup windows are enabled (`DD_DISPLAY_POPUP_WINDOWS`):
    /// the nearest ancestor that is NOT a subsurface — a popup (its own native window) or the owning
    /// toplevel. Unlike [`Self::window_root`], this STOPS at a popup instead of climbing through it to the
    /// toplevel, so a popup (and its own subsurface children) presents as a standalone window at the
    /// positioner anchor rather than compositing into — and being clipped by — the toplevel's frame.
    pub(crate) fn present_root(&self, surface: &WlSurface) -> Option<WlSurface> {
        let mut cur = surface.clone();
        // Bounded to defend against a pathological cycle in the parent links.
        for _ in 0..256 {
            match get_parent(&cur) {
                Some(p) => cur = p, // subsurface → its parent surface
                // Not a subsurface: a popup or a toplevel. Either is a present root of its own.
                None => return Some(cur),
            }
        }
        Some(cur)
    }

    /// Resolve where a popup's native window should open: the DIRECT parent surface it is anchored to plus
    /// the positioner-resolved `(x, y)` offset from that parent's window-geometry top-left. Mirrors the
    /// legacy `server.rs::popup_placement` exactly (direct parent + this popup's own geometry origin), so
    /// the shared `present_cocoa` presenter — which places the popup window at parent-content-top-left +
    /// (x, y) and attaches it as a child window — opens the menu AT the anchoring widget. Nested popups
    /// (submenu chains) compose correctly because each popup's parent is itself a placed window. Returns
    /// `None` for toplevels and any surface with no popup role.
    pub(crate) fn popup_placement(&self, surface: &WlSurface) -> Option<PopupPlacement> {
        let (x, y, _, _) = self.popup_geometry(surface)?;
        let parent = self.popup_parent(surface)?;
        Some(PopupPlacement { parent_sid: self.surface_id(&parent), x, y })
    }

    /// Present `root` (a toplevel `wl_surface`) with its full surface tree composited in: the root's own
    /// buffer as the base, every mapped subsurface descendant blended at its parent-relative offset, then
    /// every popup anchored anywhere in this window (menus/dropdowns/tooltips) blended at its resolved
    /// screen offset — plus each popup's own subsurface descendants. CPU-side over-composite (the same
    /// model `server.rs` uses); a GPU/IOSurface root is presented as a single zero-copy texture and its
    /// children are not blended (documented limitation). Returns whether the frame reached the screen.
    pub(crate) fn present_render_root(&mut self, root: &WlSurface) -> bool {
        let mut evidence = None;
        let pacing = match self.present_tree(root) {
            Some(base) => {
                // Map the presenter's structured outcome onto frame pacing. Only a visibly Delivered
                // frame advances callbacks/feedback; an Offscreen present or a real output/device error
                // (both previously hidden behind a `false`) is a FAILED present — pacing is retained.
                let sid = base.sid;
                self.presenter_windows.insert(sid);
                match self.presenter.present(&base) {
                    Ok(dd_display::present::PresentOutcome::Delivered { serial, timing }) => {
                        evidence = timing.map(|timing| PresentedFrame::from_timing(
                            self.output.clone(),
                            serial,
                            timing,
                        ));
                        FramePacing::Presented
                    }
                    Ok(dd_display::present::PresentOutcome::Offscreen) => {
                        eprintln!(
                            "dd-compositor: present sid {sid} rendered offscreen but not delivered; \
                             retaining frame for retry"
                        );
                        FramePacing::Failed
                    }
                    Err(e) => {
                        eprintln!("dd-compositor: present sid {sid} failed: {e}");
                        FramePacing::Failed
                    }
                }
            }
            None => FramePacing::Failed,
        };
        let did_present = pacing == FramePacing::Presented;
        // The tree reached the screen — every surface in it is now clean. On a FAILED present we keep the
        // dirty flags so the next commit retries the repaint rather than skipping it.
        if did_present {
            self.complete_tree_buffer_uses(root);
            self.clear_tree_dirty(root);
        }
        self.pace_tree(root, pacing, evidence.as_ref());
        did_present
    }

    pub(crate) fn root_is_visible(&self, root: &WlSurface) -> bool {
        let sid = self.surface_id(root);
        visibility_allows_present(self.visibility
            .get(&sid)
            .copied()
            .or_else(|| self.presenter.surface_visibility(sid))
            .unwrap_or(dd_display::present::SurfaceVisibility::Visible))
    }

    /// Apply client or host visibility. Reveal presents the latest retained content exactly once.
    pub fn set_surface_visibility(
        &mut self,
        surface: &WlSurface,
        visibility: dd_display::present::SurfaceVisibility,
    ) {
        let root = self.window_root(surface).unwrap_or_else(|| surface.clone());
        let sid = self.surface_id(&root);
        let was_visible = self.root_is_visible(&root);
        self.visibility.insert(sid, visibility);
        self.presenter.set_surface_visibility(sid, visibility);
        let is_visible = visibility == dd_display::present::SurfaceVisibility::Visible;
        if !is_visible {
            self.dismiss_popup_grabs();
            if self.focus.as_ref().is_some_and(|focus| self.window_root(focus).as_ref() == Some(&root)) {
                self.focus = None;
                self.last_cfg = None;
                self.set_text_input_focus(None);
            }
        } else if !was_visible && self.tree_dirty(&root) {
            self.present_render_root(&root);
        }
    }

    /// Host-facing visibility entry point for native window notifications.
    pub fn set_surface_visibility_by_sid(
        &mut self,
        sid: u32,
        visibility: dd_display::present::SurfaceVisibility,
    ) -> bool {
        let Some(surface) = self.surface_resources.get(&sid).cloned() else {
            return false;
        };
        self.set_surface_visibility(&surface, visibility);
        true
    }

    fn complete_tree_buffer_uses(&mut self, root: &WlSurface) {
        let mut surfaces = Vec::new();
        self.collect_tree_surfaces(root, &mut surfaces);
        for (popup, _, _) in self.collect_popups_for_root(root) {
            self.collect_tree_surfaces(&popup, &mut surfaces);
        }
        for surface in surfaces {
            let sid = self.surface_id(&surface);
            self.complete_buffer_use(sid);
        }
    }

    /// Compose the full window tree rooted at `root` into a single present-ready [`SurfaceBuffer`],
    /// handling BOTH a CPU (`wl_shm`) root and a GPU (IOSurface) root — the mixed shm/IOSurface case that
    /// previously dropped every child of a GPU window (only the root texture was presented):
    ///   - **CPU root:** over-composite every subsurface descendant and every popup (and their
    ///     descendants) into the root's `bgra` on the CPU, as before. The root's partial-damage upload
    ///     hint is widened to the whole texture when overlays exist, since composited children fall
    ///     outside the root's own damage rect.
    ///   - **GPU root:** the IOSurface pixels are not CPU-addressable, so instead of losing the children
    ///     we gather each `wl_shm` subsurface/popup as a [`GpuCompositeNode`] into `base.overlays`; the
    ///     presenter composites them over the resolved IOSurface base. A shm subsurface + a popup over a
    ///     GPU (accelerated Chrome/glmark) root now both reach the screen.
    /// Returns `None` when the root has no committed buffer.
    fn present_tree(&mut self, root: &WlSurface) -> Option<SurfaceBuffer> {
        let mut base = self.snapshot_surface(root)?;
        let popups = self.collect_popups_for_root(root);
        let has_overlays =
            !popups.is_empty() || get_children(root).into_iter().any(|c| &c != root);
        if base.iosurface_id.is_some() {
            // GPU root: carry the CPU overlay layers for the presenter (root + popup subsurface trees).
            let mut overlays: Vec<GpuCompositeNode> = Vec::new();
            self.collect_overlay_nodes(root, 0, 0, &mut overlays);
            for (popup, ox, oy) in popups {
                if let Some(psb) = self.snapshot_surface(&popup) {
                    overlays.push(GpuCompositeNode { buffer: psb, x: ox, y: oy });
                }
                self.collect_overlay_nodes(&popup, ox, oy, &mut overlays);
            }
            base.overlays = overlays;
        } else if !base.bgra.is_empty() {
            // CPU root: over-composite the whole tree into the base now.
            if has_overlays {
                base.damage = None;
            }
            self.blend_subtree(&mut base, root, 0, 0);
            for (popup, ox, oy) in popups {
                if let Some(psb) = self.snapshot_surface(&popup) {
                    blend(&mut base, &psb, ox, oy);
                }
                self.blend_subtree(&mut base, &popup, ox, oy);
            }
        }
        Some(base)
    }

    /// Collect every mapped subsurface descendant of `surface` as a [`GpuCompositeNode`] overlay layer
    /// (snapshot + accumulated device-relative offset), bottom→top — the GPU-root analogue of
    /// [`Self::blend_subtree`], which does the same walk but composites into a CPU base instead of
    /// emitting layers. Only `wl_shm` children can be an overlay (an IOSurface child is skipped: nested
    /// zero-copy GPU subsurfaces are a separate, unimplemented case).
    fn collect_overlay_nodes(
        &self,
        surface: &WlSurface,
        base_x: i32,
        base_y: i32,
        out: &mut Vec<GpuCompositeNode>,
    ) {
        for child in get_children(surface) {
            if &child == surface {
                continue;
            }
            let (cx, cy) = with_states(&child, |states| {
                let mut sub = states.cached_state.get::<SubsurfaceCachedState>();
                let loc = sub.current().location;
                (loc.x, loc.y)
            });
            let (ax, ay) = (base_x + cx, base_y + cy);
            if let Some(csb) = self.snapshot_surface(&child) {
                if csb.iosurface_id.is_none() {
                    out.push(GpuCompositeNode { buffer: csb, x: ax, y: ay });
                }
            }
            self.collect_overlay_nodes(&child, ax, ay, out);
        }
    }

    /// Whether any surface in `root`'s presented tree (root + subsurface descendants + popups + their
    /// descendants) has changed since the tree was last presented. Drives the skip-redundant-present
    /// decision in [`Self::on_commit`].
    fn tree_dirty(&self, root: &WlSurface) -> bool {
        if self.dirty.is_empty() {
            return false;
        }
        let mut surfaces = Vec::new();
        self.collect_tree_surfaces(root, &mut surfaces);
        for (popup, _, _) in self.collect_popups_for_root(root) {
            self.collect_tree_surfaces(&popup, &mut surfaces);
        }
        surfaces.iter().any(|s| self.dirty.contains(&self.surface_id(s)))
    }

    /// Clear the dirty flag for every surface in `root`'s presented tree (called after a successful
    /// present — the whole tree is now on screen).
    fn clear_tree_dirty(&mut self, root: &WlSurface) {
        let mut surfaces = Vec::new();
        self.collect_tree_surfaces(root, &mut surfaces);
        for (popup, _, _) in self.collect_popups_for_root(root) {
            self.collect_tree_surfaces(&popup, &mut surfaces);
        }
        for s in surfaces {
            self.dirty.remove(&self.surface_id(&s));
        }
    }

    /// Blend every mapped subsurface descendant of `surface` into `base`, bottom→top (the z-order Smithay
    /// keeps in `get_children`), each at its accumulated parent-relative offset `(base_x, base_y)` plus its
    /// own `set_position`.
    fn blend_subtree(&self, base: &mut SurfaceBuffer, surface: &WlSurface, base_x: i32, base_y: i32) {
        for child in get_children(surface) {
            // A wl_surface is its own root in `get_children`; skip the self-entry Smithay includes.
            if &child == surface {
                continue;
            }
            let (cx, cy) = with_states(&child, |states| {
                let mut sub = states.cached_state.get::<SubsurfaceCachedState>();
                let loc = sub.current().location;
                (loc.x, loc.y)
            });
            let (ax, ay) = (base_x + cx, base_y + cy);
            if let Some(csb) = self.snapshot_surface(&child) {
                blend(base, &csb, ax, ay);
            }
            self.blend_subtree(base, &child, ax, ay);
        }
    }

    /// Every popup that ultimately belongs to `root`, each with its screen offset within `root` (the sum of
    /// the popup chain's per-popup geometry origins), ordered parents-before-children so a submenu blends on
    /// top of the menu that spawned it.
    fn collect_popups_for_root(&self, root: &WlSurface) -> Vec<(WlSurface, i32, i32)> {
        // With native popup windows enabled, popups are NOT composited into the toplevel frame — each
        // presents as its own window (see `present_root`) — so the toplevel's composite/pace tree carries
        // no popups.
        if popup_windows_enabled() {
            return Vec::new();
        }
        let mut out: Vec<(WlSurface, i32, i32, usize)> = Vec::new();
        for popup in self.xdg_shell.popup_surfaces() {
            if !popup.alive() {
                continue;
            }
            if let Some((tl, x, y, depth)) = self.popup_offset_to_toplevel(popup.wl_surface()) {
                if &tl == root {
                    out.push((popup.wl_surface().clone(), x, y, depth));
                }
            }
        }
        out.sort_by_key(|(_, _, _, depth)| *depth);
        out.into_iter().map(|(s, x, y, _)| (s, x, y)).collect()
    }

    /// Walk a popup's parent chain to the owning toplevel, summing each popup's geometry origin. Returns
    /// `(toplevel, x, y, depth)` where `(x, y)` is the popup's top-left relative to the toplevel and
    /// `depth` is the number of popups traversed (1 = anchored directly on the toplevel). Popup geometry is
    /// relative to the parent's window-geometry origin; the parent toplevel's own window-geometry offset is
    /// treated as zero (matching `server.rs`).
    fn popup_offset_to_toplevel(&self, popup: &WlSurface) -> Option<(WlSurface, i32, i32, usize)> {
        let mut cur = popup.clone();
        let (mut x, mut y, mut depth) = (0i32, 0i32, 0usize);
        for _ in 0..256 {
            let (gx, gy, _, _) = self.popup_geometry(&cur)?;
            let parent = self.popup_parent(&cur)?;
            x += gx;
            y += gy;
            depth += 1;
            if self.popup_parent(&parent).is_some() {
                cur = parent; // parent is itself a popup — keep climbing the submenu chain
                continue;
            }
            // Parent is not a popup: resolve it to its window root (handles a popup anchored on a
            // subsurface) and stop.
            return self.window_root(&parent).map(|tl| (tl, x, y, depth));
        }
        None
    }

    /// Build a [`SurfaceBuffer`] from a surface's last remembered `wl_shm` buffer and its current
    /// viewport, WITHOUT touching frame callbacks/feedback (that is [`pace_tree`]'s job). Used to read the
    /// pixels of the root and of every composited child/popup each present. `None` if the surface has no
    /// buffer yet.
    fn snapshot_surface(&self, surface: &WlSurface) -> Option<SurfaceBuffer> {
        let sid = self.surface_id(surface);
        let buffer = self.buffers.get(&sid)?;
        let (buffer_scale, dst, src, buffer_transform) = with_states(surface, |states| {
            let mut attrs = states.cached_state.get::<SurfaceAttributes>();
            let cur_attrs = attrs.current();
            let scale = cur_attrs.buffer_scale.max(1);
            // `wl_surface.set_buffer_transform`: the buffer's contents are stored rotated/flipped and must
            // be un-transformed to present upright. Read it here and apply it below (see
            // `apply_buffer_transform`); Normal is the overwhelming common case and stays a passthrough.
            let buffer_transform = cur_attrs.buffer_transform;
            let mut vp = states.cached_state.get::<ViewportCachedState>();
            let cur = vp.current();
            let src = cur.src.map(|r| (r.loc.x, r.loc.y, r.size.w, r.size.h));
            (scale, cur.size(), src, buffer_transform)
        });
        // GPU present path: a dmabuf-backed buffer carries a dd IOSurface id (no CPU pixels). Resolve it
        // to a zero-copy IOSurface `SurfaceBuffer` and skip the shm cache.
        if let Some(sb) = self.dmabuf_surface_buffer(sid, buffer, buffer_scale, dst, src) {
            return Some(sb);
        }
        // `wl_shm` path: build from the tight-BGRA cache, repacked (whole buffer or only the damaged rows)
        // at commit time in `repack_shm`. Apply the committed `wl_surface.buffer_transform` to the cached
        // pixels FIRST — producing an upright texture (`uw`×`uh`, with w/h swapped for 90°/270°) — so the
        // `wp_viewport` source crop, logical size, and damage are all resolved in upright surface space.
        let cache = self.repacks.get(&sid)?;
        let title = self.titles.get(&sid).cloned().unwrap_or_else(|| "dd".into());
        let (bgra, uw, uh) =
            apply_buffer_transform(&cache.bgra, cache.tex_w, cache.tex_h, buffer_transform);
        let damage = transform_damage(cache.damage, cache.tex_w, cache.tex_h, buffer_transform);
        let (log_w, log_h, uv_rect) = logical_size_and_uv(dst, src, uw, uh, buffer_scale);
        Some(SurfaceBuffer {
            sid,
            width: log_w,
            height: log_h,
            texture_width: uw,
            texture_height: uh,
            stride: uw * 4,
            format: cache.format,
            bgra,
            title,
            iosurface_id: None,
            gpu_render: false,
            uv_rect,
            damage,
            // If this surface is an `xdg_popup`, carry its positioner-resolved placement so a windowed
            // presenter can open it as a native popup window at the anchor. This is inert on the default
            // composite-into-parent path (a popup's `SurfaceBuffer` is only blended, which ignores the
            // field, and a toplevel present root is never a popup so this is `None`); it becomes live when
            // `DD_DISPLAY_POPUP_WINDOWS` makes a popup its own present root (see `present_root`).
            popup: self.popup_placement(surface),
            overlays: Vec::new(),
        })
    }

    /// Advance frame pacing for every surface in the presented window tree (root + subsurface descendants +
    /// popups + their descendants): drain and fire each surface's `wl_surface.frame` callbacks and answer
    /// its `wp_presentation` feedback. On a failed present the feedback is `discarded`. Draining is
    /// idempotent — a surface with no queued callback fires nothing — so re-presenting a tree that only
    /// partly changed does not double-fire.
    fn pace_tree(
        &mut self,
        root: &WlSurface,
        pacing: FramePacing,
        evidence: Option<&PresentedFrame>,
    ) {
        let mut surfaces = Vec::new();
        self.collect_tree_surfaces(root, &mut surfaces);
        for (popup, _, _) in self.collect_popups_for_root(root) {
            self.collect_tree_surfaces(&popup, &mut surfaces);
        }
        for s in surfaces {
            self.pace_surface(&s, pacing, evidence);
        }
    }

    /// Drain and fire ONE surface's `wl_surface.frame` callbacks and answer its `wp_presentation`
    /// feedback (`presented` on success, `discarded` otherwise). Draining is idempotent — a surface with
    /// no queued callback fires nothing. Split out of [`Self::pace_tree`] so the skip-present path can
    /// pace the committed surface (firing its frame callback so it never stalls) without re-compositing.
    fn pace_surface(
        &mut self,
        surface: &WlSurface,
        pacing: FramePacing,
        evidence: Option<&PresentedFrame>,
    ) {
        let (callbacks, feedback) = with_states(surface, |states| {
            let callbacks: Vec<_> = std::mem::take(
                &mut states
                    .cached_state
                    .get::<SurfaceAttributes>()
                    .current()
                    .frame_callbacks,
            );
            let feedback = std::mem::take(
                &mut states
                    .cached_state
                    .get::<PresentationFeedbackCachedState>()
                    .current()
                    .callbacks,
            );
            (callbacks, feedback)
        });
        let sid = self.surface_id(surface);
        let t = self.now_ms();
        // A frame callback tells the client "your frame is on screen, draw the next one". Fire it only
        // when the surface's content actually reached (or still stands on) the screen — a fresh Present
        // or a clean Skip. On a FAILED present the client's frame did NOT ship, so its callbacks are
        // RETAINED (re-fired on the next accepted present) instead of completed; completing them here
        // would let the client recycle a buffer the compositor still needs to retry.
        let did_present = pacing != FramePacing::Failed;
        if did_present {
            // Fire any callbacks retained from a prior failed present of this surface, then this cycle's.
            for cb in self.take_retained_callbacks(sid) {
                cb.done(t);
            }
            for cb in callbacks {
                cb.done(t);
            }
        } else {
            self.retain_frame_callbacks(sid, callbacks);
        }
        // Presentation feedback is `presented` ONLY for a real new delivery; a Skip or a Failed present
        // both `discard` it (no new content reached the screen this cycle).
        self.send_presentation_feedback(feedback, evidence);
    }

    /// Retain a surface's `wl_surface.frame` callbacks across a FAILED present so they can be fired on the
    /// next accepted present (the client is not told its frame shipped). Bounded by
    /// [`MAX_RETAINED_CALLBACKS`]: under a permanently-dead presenter the oldest retained callbacks are
    /// dropped (released without `done`) rather than growing the queue without limit.
    fn retain_frame_callbacks(&mut self, sid: u32, callbacks: Vec<WlCallback>) {
        if callbacks.is_empty() {
            return;
        }
        for cb in callbacks {
            if !self.reserve_callback(sid) {
                self.reject_budget_exhaustion(sid, "retained callback");
                break;
            }
            let q = self.retained_callbacks.entry(sid).or_default();
            q.push_back(cb);
            let dropped = if q.len() > MAX_RETAINED_CALLBACKS {
                q.pop_front(); // terminal policy: drop the oldest undeliverable callback
                true
            } else {
                false
            };
            if dropped {
                self.release_callbacks(sid, 1);
            }
        }
    }

    /// Take (and clear) the callbacks retained for `sid` across earlier failed presents — fired by the
    /// next accepted present.
    fn take_retained_callbacks(&mut self, sid: u32) -> Vec<WlCallback> {
        let callbacks: Vec<_> = self
            .retained_callbacks
            .remove(&sid)
            .map(|q| q.into_iter().collect())
            .unwrap_or_default();
        self.release_callbacks(sid, callbacks.len());
        callbacks
    }

    /// `surface` and every subsurface descendant, depth-first.
    fn collect_tree_surfaces(&self, surface: &WlSurface, out: &mut Vec<WlSurface>) {
        out.push(surface.clone());
        for child in get_children(surface) {
            if &child == surface {
                continue;
            }
            self.collect_tree_surfaces(&child, out);
        }
    }

    /// Answer every `wp_presentation_feedback` for a just-processed commit, mirroring `server.rs`'s
    /// `send_presentation_feedback` on the Smithay callback objects.
    fn send_presentation_feedback(
        &mut self,
        feedback: Vec<smithay::wayland::presentation::PresentationFeedbackCallback>,
        evidence: Option<&PresentedFrame>,
    ) {
        if feedback.is_empty() {
            return;
        }
        let Some(frame) = evidence else {
            for fb in feedback {
                fb.discarded();
            }
            return;
        };
        for fb in feedback {
            fb.presented(
                &frame.output,
                frame.time,
                frame.refresh,
                frame.serial,
                frame.flags,
            );
        }
    }

}

fn visibility_allows_present(visibility: dd_display::present::SurfaceVisibility) -> bool {
    visibility == dd_display::present::SurfaceVisibility::Visible
}

/// Per-surface repacked tight-BGRA texture — the CPU cache behind damage tracking (see
/// [`DdState::repacks`]). Holds the surface's last committed content so a damaged commit copies only its
/// changed rows into it (instead of repacking the whole `wl_shm` buffer) and a re-composite of an
/// unchanged surface reuses the pixels without re-reading `wl_shm`. The cache always holds the complete,
/// current frame, so the composited output is byte-for-byte identical to the full-upload path.
pub(crate) struct RepackCache {
    /// Tight BGRA, `tex_w * tex_h * 4` bytes.
    pub(crate) bgra: Vec<u8>,
    pub(crate) tex_w: i32,
    pub(crate) tex_h: i32,
    /// dd-display format convention: 1 ⇒ opaque XRGB, 0 ⇒ honour alpha (ARGB / anything else).
    pub(crate) format: u32,
    /// The backing-texture region the latest commit changed, `(x, y, w, h)`, or `None` when the whole
    /// texture was (re)uploaded. Copied onto the presented [`SurfaceBuffer::damage`] upload hint.
    pub(crate) damage: Option<(i32, i32, i32, i32)>,
}

/// The bounding row band `[y0, y1)` (in buffer/texture pixels) covered by `damage`, or `None` when there
/// is no damage OR it cannot be safely mapped to buffer rows — in which case the caller repacks/uploads
/// the whole buffer. Buffer-space damage (`wl_surface.damage_buffer`) is already in buffer pixels and is
/// always mappable; surface-space damage (`wl_surface.damage`) maps by `* scale` only when `surface_safe`
/// (no buffer transform, no `wp_viewport` source crop — see `ingest_buffer`). A degenerate (empty) band
/// also returns `None`, conservatively forcing a full repack. Only the Y extent is used: the copy stays a
/// contiguous full-width row band, which is always correct (undamaged columns in a damaged row equal the
/// new buffer's pixels there anyway).
fn damage_to_rows(
    damage: &[(i32, i32, bool)],
    scale: i32,
    h: i32,
    surface_safe: bool,
) -> Option<(i32, i32)> {
    if damage.is_empty() {
        return None;
    }
    let mut y0 = i32::MAX;
    let mut y1 = i32::MIN;
    for &(ry, rh, surface_space) in damage {
        let (ry, rh) = if surface_space {
            if !surface_safe {
                return None;
            }
            (ry.saturating_mul(scale), rh.saturating_mul(scale))
        } else {
            (ry, rh)
        };
        y0 = y0.min(ry);
        y1 = y1.max(ry.saturating_add(rh));
    }
    let y0 = y0.clamp(0, h);
    let y1 = y1.clamp(0, h);
    if y0 >= y1 {
        None
    } else {
        Some((y0, y1))
    }
}

/// Resolve a surface's on-screen logical size and its normalized backing-texture sample rect from
/// the `wp_viewport` destination (`dst`) / source (`src`) and the buffer's pixel size (`tex_w/h`) at
/// `buffer_scale`. Shared by the `wl_shm` repack and the dmabuf/IOSurface path so both honour the
/// viewport identically: a viewport `dst` sets the logical size; else a `src` crop's size; else the
/// buffer pixels divided by `buffer_scale` (HiDPI). The `uv_rect` crops to the `src` rectangle.
/// Apply a committed `wl_surface.buffer_transform` to tight-BGRA buffer pixels, producing the UPRIGHT
/// texture the presenter shows. Returns `(pixels, out_w, out_h)`; width/height are swapped for the 90°/
/// 270° variants (and their flips). `Normal` is a passthrough (just clones, as the untransformed path
/// did). The mapping follows the Wayland `wl_output.transform` convention (weston `weston_transformed_
/// coord`): for each output pixel `(ox, oy)` in the upright image we sample the buffer pixel it came from.
fn apply_buffer_transform(src: &[u8], bw: i32, bh: i32, transform: Transform) -> (Vec<u8>, i32, i32) {
    if matches!(transform, Transform::Normal) || bw <= 0 || bh <= 0 {
        return (src.to_vec(), bw, bh);
    }
    // Output (upright) dimensions: 90°/270° (and their flips) swap width and height.
    let (ow, oh) = match transform {
        Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270 => (bh, bw),
        _ => (bw, bh),
    };
    let sstride = (bw * 4) as usize;
    let dstride = (ow * 4) as usize;
    if src.len() < sstride * bh as usize {
        return (src.to_vec(), bw, bh);
    }
    let mut out = vec![0u8; dstride * oh as usize];
    for oy in 0..oh {
        for ox in 0..ow {
            // Output pixel (ox,oy) → source buffer pixel (sx,sy). `ow`/`oh` are the upright (surface-space)
            // dimensions; the reflections subtract 1 for 0-indexed pixels.
            let (sx, sy) = match transform {
                Transform::Flipped => (ow - 1 - ox, oy),
                Transform::_180 => (ow - 1 - ox, oh - 1 - oy),
                Transform::Flipped180 => (ox, oh - 1 - oy),
                Transform::_90 => (oh - 1 - oy, ox),
                Transform::Flipped90 => (oh - 1 - oy, ow - 1 - ox),
                Transform::_270 => (oy, ow - 1 - ox),
                Transform::Flipped270 => (oy, ox),
                // Normal handled above; any future variant falls back to identity.
                _ => (ox, oy),
            };
            if sx < 0 || sx >= bw || sy < 0 || sy >= bh {
                continue;
            }
            let si = sy as usize * sstride + sx as usize * 4;
            let di = oy as usize * dstride + ox as usize * 4;
            out[di..di + 4].copy_from_slice(&src[si..si + 4]);
        }
    }
    (out, ow, oh)
}

/// Map a damage rectangle from BUFFER-texture coordinates into the UPRIGHT (post-`buffer_transform`)
/// texture coordinates the presented `SurfaceBuffer` uses, so a partial-upload damage hint stays valid
/// after the transform. `Normal` passes through unchanged; for other transforms the rectangle's four
/// corners are mapped and the bounding box returned (a superset is always a safe upload hint, and `bgra`
/// carries the complete frame regardless). `None` in ⇒ `None` out (full upload).
fn transform_damage(
    damage: Option<(i32, i32, i32, i32)>,
    bw: i32,
    bh: i32,
    transform: Transform,
) -> Option<(i32, i32, i32, i32)> {
    let (x, y, w, h) = damage?;
    if matches!(transform, Transform::Normal) || w <= 0 || h <= 0 {
        return damage;
    }
    // Buffer→output point map (inverse of `apply_buffer_transform`'s sampling), with output size (ow,oh).
    let (ow, oh) = match transform {
        Transform::_90 | Transform::_270 | Transform::Flipped90 | Transform::Flipped270 => (bh, bw),
        _ => (bw, bh),
    };
    let map = |bx: i32, by: i32| -> (i32, i32) {
        match transform {
            Transform::Flipped => (bw - 1 - bx, by),
            Transform::_180 => (bw - 1 - bx, bh - 1 - by),
            Transform::Flipped180 => (bx, bh - 1 - by),
            Transform::_90 => (by, bw - 1 - bx),
            Transform::Flipped90 => (bh - 1 - by, bw - 1 - bx),
            Transform::_270 => (bh - 1 - by, bx),
            Transform::Flipped270 => (by, bx),
            _ => (bx, by),
        }
    };
    let corners = [(x, y), (x + w - 1, y), (x, y + h - 1), (x + w - 1, y + h - 1)];
    let mut minx = i32::MAX;
    let mut miny = i32::MAX;
    let mut maxx = i32::MIN;
    let mut maxy = i32::MIN;
    for (bx, by) in corners {
        let (px, py) = map(bx, by);
        minx = minx.min(px);
        miny = miny.min(py);
        maxx = maxx.max(px);
        maxy = maxy.max(py);
    }
    let minx = minx.clamp(0, ow - 1);
    let miny = miny.clamp(0, oh - 1);
    let maxx = maxx.clamp(0, ow - 1);
    let maxy = maxy.clamp(0, oh - 1);
    Some((minx, miny, maxx - minx + 1, maxy - miny + 1))
}

pub(crate) fn logical_size_and_uv(
    dst: Option<Size<i32, smithay::utils::Logical>>,
    src: Option<(f64, f64, f64, f64)>,
    tex_w: i32,
    tex_h: i32,
    buffer_scale: i32,
) -> (i32, i32, [f32; 4]) {
    match (dst, src) {
        (Some(sz), src) if sz.w > 0 && sz.h > 0 => {
            (sz.w, sz.h, uv_from_src(src, tex_w, tex_h, buffer_scale))
        }
        (None, Some((_, _, sw, sh))) if sw > 0.0 && sh > 0.0 => (
            (sw.round() as i32).max(1),
            (sh.round() as i32).max(1),
            uv_from_src(src, tex_w, tex_h, buffer_scale),
        ),
        _ => (
            (tex_w / buffer_scale.max(1)).max(1),
            (tex_h / buffer_scale.max(1)).max(1),
            [0.0, 0.0, 1.0, 1.0],
        ),
    }
}

/// Normalize a `wp_viewport` source rectangle `(x, y, w, h)` — given in post-buffer-scale/logical
/// coords — into a `[u0, v0, u1, v1]` sample rect over the backing texture (buffer pixels). Returns the
/// full texture when there is no source crop or the texture has no area.
fn uv_from_src(src: Option<(f64, f64, f64, f64)>, tex_w: i32, tex_h: i32, buffer_scale: i32) -> [f32; 4] {
    match src {
        Some((x, y, w, h)) if tex_w > 0 && tex_h > 0 && w > 0.0 && h > 0.0 => {
            let s = buffer_scale.max(1) as f64;
            let (tw, th) = (tex_w as f64, tex_h as f64);
            let u0 = ((x * s) / tw).clamp(0.0, 1.0) as f32;
            let v0 = ((y * s) / th).clamp(0.0, 1.0) as f32;
            let u1 = (((x + w) * s) / tw).clamp(0.0, 1.0) as f32;
            let v1 = (((y + h) * s) / th).clamp(0.0, 1.0) as f32;
            [u0, v0, u1, v1]
        }
        _ => [0.0, 0.0, 1.0, 1.0],
    }
}

/// Alpha-composite `top` over `base` at the logical offset `(x_logical, y_logical)` (relative to the
/// base surface's origin). `base` holds tight BGRA backing-texture pixels; `top` is drawn at its LOGICAL
/// destination size (`top.width` × `top.height`, scaled to the base's device pixels) — NOT its raw
/// backing dimensions — and SAMPLED through its `wp_viewport` source crop (`top.uv_rect`, a normalized
/// rect over `top`'s backing texture). So a child that scales a small buffer up, or crops a sub-region of
/// a larger one via `wp_viewport`, composites at the correct on-screen size instead of 1:1 backing
/// pixels. `top` is clipped to the base bounds (a menu past the window edge is cropped — a documented
/// limitation of compositing popups into the parent frame rather than their own native windows).
///
/// Color math is correct premultiplied source-over. Wayland ARGB8888 buffers carry PREMULTIPLIED alpha
/// (color channels already multiplied by their own alpha), so the Porter-Duff "over" is
/// `dst = src + dst·(1-a)` — the source is NOT multiplied by `a` again (doing so double-applies the
/// alpha and darkens semi-transparent children). An XRGB `top` (`format == 1`) is fully opaque.
fn blend(base: &mut SurfaceBuffer, top: &SurfaceBuffer, x_logical: i32, y_logical: i32) {
    let (bw, bh) = (base.texture_width, base.texture_height);
    let (tw, th) = (top.texture_width, top.texture_height);
    // Destination extent in the top's own LOGICAL space (its on-screen size after `wp_viewport` dst-size
    // / buffer-scale) — what `top.width`/`top.height` carry — NOT the backing texture size.
    let (dw, dh) = (top.width, top.height);
    if bw <= 0 || bh <= 0 || tw <= 0 || th <= 0 || dw <= 0 || dh <= 0 {
        return;
    }
    let base_stride = (bw * 4) as usize;
    let top_stride = (tw * 4) as usize;
    if base.bgra.len() < base_stride * bh as usize || top.bgra.len() < top_stride * th as usize {
        return;
    }
    // Backing-store scale of the base texture relative to its logical size; child offsets + sizes are
    // logical, so the child occupies `dw*s × dh*s` device pixels at `(x_logical*s, y_logical*s)`.
    let s = if base.width > 0 {
        (bw as f64 / base.width as f64).round().max(1.0)
    } else {
        1.0
    };
    let ddw = ((dw as f64) * s).round().max(1.0) as i32;
    let ddh = ((dh as f64) * s).round().max(1.0) as i32;
    let (ox, oy) = ((x_logical as f64 * s).round() as i32, (y_logical as f64 * s).round() as i32);
    // Source sample window over the top's backing texture, from its `wp_viewport` source crop
    // (`top.uv_rect` is normalized `[u0,v0,u1,v1]`; the full texture when there is no crop).
    let [u0, v0, u1, v1] = top.uv_rect;
    let (su0, sv0) = (u0 as f64 * tw as f64, v0 as f64 * th as f64);
    let (sw, sh) = ((u1 - u0) as f64 * tw as f64, (v1 - v0) as f64 * th as f64);
    let top_opaque = top.format == 1;
    for dy in 0..ddh {
        let by = oy + dy;
        if by < 0 || by >= bh {
            continue;
        }
        // Map this device destination row to a source texel row through the viewport crop.
        let fy = if ddh > 1 { dy as f64 / ddh as f64 } else { 0.0 };
        let sy = (sv0 + fy * sh).floor() as i32;
        let sy = sy.clamp(0, th - 1);
        for dx in 0..ddw {
            let bx = ox + dx;
            if bx < 0 || bx >= bw {
                continue;
            }
            let fx = if ddw > 1 { dx as f64 / ddw as f64 } else { 0.0 };
            let sx = (su0 + fx * sw).floor() as i32;
            let sx = sx.clamp(0, tw - 1);
            let ti = sy as usize * top_stride + sx as usize * 4;
            let bi = by as usize * base_stride + bx as usize * 4;
            let a = if top_opaque { 255u32 } else { top.bgra[ti + 3] as u32 };
            if a == 0 {
                continue;
            }
            if a == 255 {
                base.bgra[bi] = top.bgra[ti];
                base.bgra[bi + 1] = top.bgra[ti + 1];
                base.bgra[bi + 2] = top.bgra[ti + 2];
                base.bgra[bi + 3] = 255;
            } else {
                // Premultiplied "over": the source channels are ALREADY multiplied by `a`, so add them
                // directly to the attenuated destination — do not multiply the source by `a` again.
                let ia = 255 - a;
                for c in 0..3 {
                    let src_pm = top.bgra[ti + c] as u32;
                    base.bgra[bi + c] = (src_pm + base.bgra[bi + c] as u32 * ia / 255).min(255) as u8;
                }
                base.bgra[bi + 3] = (a + base.bgra[bi + 3] as u32 * ia / 255).min(255) as u8;
            }
        }
    }
}

/// Whether native popup windows are enabled (`DD_DISPLAY_POPUP_WINDOWS=1|true|on`). When set, an
/// `xdg_popup` presents as its own native window at the positioner anchor (via `SurfaceBuffer::popup`,
/// which the shared `present_cocoa` presenter turns into a child NSWindow) instead of compositing into —
/// and being clipped by — the owning toplevel's frame. Off (default) preserves the composite-into-parent
/// behaviour, which is byte-for-byte identical to the pre-existing path. Gated until the live
/// Chrome/GTK-on-Smithay menu validation runs (see docs/rendering/SMITHAY_DEFAULT_READINESS.md, Gap 2).
pub(crate) fn popup_windows_enabled() -> bool {
    matches!(
        std::env::var("DD_DISPLAY_POPUP_WINDOWS").as_deref(),
        Ok("1") | Ok("true") | Ok("on")
    )
}

#[cfg(test)]
mod presentation_evidence_tests {
    use super::*;
    use dd_display::present::PresentTiming;
    use smithay::output::{Output, PhysicalProperties, Subpixel};

    fn output(name: &str) -> Output {
        Output::new(
            name.into(),
            PhysicalProperties {
                size: (600, 340).into(),
                subpixel: Subpixel::Unknown,
                make: "dd".into(),
                model: "test".into(),
            },
        )
    }

    #[test]
    fn two_surfaces_share_exactly_one_delivered_frame_record() {
        let frame = PresentedFrame::from_timing(
            output("left"),
            77,
            PresentTiming {
                present_ns: 5_000_000_123,
                refresh_ns: 8_333_333,
                vsync: true,
            },
        );

        // pace_tree passes this one immutable record by reference to every surface in the tree.
        let parent = Some(&frame);
        let child = Some(&frame);
        assert!(std::ptr::eq(parent.unwrap(), child.unwrap()));
        assert_eq!(frame.serial, 77);
        assert_eq!(frame.time, Duration::new(5, 123));
        assert_eq!(frame.refresh, Refresh::fixed(Duration::from_nanos(8_333_333)));
        assert_eq!(frame.flags, wp_presentation_feedback::Kind::Vsync);
    }

    #[test]
    fn output_frames_keep_independent_backend_timing_and_do_not_invent_vsync() {
        let left = PresentedFrame::from_timing(
            output("left"),
            11,
            PresentTiming { present_ns: 10, refresh_ns: 16_666_667, vsync: true },
        );
        let right = PresentedFrame::from_timing(
            output("right"),
            29,
            PresentTiming { present_ns: 20, refresh_ns: 0, vsync: false },
        );

        assert_eq!(left.output.name(), "left");
        assert_eq!(right.output.name(), "right");
        assert_eq!(left.serial, 11);
        assert_eq!(right.serial, 29);
        assert_eq!(left.refresh, Refresh::fixed(Duration::from_nanos(16_666_667)));
        assert_eq!(right.refresh, Refresh::Unknown);
        assert_eq!(right.flags, wp_presentation_feedback::Kind::empty());
    }

    #[test]
    fn minimized_and_occluded_roots_do_not_schedule_present_until_reveal() {
        use dd_display::present::SurfaceVisibility::{Minimized, Occluded, Visible};
        assert!(visibility_allows_present(Visible));
        assert!(!visibility_allows_present(Minimized));
        assert!(!visibility_allows_present(Occluded));
    }
}
