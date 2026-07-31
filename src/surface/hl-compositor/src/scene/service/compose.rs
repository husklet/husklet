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

use crate::scene::{
    model::{Format, PresentableImage, Rect, Scene, Surface, SurfaceId},
    port::{PresentFrame, PresentLayer, PresentTiming},
};

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
impl Scene {
    /// Build one submission batch per native-window role. Subsurfaces are composited into their owning
    /// toplevel or popup; popups are never flattened into the toplevel drawable.
    pub fn compose_present_frames(
        &self,
        root: SurfaceId,
        output: crate::scene::model::OutputId,
        timing: PresentTiming,
    ) -> Option<Vec<PresentFrame>> {
        self.get(root)?.buffer?;
        let mut roles = vec![(root, 0, 0)];
        roles.extend(self.collect_popups_for_root(root));
        roles
            .into_iter()
            .map(|(role, origin_x, origin_y)| {
                let mut surfaces = Vec::new();
                self.collect_subtree_offsets(role, 0, 0, &mut surfaces);
                let layers = surfaces
                    .into_iter()
                    .filter_map(|(surface, x, y)| {
                        self.present_item(surface, x, y).map(|item| PresentLayer {
                            image: item.image,
                            x,
                            y,
                            damage: item.damage,
                        })
                    })
                    .collect::<Vec<_>>();
                (!layers.is_empty()).then_some(PresentFrame {
                    output,
                    role,
                    origin: (origin_x, origin_y),
                    layers,
                    timing,
                })
            })
            .collect()
    }

    pub fn compose_frame(&self, root: SurfaceId) -> Option<Frame> {
        let _span = hl_span!(tag::COMPOSITOR, "compose");
        // The root must have content; if not, there is nothing to present.
        self.get(root)?.buffer?;

        let mut layers: Vec<(SurfaceId, i32, i32)> = Vec::new();
        self.collect_subtree_offsets(root, 0, 0, &mut layers);
        for (popup, ox, oy) in self.collect_popups_for_root(root) {
            self.collect_subtree_offsets(popup, ox, oy, &mut layers);
        }

        let mut items = Vec::new();
        for (sid, x, y) in layers {
            let Some(item) = self.present_item(sid, x, y) else {
                continue; // a surface with no committed buffer contributes no layer
            };
            items.push(item);
        }
        let frame = Frame { root, items };
        let (w, h) = frame
            .items
            .first()
            .map(|i| (i.image.width, i.image.height))
            .unwrap_or((0, 0));
        hl_debug!(
            tag::COMPOSITOR,
            "compose root={} layers={} {}x{}",
            root.0,
            frame.items.len(),
            w,
            h
        );
        Some(frame)
    }

    /// Build the [`PresentItem`] for one surface at root-space offset `(x, y)`, or `None` if it has no
    /// buffer. The image mirrors `snapshot_surface`: logical size, format, GPU flag, and (for a popup) its
    /// resolved native placement.
    fn present_item(&self, sid: SurfaceId, x: i32, y: i32) -> Option<PresentItem> {
        let surface = self.get(sid)?;
        let buffer = surface.buffer?;
        let (w, h) = buffer.logical_size(&surface.viewport, surface.transform);
        // If a `wp_viewport` src crop and/or dst scale is set, hand the presenter the source rectangle to
        // sample IN BUFFER PIXELS (logical src × buffer_scale; whole buffer when only a dst scale is set), so
        // it rasterizes exactly the cropped+scaled region into `w`×`h`. Absent a viewport, present verbatim.
        let present_crop = if surface.viewport.src.is_some() || surface.viewport.dst.is_some() {
            let s = buffer.buffer_scale.max(1) as f64;
            Some(match surface.viewport.src {
                // A `wp_viewport` src crop is stated in surface-local (post buffer-transform) coordinates;
                // scaling by `buffer_scale` puts it in surface-space PIXELS, the space the presenter samples
                // the (already un-rotated) buffer in.
                Some((sx, sy, sw, sh)) => (sx * s, sy * s, sw * s, sh * s),
                // No src crop: sample the whole SURFACE-space image. A 90°/270° buffer transform swaps the
                // buffer's width/height into surface space, so the default rect must use the swapped size (a
                // no-op for Normal / 180°) — else a dst-only viewport over a rotated buffer would sample the
                // wrong extent.
                None => {
                    let (fw, fh) = surface.transform.surface_size(buffer.tex_w, buffer.tex_h);
                    (0.0, 0.0, fw as f64, fh as f64)
                }
            })
        } else {
            None
        };
        let image = PresentableImage {
            surface: sid,
            width: w,
            height: h,
            format: if buffer.format.is_opaque() || declares_fully_opaque(surface, w, h) {
                Format::Xrgb8888
            } else {
                Format::Argb8888
            },
            gpu: buffer.gpu,
            popup: self.popup_placement(sid),
            present_crop,
            transform: surface.transform,
        };
        let damage = self.layer_damage(sid, x, y, w, h);
        Some(PresentItem {
            image,
            x,
            y,
            damage,
        })
    }

    /// The root-space damage a layer contributes this cycle. Preserve separate rectangles so platform
    /// presenters can restrict GPU work instead of expanding sparse updates to one large bounding box.
    /// A fresh buffer or resize without explicit damage conservatively damages the complete layer.
    fn layer_damage(&self, sid: SurfaceId, x: i32, y: i32, w: i32, h: i32) -> Vec<Rect> {
        if !self.is_dirty(sid) {
            return Vec::new();
        }
        let surface = self.get(sid).expect("dirty surface exists");
        if surface.damage.is_empty() {
            return vec![Rect::new(x, y, w, h)];
        }
        surface
            .damage
            .rects()
            .iter()
            .map(|rect| rect.translate(x, y))
            .collect()
    }

