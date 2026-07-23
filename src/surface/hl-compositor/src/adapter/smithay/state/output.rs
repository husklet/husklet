use super::*;

/// Builds advertised Wayland outputs from neutral scene outputs.
pub(super) struct WaylandOutput<'a> {
    display: &'a DisplayHandle,
    scene: &'a Output,
}

impl<'a> WaylandOutput<'a> {
    pub(super) fn new(display: &'a DisplayHandle, scene: &'a Output) -> Self {
        Self { display, scene }
    }

    pub(super) fn build(self) -> (WlOutputHandle, GlobalId) {
        let scene = self.scene;
        // Values sourced from the scene so `wl_output` reports exactly what the scene composites onto.
        let name = scene.name.clone();
        let (mode_w, mode_h) = (scene.mode_w, scene.mode_h);
        let refresh_mhz = scene.refresh_mhz;
        let scale = scene.scale.max(1);
        let transform = Transform::from(scene.transform);

        // Physical size in mm assuming ~96 dpi (25.4 mm/inch) — a plausible value for toolkits that derive DPI
        // from it; the pixel mode + scale below are the load-bearing fidelity, not the millimetre size.
        let phys_w_mm = (mode_w as f64 / 96.0 * 25.4).round() as i32;
        let phys_h_mm = (mode_h as f64 / 96.0 * 25.4).round() as i32;

        let output = WlOutputHandle::new(
            name,
            PhysicalProperties {
                size: (phys_w_mm, phys_h_mm).into(),
                subpixel: Subpixel::Unknown,
                make: "hl".into(),
                model: "hl-virtual".into(),
            },
        );
        let global = output.create_global::<HlState>(self.display);

        // `refresh` on a smithay `Mode` is millihertz (same unit as the scene's `refresh_mhz`). The location is
        // the output's layout position — smithay reports it as `wl_output.geometry.x/y` and derives xdg-output's
        // `logical_position` from it, so a multi-output layout advertises each monitor at its own coordinates.
        let mode = OutputMode {
            size: (mode_w, mode_h).into(),
            refresh: refresh_mhz as i32,
        };
        output.change_current_state(
            Some(mode),
            Some(transform),
            Some(Scale::Integer(scale)),
            Some((scene.pos_x, scene.pos_y).into()),
        );
        output.set_preferred(mode);

        (output, global)
    }
}

/// Map a Smithay `xdg_positioner` [`PositionerState`] onto the neutral [`Positioner`] value type the
/// scene's `place_popup` resolves. A straight field/enum translation — the placement math itself
/// (anchor/gravity/offset + flip/slide/resize) lives in `scene::service::popup`, not here, so the neutral
/// core owns the policy and the adapter only decodes the wire.
impl From<&PositionerState> for Positioner {
    fn from(p: &PositionerState) -> Self {
        Self {
            anchor_rect: Rect::new(
                p.anchor_rect.loc.x,
                p.anchor_rect.loc.y,
                p.anchor_rect.size.w,
                p.anchor_rect.size.h,
            ),
            size: (p.rect_size.w, p.rect_size.h),
            anchor: p.anchor_edges.into(),
            gravity: p.gravity.into(),
            constraint_adjustment: p.constraint_adjustment.into(),
            offset: (p.offset.x, p.offset.y),
        }
    }
}

/// Translate the `xdg_positioner.set_anchor` edge onto the neutral [`Anchor`].
impl From<WireAnchor> for Anchor {
    fn from(a: WireAnchor) -> Self {
        match a {
            WireAnchor::None => Anchor::None,
            WireAnchor::Top => Anchor::Top,
            WireAnchor::Bottom => Anchor::Bottom,
            WireAnchor::Left => Anchor::Left,
            WireAnchor::Right => Anchor::Right,
            WireAnchor::TopLeft => Anchor::TopLeft,
            WireAnchor::BottomLeft => Anchor::BottomLeft,
            WireAnchor::TopRight => Anchor::TopRight,
            WireAnchor::BottomRight => Anchor::BottomRight,
            _ => Anchor::None,
        }
    }
}

/// Translate the `xdg_positioner.set_gravity` direction onto the neutral [`Gravity`].
impl From<WireGravity> for Gravity {
    fn from(g: WireGravity) -> Self {
        match g {
            WireGravity::None => Gravity::None,
            WireGravity::Top => Gravity::Top,
            WireGravity::Bottom => Gravity::Bottom,
            WireGravity::Left => Gravity::Left,
            WireGravity::Right => Gravity::Right,
            WireGravity::TopLeft => Gravity::TopLeft,
            WireGravity::BottomLeft => Gravity::BottomLeft,
            WireGravity::TopRight => Gravity::TopRight,
            WireGravity::BottomRight => Gravity::BottomRight,
            _ => Gravity::None,
        }
    }
}

/// Translate the `xdg_positioner.set_constraint_adjustment` bitmask onto the neutral per-axis
/// flip/slide/resize flags the scene applies in that order.
impl From<WireConstraint> for ConstraintAdjustment {
    fn from(c: WireConstraint) -> Self {
        Self {
            flip_x: c.contains(WireConstraint::FlipX),
            flip_y: c.contains(WireConstraint::FlipY),
            slide_x: c.contains(WireConstraint::SlideX),
            slide_y: c.contains(WireConstraint::SlideY),
            resize_x: c.contains(WireConstraint::ResizeX),
            resize_y: c.contains(WireConstraint::ResizeY),
        }
    }
}

