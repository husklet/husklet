//! `compose_frame`: walk a window's tree and produce the ordered list of layers to present, with each
//! layer's root-space offset and damage — plus `is_tree_dirty`, the conservative-occlusion skip test.
//!
//! Ported from `hl-compositor`'s `present_tree` / `blend_subtree` / `collect_popups_for_root` (the
//! composite walk and z-order) and `tree_dirty` / `opaque_covers_root_rect` (the skip-redundant-present
//! decision). The neutral policy emits an ORDERED LAYER LIST (bottom → top) instead of blending pixels:
//! a real adapter either composites them into one frame or presents each. The order is exactly the
//! blend order the ported code uses — root, its subsurface descendants, then popups (by depth) and
//! their descendants.

use hl_log::{hl_debug, hl_span, tag};

use crate::scene::model::{Format, PresentableImage, Rect, Scene, SurfaceId};

use super::popup::popup_placement;

/// One layer of a composed frame: the presentable image, its offset within the present root, and the
/// root-space damage attributable to it this cycle (empty when the layer is clean).
#[derive(Clone, Debug, PartialEq)]
pub struct PresentItem {
    pub image: PresentableImage,
    pub x: i32,
    pub y: i32,
    pub damage: Vec<Rect>,
}

/// A composed frame for one present root: its ordered layers (bottom → top).
#[derive(Clone, Debug, PartialEq)]
pub struct Frame {
    pub root: SurfaceId,
    pub items: Vec<PresentItem>,
}

impl Frame {
    /// The present order (surface ids, bottom → top) — what a test asserts against a `FakePresenter`'s
    /// recorded calls.
    pub fn present_order(&self) -> Vec<SurfaceId> {
        self.items.iter().map(|i| i.image.surface).collect()
    }

    /// The union of every layer's damage, in root space — the frame's overall changed region.
    pub fn damage(&self) -> Vec<Rect> {
        let mut out: Vec<Rect> = Vec::new();
        for item in &self.items {
            out.extend(item.damage.iter().copied());
        }
        out
    }
}

/// Compose the tree rooted at `root` into an ordered layer list. Returns `None` if the root has no
/// committed buffer (nothing to present). Each mapped surface in the tree — the root, its subsurface
/// descendants, and every popup belonging to the root (and their descendants) — becomes one
/// [`PresentItem`] at its accumulated root-relative offset, in composite order.
pub fn compose_frame(scene: &Scene, root: SurfaceId) -> Option<Frame> {
    let _span = hl_span!(tag::COMPOSITOR, "compose");
    // The root must have content; if not, there is nothing to present.
    scene.get(root)?.buffer?;

    let mut layers: Vec<(SurfaceId, i32, i32)> = Vec::new();
    scene.collect_subtree_offsets(root, 0, 0, &mut layers);
    for (popup, ox, oy) in scene.collect_popups_for_root(root) {
        scene.collect_subtree_offsets(popup, ox, oy, &mut layers);
    }

    let mut items = Vec::new();
    for (sid, x, y) in layers {
        let Some(item) = present_item(scene, sid, x, y) else {
            continue; // a surface with no committed buffer contributes no layer
        };
        items.push(item);
    }
    let frame = Frame { root, items };
    let (w, h) = frame.items.first().map(|i| (i.image.width, i.image.height)).unwrap_or((0, 0));
    hl_debug!(tag::COMPOSITOR, "compose root={} layers={} {}x{}", root.0, frame.items.len(), w, h);
    Some(frame)
}

/// Build the [`PresentItem`] for one surface at root-space offset `(x, y)`, or `None` if it has no
/// buffer. The image mirrors `snapshot_surface`: logical size, format, GPU flag, and (for a popup) its
/// resolved native placement.
fn present_item(scene: &Scene, sid: SurfaceId, x: i32, y: i32) -> Option<PresentItem> {
    let surface = scene.get(sid)?;
    let buffer = surface.buffer?;
    let (w, h) = buffer.logical_size(&surface.viewport);
    // If a `wp_viewport` src crop and/or dst scale is set, hand the presenter the source rectangle to
    // sample IN BUFFER PIXELS (logical src × buffer_scale; whole buffer when only a dst scale is set), so
    // it rasterizes exactly the cropped+scaled region into `w`×`h`. Absent a viewport, present verbatim.
    let present_crop = if surface.viewport.src.is_some() || surface.viewport.dst.is_some() {
        let s = buffer.buffer_scale.max(1) as f64;
        Some(match surface.viewport.src {
            Some((sx, sy, sw, sh)) => (sx * s, sy * s, sw * s, sh * s),
            None => (0.0, 0.0, buffer.tex_w as f64, buffer.tex_h as f64),
        })
    } else {
        None
    };
    let image = PresentableImage {
        surface: sid,
        width: w,
        height: h,
        format: if buffer.format.is_opaque() { Format::Xrgb8888 } else { Format::Argb8888 },
        gpu: buffer.gpu,
        popup: popup_placement(scene, sid),
        present_crop,
    };
    let damage = layer_damage(scene, sid, x, y, w, h);
    Some(PresentItem { image, x, y, damage })
}