    /// Whether any surface in `root`'s presented tree has a change that is actually VISIBLE — the
    /// skip-redundant-present decision. Conservative opaque-region occlusion: a dirty surface whose whole
    /// logical rectangle is provably covered by the opaque region of a surface composited ABOVE it
    /// contributes no visible change. Any doubt (unknown geometry, partial/absent opaque region) keeps the
    /// tree dirty, so a present is never wrongly skipped. Exact port of `tree_dirty`.
    pub fn is_tree_dirty(&self, root: SurfaceId) -> bool {
        if !self.any_dirty() {
            return false;
        }
        let layers = self.occlusion_layers(root);
        // Each layer's root-space rectangle (None ⇒ unknown geometry).
        let rects: Vec<Option<Rect>> = layers
            .iter()
            .map(|(s, x, y)| {
                self.get(*s)
                    .and_then(|surf| surf.logical_size())
                    .map(|(w, h)| Rect::new(*x, *y, w, h))
            })
            .collect();

        for (i, (sid, _, _)) in layers.iter().enumerate() {
            if !self.is_dirty(*sid) {
                continue;
            }
            let Some(rect) = rects[i] else {
                return true; // unknown geometry: cannot prove occlusion, treat as visible
            };
            let occluded = layers[i + 1..]
                .iter()
                .any(|(up, ux, uy)| opaque_covers(self, *up, *ux, *uy, &rect));
            if !occluded {
                return true;
            }
        }
        false
    }

    /// The presented tree in composite order (bottom → top): root subtree, then each popup's subtree at its
    /// root-space offset. Port of `collect_occlusion_layers`.
    fn occlusion_layers(&self, root: SurfaceId) -> Vec<(SurfaceId, i32, i32)> {
        let mut layers = Vec::new();
        self.collect_subtree_offsets(root, 0, 0, &mut layers);
        for (popup, ox, oy) in self.collect_popups_for_root(root) {
            self.collect_subtree_offsets(popup, ox, oy, &mut layers);
        }
        layers
    }
}

/// Whether the client declared its WHOLE presented surface opaque via `wl_surface.set_opaque_region`.
///
/// An ARGB8888 buffer's alpha is only meaningful where the client did not say otherwise. Chrome (and other
/// Ozone/Wayland clients) allocate `AR24` unconditionally, leave alpha unwritten in regions they render as
/// part of an opaque window, and then declare the whole window opaque. Honouring the buffer's alpha there
/// punches the unwritten pixels through to the desktop. The declaration is the client's own statement about
/// its pixels, so a region covering the entire presented rect is taken at face value and presented opaque.
///
/// Deliberately all-or-nothing: the presenter forces opacity per surface, so a PARTIAL region cannot be
/// honoured without wrongly flattening the rest. Partial or absent regions keep the buffer's alpha, which is
/// what cursors, rounded popups and translucent subsurfaces rely on.
fn declares_fully_opaque(surface: &Surface, w: i32, h: i32) -> bool {
    surface
        .opaque_region
        .is_some_and(|region| region.contains(&Rect::new(0, 0, w, h)))
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
            Some(region) => region.translate(ux, uy).contains(rect),
            None => false,
        },
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scene::model::{BufferState, Format};
    use crate::scene::service::{commit_surface, Commit};

    #[test]
    fn composed_frame_preserves_sparse_damage_rectangles() {
        let mut scene = Scene::new();
        let surface = scene.create_surface();
        let buffer = BufferState {
            tex_w: 100,
            tex_h: 80,
            format: Format::Argb8888,
            buffer_scale: 1,
            gpu: true,
        };
        let mut commit = Commit::attach(buffer);
        commit.damage = vec![Rect::new(1, 2, 3, 4), Rect::new(70, 60, 5, 6)];
        assert!(commit_surface(&mut scene, surface, commit));

        assert_eq!(
            scene.compose_frame(surface).unwrap().items[0].damage,
            vec![Rect::new(1, 2, 3, 4), Rect::new(70, 60, 5, 6)]
        );
    }

    /// Attach a 100x80 ARGB8888 buffer, optionally declaring `opaque` via `set_opaque_region`, and report
    /// the format the presenter is handed.
    fn presented_format(opaque: Option<Rect>) -> Format {
        let mut scene = Scene::new();
        let surface = scene.create_surface();
        let mut commit = Commit::attach(BufferState {
            tex_w: 100,
            tex_h: 80,
            format: Format::Argb8888,
            buffer_scale: 1,
            gpu: true,
        });
        commit.opaque_region = Some(opaque);
        assert!(commit_surface(&mut scene, surface, commit));
        scene.compose_frame(surface).unwrap().items[0].image.format
    }

    #[test]
    fn whole_surface_opaque_region_presents_argb_as_opaque() {
        // Chrome's case: an AR24 buffer whose alpha is unwritten in places, plus a whole-window
        // `set_opaque_region`. Honouring the declaration is what stops those pixels reaching the desktop.
        assert_eq!(presented_format(Some(Rect::new(0, 0, 100, 80))), Format::Xrgb8888);
        // A larger declared region still covers the surface.
        assert_eq!(presented_format(Some(Rect::new(0, 0, 200, 200))), Format::Xrgb8888);
    }

    #[test]
    fn partial_or_absent_opaque_region_keeps_client_alpha() {
        // Nothing declared: a translucent cursor / rounded popup must keep its alpha.
        assert_eq!(presented_format(None), Format::Argb8888);
        // Partially declared: the presenter forces opacity per surface, so a partial claim is not honoured.
        assert_eq!(presented_format(Some(Rect::new(0, 0, 100, 40))), Format::Argb8888);
        assert_eq!(presented_format(Some(Rect::new(10, 0, 100, 80))), Format::Argb8888);
    }
}
