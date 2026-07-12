//! `wl_compositor` / `wl_subcompositor` / `wl_shm` handlers and the commit → present path.
//!
//! `commit()` snapshots the committed surface state (buffer, viewport, frame + presentation-feedback
//! callbacks), repacks the `wl_shm` pixels into dd-display's tight-BGRA [`SurfaceBuffer`], hands it to
//! the boxed [`Presenter`], fires the frame callbacks so the client keeps drawing, and answers
//! `wp_presentation` feedback so Chrome/viz's BeginFrameSource keeps ticking. This is the exact seam
//! `server.rs` drives; the difference is Smithay decoded the wire for us.

use std::time::Duration;

use dd_display::present::SurfaceBuffer;

use smithay::{
    reexports::{
        wayland_protocols::wp::presentation_time::server::wp_presentation_feedback,
        wayland_server::{
            protocol::{wl_buffer::WlBuffer, wl_shm, wl_surface::WlSurface},
            Client,
        },
    },
    utils::Size,
    wayland::{
        buffer::BufferHandler,
        compositor::{
            get_children, get_parent, is_sync_subsurface, with_states, BufferAssignment,
            CompositorClientState, CompositorHandler, CompositorState, SubsurfaceCachedState,
            SurfaceAttributes,
        },
        presentation::{PresentationFeedbackCachedState, Refresh},
        shm::{with_buffer_contents, ShmHandler, ShmState},
        viewporter::ViewportCachedState,
    },
};