/// Reduce a committed `wl_surface.set_input_region` into the neutral scene's single-[`Rect`] input region
/// (which gates pointer hit-testing in `surface_at`/`accepts_input_at`). `None` — the client never set a
/// region, or set it to null — means the WHOLE surface accepts input (the scene's `None`). A set region is
/// reduced to the bounding box of its ADDITIVE rectangles: EXACT for the common single-rect region a
/// toolkit sets (e.g. GTK excluding its CSD shadow from input), and a safe superset for a shaped one. An
/// EMPTY region (a client making a surface click-through) has no additive rects and reduces to a zero-area
/// `Rect`, which `accepts_input_at` rejects everywhere — so that surface correctly receives no pointer
/// input. Without this the request would be silently dropped and every surface would accept input over its
/// whole rectangle regardless of what the client requested.
pub(super) struct Region<'a> {
    attributes: &'a Option<RegionAttributes>,
}

impl<'a> Region<'a> {
    pub(super) fn new(attributes: &'a Option<RegionAttributes>) -> Self {
        Self { attributes }
    }

    pub(super) fn input(&self) -> Option<Rect> {
        let attrs = self.attributes.as_ref()?;
        Some(Self::additive_bounds(attrs).unwrap_or(Rect::new(0, 0, 0, 0)))
    }

    /// Reduce a committed `wl_surface.set_opaque_region` into the neutral scene's single-[`Rect`] opaque region
    /// — CONSERVATIVELY, because it drives the occlusion present-skip (`is_tree_dirty` → `opaque_covers`) where
    /// OVER-claiming opacity could wrongly hide a surface below and drop its update. Only a region that is
    /// exactly one additive rectangle (the common case — a client marking its whole opaque window so the
    /// compositor may skip redundant work behind it) is trusted verbatim. Anything a single rect cannot model
    /// without over-claiming — a subtracted hole, or multiple disjoint rects — reduces to `None` (proves
    /// nothing opaque), so a present is never wrongly skipped. `None` in (unset) ⇒ `None` out (the whole
    /// surface may be transparent).
    pub(super) fn opaque(&self) -> Option<Rect> {
        match self.attributes.as_ref()?.rects.as_slice() {
            [(RectangleKind::Add, r)] if r.size.w > 0 && r.size.h > 0 => {
                Some(Rect::new(r.loc.x, r.loc.y, r.size.w, r.size.h))
            }
            _ => None,
        }
    }

    /// The bounding box of a region's ADDITIVE (`Add`) rectangles, or `None` if it has none. Subtract rects and
    /// degenerate (zero-area) rects are ignored — a single `Rect` cannot model a hole, and the resulting
    /// superset is the SAFE direction for an input region (over-accepting input is a hint, never a correctness
    /// hazard, unlike over-claiming opacity — see [`map_input_region`] vs [`map_opaque_region`]).
    pub(super) fn additive_bounds(attrs: &RegionAttributes) -> Option<Rect> {
        // (min_x, min_y, max_right, max_bottom) accumulated over the additive rects.
        let mut bounds: Option<(i32, i32, i32, i32)> = None;
        for (kind, r) in &attrs.rects {
            if !matches!(kind, RectangleKind::Add) || r.size.w <= 0 || r.size.h <= 0 {
                continue;
            }
            let (x0, y0, x1, y1) = (r.loc.x, r.loc.y, r.loc.x + r.size.w, r.loc.y + r.size.h);
            bounds = Some(match bounds {
                Some((mx, my, mr, mb)) => (mx.min(x0), my.min(y0), mr.max(x1), mb.max(y1)),
                None => (x0, y0, x1, y1),
            });
        }
        bounds.map(|(mx, my, mr, mb)| Rect::new(mx, my, mr - mx, mb - my))
    }
}

/// Map Smithay's `wl_output::Transform` (the wire enum `wl_surface.set_buffer_transform` speaks) onto the
/// neutral [`BufferTransform`]. A straight enum translation; the rotation/flip math itself lives in the
/// neutral `BufferTransform` (dimension swap) and the presenter (pixel remap), not here.
impl From<smithay::reexports::wayland_server::protocol::wl_output::Transform> for BufferTransform {
    fn from(t: smithay::reexports::wayland_server::protocol::wl_output::Transform) -> Self {
        use smithay::reexports::wayland_server::protocol::wl_output::Transform as WlT;
        match t {
            WlT::Normal => BufferTransform::Normal,
            WlT::_90 => BufferTransform::_90,
            WlT::_180 => BufferTransform::_180,
            WlT::_270 => BufferTransform::_270,
            WlT::Flipped => BufferTransform::Flipped,
            WlT::Flipped90 => BufferTransform::Flipped90,
            WlT::Flipped180 => BufferTransform::Flipped180,
            WlT::Flipped270 => BufferTransform::Flipped270,
            _ => BufferTransform::Normal,
        }
    }
}
