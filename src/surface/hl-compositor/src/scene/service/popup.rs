//! `place_popup`: resolve an `xdg_positioner` to a popup's on-screen geometry — anchor → gravity →
//! offset, then the flip → slide → resize constraint adjustment that keeps a menu/dropdown on-screen.
//!
//! This is the neutral REIMPLEMENTATION of what `hl-compositor`'s `constrain_popup` delegated to
//! Smithay's `PositionerState::get_unconstrained_geometry` — the algorithm the neutral core cannot call
//! into Smithay for. It follows the `xdg_shell` `xdg_positioner` spec (the same math wlroots/weston
//! implement); the resolved geometry feeds `popup_placement` (parent-relative offset) exactly as the
//! ported `popup_placement` / `popup_offset_to_toplevel` expect.

use crate::scene::model::{
    Anchor, ConstraintAdjustment, Gravity, PopupPlacement, Positioner, Rect, Scene, SurfaceId,
};

/// Resolve `positioner` to the popup's geometry (origin relative to the parent's window geometry),
/// constrained to fit within a `target_w` × `target_h` area anchored at the parent's origin — the
/// output logical area in the common case. Convenience over [`place_popup_in`].
pub fn place_popup(positioner: &Positioner, target_w: i32, target_h: i32) -> Rect {
    place_popup_in(
        positioner,
        Rect::new(0, 0, target_w.max(1), target_h.max(1)),
    )
}

/// The scene-driven entry point: constrain `positioner` against the scene's primary output logical
/// area, mirroring `HlState::constrain_popup`.
pub fn constrain_popup(scene: &Scene, positioner: &Positioner) -> Rect {
    let (w, h) = scene.output_logical_size();
    place_popup(positioner, w, h)
}

/// Constrain a popup in its direct parent's coordinate space. Husklet presents each Wayland toplevel as
/// an independent native window, so the usable constraint rectangle is that owning toplevel's content
/// bounds, not the whole desktop output. For a submenu, translate those root bounds back through the
/// parent popup's accumulated offset. Before the root has committed a buffer, fall back to the advertised
/// output so the initial xdg-shell configure can still complete.
pub fn constrain_popup_for_parent(
    scene: &Scene,
    parent: SurfaceId,
    positioner: &Positioner,
) -> Rect {
    let Some(root) = scene.window_root(parent) else {
        return constrain_popup(scene, positioner);
    };
    let Some((w, h)) = scene.get(root).and_then(|surface| surface.logical_size()) else {
        return constrain_popup(scene, positioner);
    };
    let (parent_x, parent_y) = if scene.popup_parent(parent).is_some() {
        scene
            .popup_offset_to_toplevel(parent)
            .map(|(_, x, y, _)| (x, y))
            .unwrap_or((0, 0))
    } else {
        (0, 0)
    };
    place_popup_in(
        positioner,
        Rect::new(-parent_x, -parent_y, w.max(1), h.max(1)),
    )
}

/// The parent-relative placement a windowed presenter opens the popup at — its direct parent plus the
/// resolved geometry origin. Mirrors `HlState::popup_placement`. `None` if `surface` is not a popup.
pub fn popup_placement(scene: &Scene, surface: SurfaceId) -> Option<PopupPlacement> {
    let geometry = scene.popup_geometry(surface)?;
    let parent = scene.popup_parent(surface)?;
    Some(PopupPlacement {
        parent,
        x: geometry.x,
        y: geometry.y,
    })
}

/// The full placement solve within an explicit `target` rectangle.
pub fn place_popup_in(positioner: &Positioner, target: Rect) -> Rect {
    let mut geo = unconstrained(positioner);
    let c = positioner.constraint_adjustment;

    // ---- X axis: flip → slide → resize (the order xdg-shell mandates) ----
    if violates_x(&geo, &target) {
        if c.flip_x {
            let flipped = unconstrained(&mirror_x(positioner));
            if !violates_x(&flipped, &target) {
                geo.x = flipped.x;
            }
        }
        if violates_x(&geo, &target) && c.slide_x {
            geo.x = slide(geo.x, geo.w, target.x, target.right());
        }
        if violates_x(&geo, &target) && c.resize_x {
            let (x, w) = resize(geo.x, geo.w, target.x, target.right());
            geo.x = x;
            geo.w = w;
        }
    }

    // ---- Y axis ----
    if violates_y(&geo, &target) {
        if c.flip_y {
            let flipped = unconstrained(&mirror_y(positioner));
            if !violates_y(&flipped, &target) {
                geo.y = flipped.y;
            }
        }
        if violates_y(&geo, &target) && c.slide_y {
            geo.y = slide(geo.y, geo.h, target.y, target.bottom());
        }
        if violates_y(&geo, &target) && c.resize_y {
            let (y, h) = resize(geo.y, geo.h, target.y, target.bottom());
            geo.y = y;
            geo.h = h;
        }
    }

    geo
}

/// The unconstrained placement: anchor point on the anchor rect + gravity offset + the extra offset.
fn unconstrained(p: &Positioner) -> Rect {
    let (ax, ay) = anchor_point(p.anchor_rect, p.anchor);
    let (gx, gy) = gravity_offset(p.size.0, p.size.1, p.gravity);
    Rect::new(
        ax + gx + p.offset.0,
        ay + gy + p.offset.1,
        p.size.0,
        p.size.1,
    )
}