/// The root-space damage a layer contributes this cycle: its accumulated damage bounding box (or its
/// whole rectangle when it is dirty but carries no rects — a fresh buffer / resize), translated by the
/// layer offset. A clean layer contributes nothing.
fn layer_damage(scene: &Scene, sid: SurfaceId, x: i32, y: i32, w: i32, h: i32) -> Vec<Rect> {
    if !scene.is_dirty(sid) {
        return Vec::new();
    }
    let surface = scene.get(sid).expect("dirty surface exists");
    let rect = surface
        .damage
        .bounding_box()
        .unwrap_or(Rect::new(0, 0, w, h))
        .translate(x, y);
    vec![rect]
}

/// Whether any surface in `root`'s presented tree has a change that is actually VISIBLE — the
/// skip-redundant-present decision. Conservative opaque-region occlusion: a dirty surface whose whole
/// logical rectangle is provably covered by the opaque region of a surface composited ABOVE it
/// contributes no visible change. Any doubt (unknown geometry, partial/absent opaque region) keeps the
/// tree dirty, so a present is never wrongly skipped. Exact port of `tree_dirty`.
pub fn is_tree_dirty(scene: &Scene, root: SurfaceId) -> bool {
    if !scene.any_dirty() {
        return false;
    }
    let layers = collect_occlusion_layers(scene, root);
    // Each layer's root-space rectangle (None ⇒ unknown geometry).
    let rects: Vec<Option<Rect>> = layers
        .iter()
        .map(|(s, x, y)| scene.get(*s).and_then(|surf| surf.logical_size()).map(|(w, h)| Rect::new(*x, *y, w, h)))
        .collect();

    for (i, (sid, _, _)) in layers.iter().enumerate() {
        if !scene.is_dirty(*sid) {
            continue;
        }
        let Some(rect) = rects[i] else {
            return true; // unknown geometry: cannot prove occlusion, treat as visible
        };
        let occluded = layers[i + 1..]
            .iter()
            .any(|(up, ux, uy)| opaque_covers(scene, *up, *ux, *uy, &rect));
        if !occluded {
            return true;
        }
    }
    false
}

/// The presented tree in composite order (bottom → top): root subtree, then each popup's subtree at its
/// root-space offset. Port of `collect_occlusion_layers`.
fn collect_occlusion_layers(scene: &Scene, root: SurfaceId) -> Vec<(SurfaceId, i32, i32)> {
    let mut layers = Vec::new();
    scene.collect_subtree_offsets(root, 0, 0, &mut layers);
    for (popup, ox, oy) in scene.collect_popups_for_root(root) {
        scene.collect_subtree_offsets(popup, ox, oy, &mut layers);
    }
    layers
}

/// Whether `up`'s opaque region — translated from its surface-local space to root space by `(ux, uy)` —
/// provably covers the whole root-space rectangle `rect`. A `None` opaque region proves nothing. Port
/// of `opaque_covers_root_rect` (single-rect neutral opaque region; a conservative subset is safe).
///
/// A surface with NO committed buffer draws nothing — `compose_frame` emits no layer for it — so it can
/// never occlude anything, regardless of a stale `set_opaque_region` left over from before a detach.
/// Requiring a live buffer here upholds the module contract ("a present is never wrongly skipped"): an
/// unmapped cover must not hide the damage below it.
fn opaque_covers(scene: &Scene, up: SurfaceId, ux: i32, uy: i32, rect: &Rect) -> bool {
    match scene.get(up) {
        Some(s) if s.buffer.is_some() => match s.opaque_region {
            Some(region) => region.translate(ux, uy).contains_rect(rect),
            None => false,
        },
        _ => false,
    }
}
