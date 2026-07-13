//! `wl_compositor` / `wl_subcompositor` / `wl_shm` handlers and the commit → present path.
//!
//! `commit()` snapshots the committed surface state (buffer, viewport, frame + presentation-feedback
//! callbacks), repacks the `wl_shm` pixels into dd-display's tight-BGRA [`SurfaceBuffer`], hands it to
//! the boxed [`Presenter`], fires the frame callbacks so the client keeps drawing, and answers
//! `wp_presentation` feedback so Chrome/viz's BeginFrameSource keeps ticking. This is the exact seam
//! `server.rs` drives; the difference is Smithay decoded the wire for us.

use std::time::Duration;

use dd_display::present::{PopupPlacement, SurfaceBuffer};

use smithay::{
    reexports::{
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::{
            protocol::{wl_buffer::WlBuffer, wl_callback::WlCallback, wl_shm, wl_surface::WlSurface},
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

use crate::{surface_id, ClientState, DdState, OUTPUT_REFRESH_MHZ};

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
    fn commit(&mut self, surface: &WlSurface) {
        self.on_commit(surface);
    }
}

impl BufferHandler for DdState {
    fn buffer_destroyed(&mut self, _buffer: &WlBuffer) {}
}

impl ShmHandler for DdState {
    fn shm_state(&self) -> &ShmState {
        &self.shm
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
            let sid = surface_id(surface);
            self.dirty.remove(&sid);
            self.repacks.remove(&sid);
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
            self.pace_tree(&root, FramePacing::Skipped);
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
    /// Smithay's `SurfaceAttributes` are cumulative, not per-commit: `current().buffer` PERSISTS across
    /// commits (its `merge_into` only overwrites it on a fresh attach, so it reads `Some(NewBuffer)` on
    /// every commit once a buffer is attached) and `current().damage` only ever ACCUMULATES. So a genuinely
    /// new buffer is detected by object identity against the last one we stored, and the damage is DRAINED
    /// here (like the frame callbacks are drained when pacing) so it reflects only this commit.
    fn ingest_buffer(&mut self, surface: &WlSurface) -> bool {
        let sid = surface_id(surface);
        let (buffer, removed, damage, scale, surface_safe) = with_states(surface, |states| {
            let mut attrs = states.cached_state.get::<SurfaceAttributes>();
            let cur = attrs.current();
            // Read the persistent buffer assignment WITHOUT clearing it — Smithay uses `current().buffer`
            // for its own `wl_buffer.release` bookkeeping on the next attach, so clearing it would leak the
            // release and stall a multi-buffered client.
            let (buffer, removed) = match &cur.buffer {
                Some(BufferAssignment::NewBuffer(b)) => (Some(b.clone()), false),
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
            self.repacks.remove(&sid);
            if had {
                self.dirty.insert(sid);
            }
            return had;
        }
        match buffer {
            Some(b) => {
                // A genuinely new buffer object, or damage against the current one, is a real content
                // change; the same buffer re-committed with no damage (e.g. a frame-callback-only commit)
                // is not.
                let new_buffer = self.buffers.get(&sid) != Some(&b);
                let changed = new_buffer || !damage.is_empty();
                self.buffers.insert(sid, b.clone());
                if changed {
                    self.repack_shm(sid, &b, &damage, scale, surface_safe);
                    self.dirty.insert(sid);
                }
                changed
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
    ) {
        let repacks = &mut self.repacks;
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
                    repacks.remove(&sid);
                    return;
                }
            };
            let last_row_start = src_off as usize + (h as usize - 1) * stride as usize;
            match last_row_start.checked_add(tight) {
                Some(max_read) if max_read <= len => {}
                _ => {
                    repacks.remove(&sid);
                    return;
                }
            }
            // Reuse (and partially update) the cache only if it describes the same backing texture.
            let reusable = matches!(
                repacks.get(&sid),
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
                    let cache = repacks.get_mut(&sid).unwrap();
                    for row in y0..y1 {
                        let src =
                            unsafe { ptr.offset(src_off as isize + row as isize * stride as isize) };
                        let dstart = row as usize * tight;
                        unsafe {
                            std::ptr::copy_nonoverlapping(src, cache.bgra[dstart..].as_mut_ptr(), tight);
                        }
                    }
                    cache.damage = Some((0, y0, w, y1 - y0));
                }
                // Full repack (first upload / resize / format change / unmappable damage).
                None => {
                    let mut bgra = vec![0u8; tight * h as usize];
                    for row in 0..h as isize {
                        let src = unsafe { ptr.offset(src_off as isize + row * stride as isize) };
                        let dstart = row as usize * tight;
                        unsafe {
                            std::ptr::copy_nonoverlapping(src, bgra[dstart..].as_mut_ptr(), tight);
                        }
                    }
                    repacks.insert(
                        sid,
                        RepackCache { bgra, tex_w: w, tex_h: h, format: fmt, damage: None },
                    );
                }
            }
        });
        if res.is_err() {
            // Not a `wl_shm` buffer (e.g. a dmabuf) — no CPU pixels to cache.
            self.repacks.remove(&sid);
        }
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
        Some(PopupPlacement { parent_sid: surface_id(&parent), x, y })
    }

    /// Present `root` (a toplevel `wl_surface`) with its full surface tree composited in: the root's own
    /// buffer as the base, every mapped subsurface descendant blended at its parent-relative offset, then
    /// every popup anchored anywhere in this window (menus/dropdowns/tooltips) blended at its resolved
    /// screen offset — plus each popup's own subsurface descendants. CPU-side over-composite (the same
    /// model `server.rs` uses); a GPU/IOSurface root is presented as a single zero-copy texture and its
    /// children are not blended (documented limitation). Returns whether the frame reached the screen.
    pub(crate) fn present_render_root(&mut self, root: &WlSurface) -> bool {
        let pacing = match self.snapshot_surface(root) {
            Some(mut base) => {
                if base.iosurface_id.is_none() && !base.bgra.is_empty() {
                    let popups = self.collect_popups_for_root(root);
                    // The partial-damage upload hint on `base` describes only the ROOT's own changed
                    // region. Once subsurfaces or popups are blended in, their changed pixels lie outside
                    // that rect, so the composited frame must be uploaded in full — widen the hint to the
                    // whole texture. (The CPU repack of each surface was still incremental; only the GPU
                    // upload hint is widened, and correctness is preserved either way since `bgra` is the
                    // complete composite.)
                    let has_overlays =
                        !popups.is_empty() || get_children(root).into_iter().any(|c| &c != root);
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
                // Map the presenter's structured outcome onto frame pacing. Only a visibly Delivered
                // frame advances callbacks/feedback; an Offscreen present or a real output/device error
                // (both previously hidden behind a `false`) is a FAILED present — pacing is retained.
                let sid = base.sid;
                match self.presenter.present(&base) {
                    Ok(dd_display::present::PresentOutcome::Delivered { .. }) => {
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
            self.clear_tree_dirty(root);
        }
        self.pace_tree(root, pacing);
        did_present
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
        surfaces.iter().any(|s| self.dirty.contains(&surface_id(s)))
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
            self.dirty.remove(&surface_id(&s));
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
        let sid = surface_id(surface);
        let buffer = self.buffers.get(&sid)?;
        let (buffer_scale, dst, src) = with_states(surface, |states| {
            let scale = states
                .cached_state
                .get::<SurfaceAttributes>()
                .current()
                .buffer_scale
                .max(1);
            let mut vp = states.cached_state.get::<ViewportCachedState>();
            let cur = vp.current();
            let src = cur.src.map(|r| (r.loc.x, r.loc.y, r.size.w, r.size.h));
            (scale, cur.size(), src)
        });
        // GPU present path: a dmabuf-backed buffer carries a dd IOSurface id (no CPU pixels). Resolve it
        // to a zero-copy IOSurface `SurfaceBuffer` and skip the shm cache.
        if let Some(sb) = self.dmabuf_surface_buffer(sid, buffer, buffer_scale, dst, src) {
            return Some(sb);
        }
        // `wl_shm` path: build from the tight-BGRA cache, repacked (whole buffer or only the damaged rows)
        // at commit time in `repack_shm`. The `wp_viewport` destination/source resolve the logical size
        // and the normalized sample rect exactly as the full-upload path did.
        let cache = self.repacks.get(&sid)?;
        let title = self.titles.get(&sid).cloned().unwrap_or_else(|| "dd".into());
        let (log_w, log_h, uv_rect) =
            logical_size_and_uv(dst, src, cache.tex_w, cache.tex_h, buffer_scale);
        Some(SurfaceBuffer {
            sid,
            width: log_w,
            height: log_h,
            texture_width: cache.tex_w,
            texture_height: cache.tex_h,
            stride: cache.tex_w * 4,
            format: cache.format,
            bgra: cache.bgra.clone(),
            title,
            iosurface_id: None,
            gpu_render: false,
            uv_rect,
            damage: cache.damage,
            // If this surface is an `xdg_popup`, carry its positioner-resolved placement so a windowed
            // presenter can open it as a native popup window at the anchor. This is inert on the default
            // composite-into-parent path (a popup's `SurfaceBuffer` is only blended, which ignores the
            // field, and a toplevel present root is never a popup so this is `None`); it becomes live when
            // `DD_DISPLAY_POPUP_WINDOWS` makes a popup its own present root (see `present_root`).
            popup: self.popup_placement(surface),
        })
    }

    /// Advance frame pacing for every surface in the presented window tree (root + subsurface descendants +
    /// popups + their descendants): drain and fire each surface's `wl_surface.frame` callbacks and answer
    /// its `wp_presentation` feedback. On a failed present the feedback is `discarded`. Draining is
    /// idempotent — a surface with no queued callback fires nothing — so re-presenting a tree that only
    /// partly changed does not double-fire.
    fn pace_tree(&mut self, root: &WlSurface, pacing: FramePacing) {
        let mut surfaces = Vec::new();
        self.collect_tree_surfaces(root, &mut surfaces);
        for (popup, _, _) in self.collect_popups_for_root(root) {
            self.collect_tree_surfaces(&popup, &mut surfaces);
        }
        for s in surfaces {
            self.pace_surface(&s, pacing);
        }
    }

    /// Drain and fire ONE surface's `wl_surface.frame` callbacks and answer its `wp_presentation`
    /// feedback (`presented` on success, `discarded` otherwise). Draining is idempotent — a surface with
    /// no queued callback fires nothing. Split out of [`Self::pace_tree`] so the skip-present path can
    /// pace the committed surface (firing its frame callback so it never stalls) without re-compositing.
    fn pace_surface(&mut self, surface: &WlSurface, pacing: FramePacing) {
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
        let sid = surface_id(surface);
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
        self.send_presentation_feedback(feedback, pacing == FramePacing::Presented);
    }

    /// Retain a surface's `wl_surface.frame` callbacks across a FAILED present so they can be fired on the
    /// next accepted present (the client is not told its frame shipped). Bounded by
    /// [`MAX_RETAINED_CALLBACKS`]: under a permanently-dead presenter the oldest retained callbacks are
    /// dropped (released without `done`) rather than growing the queue without limit.
    fn retain_frame_callbacks(&mut self, sid: u32, callbacks: Vec<WlCallback>) {
        if callbacks.is_empty() {
            return;
        }
        let q = self.retained_callbacks.entry(sid).or_default();
        for cb in callbacks {
            q.push_back(cb);
            while q.len() > MAX_RETAINED_CALLBACKS {
                q.pop_front(); // terminal policy: drop the oldest undeliverable callback
            }
        }
    }

    /// Take (and clear) the callbacks retained for `sid` across earlier failed presents — fired by the
    /// next accepted present.
    fn take_retained_callbacks(&mut self, sid: u32) -> Vec<WlCallback> {
        self.retained_callbacks
            .remove(&sid)
            .map(|q| q.into_iter().collect())
            .unwrap_or_default()
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
        did_present: bool,
    ) {
        if feedback.is_empty() {
            return;
        }
        if !did_present {
            for fb in feedback {
                fb.discarded();
            }
            return;
        }
        self.present_seq = self.present_seq.wrapping_add(1);
        let seq = self.present_seq;
        let time = monotonic_now();
        let refresh = Refresh::fixed(output_refresh());
        for fb in feedback {
            fb.presented(
                &self.output,
                time,
                refresh,
                seq,
                wp_presentation_feedback::Kind::Vsync,
            );
        }
    }

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
/// base surface's origin). Both buffers hold tight BGRA backing-texture pixels; the offset is scaled by
/// the base's backing scale so a HiDPI window blends its children at the right device position. `top` is
/// clipped to the base bounds (a menu extending past the window edge is cropped — a documented limitation
/// of compositing popups into the parent frame rather than into their own native windows). An XRGB `top`
/// (`format == 1`) is treated as fully opaque; an ARGB `top` is straight-alpha blended.
fn blend(base: &mut SurfaceBuffer, top: &SurfaceBuffer, x_logical: i32, y_logical: i32) {
    let (bw, bh) = (base.texture_width, base.texture_height);
    let (tw, th) = (top.texture_width, top.texture_height);
    if bw <= 0 || bh <= 0 || tw <= 0 || th <= 0 {
        return;
    }
    let base_stride = (bw * 4) as usize;
    let top_stride = (tw * 4) as usize;
    if base.bgra.len() < base_stride * bh as usize || top.bgra.len() < top_stride * th as usize {
        return;
    }
    // Backing-store scale of the base texture relative to its logical size; child offsets are logical.
    let s = if base.width > 0 {
        (bw as f64 / base.width as f64).round().max(1.0) as i32
    } else {
        1
    };
    let (ox, oy) = (x_logical * s, y_logical * s);
    let top_opaque = top.format == 1;
    for ty in 0..th {
        let by = oy + ty;
        if by < 0 || by >= bh {
            continue;
        }
        for tx in 0..tw {
            let bx = ox + tx;
            if bx < 0 || bx >= bw {
                continue;
            }
            let ti = ty as usize * top_stride + tx as usize * 4;
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
                let ia = 255 - a;
                for c in 0..3 {
                    base.bgra[bi + c] =
                        ((top.bgra[ti + c] as u32 * a + base.bgra[bi + c] as u32 * ia) / 255) as u8;
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

/// Host `CLOCK_MONOTONIC` as a `Duration` (the `wp_presentation.presented` timestamp). dd runs the guest
/// clock in the host's monotonic domain, so this is the value the guest reads back — mirrors
/// `server.rs`'s `monotonic_now`.
fn monotonic_now() -> Duration {
    let mut ts: libc::timespec = unsafe { std::mem::zeroed() };
    unsafe {
        libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts);
    }
    Duration::new(ts.tv_sec as u64, (ts.tv_nsec as u32) % 1_000_000_000)
}

/// The output refresh interval (time until the next vblank) derived from the advertised mode. 60000 mHz
/// ⇒ ~16.667 ms; clients add multiples of it to predict future vblanks.
fn output_refresh() -> Duration {
    if OUTPUT_REFRESH_MHZ <= 0 {
        return Duration::ZERO;
    }
    Duration::from_nanos(1_000_000_000_000u64 / OUTPUT_REFRESH_MHZ as u64)
}