/// The point on the anchor rectangle the popup hangs off.
fn anchor_point(rect: Rect, anchor: Anchor) -> (i32, i32) {
    let x = rect.x
        + match anchor {
            Anchor::Left | Anchor::TopLeft | Anchor::BottomLeft => 0,
            Anchor::Right | Anchor::TopRight | Anchor::BottomRight => rect.w,
            _ => rect.w / 2,
        };
    let y = rect.y
        + match anchor {
            Anchor::Top | Anchor::TopLeft | Anchor::TopRight => 0,
            Anchor::Bottom | Anchor::BottomLeft | Anchor::BottomRight => rect.h,
            _ => rect.h / 2,
        };
    (x, y)
}

/// The popup's top-left relative to the anchor point, so the requested gravity direction grows AWAY
/// from the anchor (e.g. gravity Bottom-Right places the popup below-and-right of the anchor point).
fn gravity_offset(w: i32, h: i32, gravity: Gravity) -> (i32, i32) {
    let x = match gravity {
        Gravity::Left | Gravity::TopLeft | Gravity::BottomLeft => -w,
        Gravity::Right | Gravity::TopRight | Gravity::BottomRight => 0,
        _ => -w / 2,
    };
    let y = match gravity {
        Gravity::Top | Gravity::TopLeft | Gravity::TopRight => -h,
        Gravity::Bottom | Gravity::BottomLeft | Gravity::BottomRight => 0,
        _ => -h / 2,
    };
    (x, y)
}

fn violates_x(geo: &Rect, target: &Rect) -> bool {
    geo.x < target.x || geo.right() > target.right()
}
fn violates_y(geo: &Rect, target: &Rect) -> bool {
    geo.y < target.y || geo.bottom() > target.bottom()
}

/// Slide the popup along one axis to bring it into `[lo, hi)`: push in from the far edge first, then
/// clamp to the near edge (so an oversized popup ends up flush with the near edge).
fn slide(pos: i32, size: i32, lo: i32, hi: i32) -> i32 {
    let mut pos = pos;
    if pos + size > hi {
        pos = hi - size;
    }
    if pos < lo {
        pos = lo;
    }
    pos
}

/// Clamp the popup's extent to `[lo, hi)` on one axis, clipping the origin and size (size stays >= 1).
fn resize(pos: i32, size: i32, lo: i32, hi: i32) -> (i32, i32) {
    let mut pos = pos;
    let mut size = size;
    if pos < lo {
        size -= lo - pos;
        pos = lo;
    }
    if pos + size > hi {
        size = hi - pos;
    }
    (pos, size.max(1))
}

/// Mirror the positioner across the X axis (flip): swap left/right in BOTH anchor and gravity and
/// negate the X offset — the spec's `flip_x` transform.
fn mirror_x(p: &Positioner) -> Positioner {
    Positioner {
        anchor: mirror_anchor_x(p.anchor),
        gravity: mirror_gravity_x(p.gravity),
        offset: (-p.offset.0, p.offset.1),
        constraint_adjustment: ConstraintAdjustment::NONE,
        ..*p
    }
}

fn mirror_y(p: &Positioner) -> Positioner {
    Positioner {
        anchor: mirror_anchor_y(p.anchor),
        gravity: mirror_gravity_y(p.gravity),
        offset: (p.offset.0, -p.offset.1),
        constraint_adjustment: ConstraintAdjustment::NONE,
        ..*p
    }
}

fn mirror_anchor_x(a: Anchor) -> Anchor {
    match a {
        Anchor::Left => Anchor::Right,
        Anchor::Right => Anchor::Left,
        Anchor::TopLeft => Anchor::TopRight,
        Anchor::TopRight => Anchor::TopLeft,
        Anchor::BottomLeft => Anchor::BottomRight,
        Anchor::BottomRight => Anchor::BottomLeft,
        other => other,
    }
}
fn mirror_anchor_y(a: Anchor) -> Anchor {
    match a {
        Anchor::Top => Anchor::Bottom,
        Anchor::Bottom => Anchor::Top,
        Anchor::TopLeft => Anchor::BottomLeft,
        Anchor::BottomLeft => Anchor::TopLeft,
        Anchor::TopRight => Anchor::BottomRight,
        Anchor::BottomRight => Anchor::TopRight,
        other => other,
    }
}
fn mirror_gravity_x(g: Gravity) -> Gravity {
    match g {
        Gravity::Left => Gravity::Right,
        Gravity::Right => Gravity::Left,
        Gravity::TopLeft => Gravity::TopRight,
        Gravity::TopRight => Gravity::TopLeft,
        Gravity::BottomLeft => Gravity::BottomRight,
        Gravity::BottomRight => Gravity::BottomLeft,
        other => other,
    }
}
fn mirror_gravity_y(g: Gravity) -> Gravity {
    match g {
        Gravity::Top => Gravity::Bottom,
        Gravity::Bottom => Gravity::Top,
        Gravity::TopLeft => Gravity::BottomLeft,
        Gravity::BottomLeft => Gravity::TopLeft,
        Gravity::TopRight => Gravity::BottomRight,
        Gravity::BottomRight => Gravity::TopRight,
        other => other,
    }
}