use crate::{surface_id, ClientState, DdState, OUTPUT_REFRESH_MHZ};

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
        self.remember_buffer(surface);

        // A synchronized subsurface is presented as part of its parent's atomic commit; do not present now
        // (its buffer is already remembered above and its frame callbacks are drained when the root
        // presents). Presenting here would show a half-applied tree.
        if is_sync_subsurface(surface) {
            return;
        }

        let Some(root) = self.window_root(surface) else {
            return;
        };
        self.present_render_root(&root);
    }

    /// Record (or clear) the surface's latest committed `wl_shm` buffer. A bufferless commit leaves the
    /// previous buffer in place (the on-screen content persists); an explicit detach removes it.
    fn remember_buffer(&mut self, surface: &WlSurface) {
        let sid = surface_id(surface);
        let assignment = with_states(surface, |states| {
            let mut attrs = states.cached_state.get::<SurfaceAttributes>();
            match &attrs.current().buffer {
                Some(BufferAssignment::NewBuffer(b)) => Some(Some(b.clone())),
                Some(BufferAssignment::Removed) => Some(None),
                None => None,
            }
        });
        match assignment {
            Some(Some(b)) => {
                self.buffers.insert(sid, b);
            }
            Some(None) => {
                self.buffers.remove(&sid);
            }
            None => {}
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

    /// Present `root` (a toplevel `wl_surface`) with its full surface tree composited in: the root's own
    /// buffer as the base, every mapped subsurface descendant blended at its parent-relative offset, then
    /// every popup anchored anywhere in this window (menus/dropdowns/tooltips) blended at its resolved
    /// screen offset — plus each popup's own subsurface descendants. CPU-side over-composite (the same
    /// model `server.rs` uses); a GPU/IOSurface root is presented as a single zero-copy texture and its
    /// children are not blended (documented limitation). Returns whether the frame reached the screen.
    pub(crate) fn present_render_root(&mut self, root: &WlSurface) -> bool {
        let did_present = match self.snapshot_surface(root) {
            Some(mut base) => {
                if base.iosurface_id.is_none() && !base.bgra.is_empty() {
                    self.blend_subtree(&mut base, root, 0, 0);
                    for (popup, ox, oy) in self.collect_popups_for_root(root) {
                        if let Some(psb) = self.snapshot_surface(&popup) {
                            blend(&mut base, &psb, ox, oy);
                        }
                        self.blend_subtree(&mut base, &popup, ox, oy);
                    }
                }
                self.presenter.present(&base)
            }
            None => false,
        };
        self.pace_tree(root, did_present);
        did_present
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
        let buffer = self.buffers.get(&sid)?.clone();
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
        self.build_surface_buffer(sid, &buffer, buffer_scale, dst, src)
    }

    /// Advance frame pacing for every surface in the presented window tree (root + subsurface descendants +
    /// popups + their descendants): drain and fire each surface's `wl_surface.frame` callbacks and answer
    /// its `wp_presentation` feedback. On a failed present the feedback is `discarded`. Draining is
    /// idempotent — a surface with no queued callback fires nothing — so re-presenting a tree that only
    /// partly changed does not double-fire.
    fn pace_tree(&mut self, root: &WlSurface, did_present: bool) {
        let mut surfaces = Vec::new();
        self.collect_tree_surfaces(root, &mut surfaces);
        for (popup, _, _) in self.collect_popups_for_root(root) {
            self.collect_tree_surfaces(&popup, &mut surfaces);
        }
        for s in surfaces {
            let (callbacks, feedback) = with_states(&s, |states| {
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
            let t = self.now_ms();
            for cb in callbacks {
                cb.done(t);
            }
            self.send_presentation_feedback(feedback, did_present);
        }
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

    /// Repack a committed `wl_shm` buffer into dd-display's tight-BGRA [`SurfaceBuffer`]. The backing
    /// texture is the full buffer; the logical size is the `wp_viewport` destination if set, else the
    /// buffer pixels divided by `wl_surface.buffer_scale` (so a HiDPI 2x buffer maps to logical units).
    fn build_surface_buffer(
        &self,
        sid: u32,
        buffer: &WlBuffer,
        buffer_scale: i32,
        dst: Option<Size<i32, smithay::utils::Logical>>,
        src: Option<(f64, f64, f64, f64)>,
    ) -> Option<SurfaceBuffer> {
        // GPU present path: a dmabuf-backed buffer carries a dd IOSurface id (no CPU pixels). Resolve
        // it to a zero-copy IOSurface `SurfaceBuffer` and skip the shm repack. A non-dmabuf buffer
        // returns `None` here and falls through to the `wl_shm` copy below.
        if let Some(sb) = self.dmabuf_surface_buffer(sid, buffer, buffer_scale, dst, src) {
            return Some(sb);
        }
        let title = self.titles.get(&sid).cloned().unwrap_or_else(|| "dd".into());
        let res = with_buffer_contents(buffer, |ptr, _len, data| {
            let w = data.width;
            let h = data.height;
            let stride = data.stride;
            let src_off = data.offset;
            let fmt = match data.format {
                wl_shm::Format::Xrgb8888 => 1u32, // opaque (dd-display convention: format==1 ⇒ XRGB)
                _ => 0u32,                        // ARGB8888 (and anything else): honour alpha
            };
            // Tight BGRA copy of the backing texture, honouring the pool offset + row stride.
            let tight = (w * 4) as usize;
            let mut bgra = vec![0u8; tight * h as usize];
            for row in 0..h as isize {
                let src = unsafe { ptr.offset(src_off as isize + row * stride as isize) };
                let dstart = row as usize * tight;
                unsafe {
                    std::ptr::copy_nonoverlapping(src, bgra[dstart..].as_mut_ptr(), tight);
                }
            }
            (w, h, fmt, bgra)
        })
        .ok()?;
        let (tex_w, tex_h, fmt, bgra) = res;

        // wp_viewport source rectangle (given in post-buffer-scale/logical coords) → a normalized
        // sample rect in the backing texture, so a client that renders into an oversized target and
        // crops via the viewport (Chrome's fractional-scale path) presents only the requested region.
        // `dst` (or, absent it, the buffer pixels / buffer_scale) is the on-screen logical size.
        let (log_w, log_h, uv_rect) = logical_size_and_uv(dst, src, tex_w, tex_h, buffer_scale);

        Some(SurfaceBuffer {
            sid,
            width: log_w,
            height: log_h,
            texture_width: tex_w,
            texture_height: tex_h,
            stride: tex_w * 4,
            format: fmt,
            bgra,
            title,
            iosurface_id: None,
            gpu_render: false,
            uv_rect,
        })
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
